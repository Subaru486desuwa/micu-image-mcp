"""httpx.AsyncClient + Endpoint dataclass + retry/Retry-After 策略 + _call_with_retry。

锁包装由本模块负责（调用 locks._big_size_file_lock_async + Semaphore）。
"""
from __future__ import annotations

import asyncio
import json
import random
import time
from dataclasses import dataclass
from email.utils import parsedate_to_datetime

import httpx

from .config import (
    _TRUST_ENV,
    RETRYABLE_STATUS, RETRY_AFTER_STATUSES, BIG_SIZE_FAIL_FAST_STATUS,
    MAX_RETRY_AFTER_SECONDS,
    NETWORK_RETRY_DELAY_SECONDS,
    SMALL_RETRY_DELAYS_SECONDS,
    BIG_RETRY_DELAY_SECONDS,
    RETRY_JITTER_SECONDS,
    MAX_RESPONSE_BYTES,
)

# 响应体超过上限时返回的状态码（不在 RETRYABLE_STATUS / FALLBACK_STATUS 内，不会重试也不会降级）。
_RESPONSE_TOO_LARGE_STATUS = 413
from .extract import _error_detail
from .locks import _get_big_size_lock, _big_size_file_lock_async


@dataclass
class Endpoint:
    url: str
    json_body: dict | None = None
    multipart: dict | None = None  # {field_name: (filename, bytes, mime)}


# 模块级共享 httpx.AsyncClient：复用 keepalive 连接，减少每次请求的 TLS handshake / DNS。
# 5 并发场景（image_generate 1K N>1 / image_batch_edit）下每张省 100-300ms。
# 懒初始化（构造本身 sync，但首次 .post() 时才会绑定到当前事件循环）。
_HTTP_CLIENT: httpx.AsyncClient | None = None


def _get_http_client() -> httpx.AsyncClient:
    """返回模块级共享 client；首次调用时创建。

    client 本身 timeout=None（不设池级默认），实际超时由每个请求自带：
      - _call_endpoint / _call_endpoint_stream 默认 600s（经 _call_with_retry 的调用走默认，不另传）。
      - _save_image_url 单独传 120s。
    这样不同用途的请求可共用同一连接池。
    """
    global _HTTP_CLIENT
    if _HTTP_CLIENT is None:
        _HTTP_CLIENT = httpx.AsyncClient(
            timeout=None,
            trust_env=_TRUST_ENV,
            limits=httpx.Limits(max_keepalive_connections=20, max_connections=20),
        )
    return _HTTP_CLIENT


async def _read_body_capped(r: httpx.Response) -> tuple[bool, str]:
    """边读边累加，超过 MAX_RESPONSE_BYTES 立即中断。返回 (ok, text)；ok=False 表示超限。

    防止恶意/异常上游用超大响应体把 r.text 一次性灌进内存（httpx 默认无上限）。
    """
    total = 0
    chunks: list[bytes] = []
    async for chunk in r.aiter_bytes():
        total += len(chunk)
        if total > MAX_RESPONSE_BYTES:
            return False, ""
        chunks.append(chunk)
    return True, b"".join(chunks).decode("utf-8", errors="replace")


async def _call_endpoint(ep: Endpoint, key: str, timeout: float = 600.0) -> tuple[int, str, dict[str, str]]:
    """非 stream 调用。timeout 拉到 600s 给慢 origin 留余地（CF 120s 仍可能拦）。

    用 cx.stream 而非 cx.post：先看 Content-Length / 边读边截断，超 MAX_RESPONSE_BYTES 即中止，
    避免把超大响应体全量缓冲进内存。
    """
    headers = {"Authorization": f"Bearer {key}", "Accept": "application/json"}
    cx = _get_http_client()
    if ep.multipart is not None:
        files = []
        data = {}
        for k, v in ep.multipart.items():
            if isinstance(v, tuple) and len(v) == 3:
                files.append((k, v))
            else:
                data[k] = v
        ctx = cx.stream("POST", ep.url, headers=headers, data=data, files=files, timeout=timeout)
    else:
        headers["Content-Type"] = "application/json"
        ctx = cx.stream("POST", ep.url, headers=headers, content=json.dumps(ep.json_body), timeout=timeout)
    async with ctx as r:
        resp_headers = {k.lower(): v for k, v in r.headers.items()}
        cl = resp_headers.get("content-length")
        if cl and cl.isdigit() and int(cl) > MAX_RESPONSE_BYTES:
            return _RESPONSE_TOO_LARGE_STATUS, (
                f"响应 Content-Length={int(cl)/1024/1024:.1f}MB 超过 "
                f"{MAX_RESPONSE_BYTES/1024/1024:.0f}MB 上限"
            ), resp_headers
        ok, text = await _read_body_capped(r)
        if not ok:
            return _RESPONSE_TOO_LARGE_STATUS, (
                f"响应体超过 {MAX_RESPONSE_BYTES/1024/1024:.0f}MB 上限，已中断"
            ), resp_headers
        return r.status_code, text, resp_headers


async def _call_endpoint_stream(ep: Endpoint, key: str, timeout: float = 600.0) -> tuple[int, str, dict[str, str]]:
    """SSE stream 调用（chat/completions 专用）。把 delta.content 累加成完整 content，
    再包装成与非 stream 等价的 chat completion JSON 结构返回，让上层 _extract_image_payload 复用。

    关键：stream 模式下 CF 看到首字节就放行，不再撞 120s upstream timeout。
    """
    if ep.json_body is None or ep.multipart is not None:
        # 只对 JSON body 端点开 stream
        return await _call_endpoint(ep, key, timeout=timeout)
    body = dict(ep.json_body)
    body["stream"] = True
    headers = {
        "Authorization": f"Bearer {key}",
        "Accept": "text/event-stream",
        "Content-Type": "application/json",
    }
    full_content = ""
    line_count = 0
    content_bytes = 0
    truncated = False
    final_status = 0
    last_finish: str | None = None
    cx = _get_http_client()
    response_headers: dict[str, str] = {}
    try:
        async with cx.stream("POST", ep.url, headers=headers, content=json.dumps(body), timeout=timeout) as r:
            final_status = r.status_code
            response_headers = {k.lower(): v for k, v in r.headers.items()}
            if not (200 <= r.status_code < 300):
                ok, err_text = await _read_body_capped(r)
                if not ok:
                    err_text = f"错误响应体超过 {MAX_RESPONSE_BYTES/1024/1024:.0f}MB 上限，已中断"
                return r.status_code, err_text, response_headers
            async for raw_line in r.aiter_lines():
                if not raw_line:
                    continue
                line = raw_line.strip()
                line_count += 1
                # 累计内容超上限即停止累加（防死循环/超长流把 full_content 撑爆内存）。
                content_bytes += len(line)
                if content_bytes > MAX_RESPONSE_BYTES:
                    truncated = True
                    break
                if not line.startswith("data:"):
                    continue
                payload = line[5:].strip()
                if payload == "[DONE]":
                    break
                try:
                    chunk = json.loads(payload)
                except Exception:  # noqa: BLE001
                    continue
                # OpenAI chat stream: choices[0].delta.content / .tool_calls
                choices = chunk.get("choices") if isinstance(chunk, dict) else None
                if isinstance(choices, list) and choices:
                    c0 = choices[0] or {}
                    delta = c0.get("delta") or {}
                    if isinstance(delta.get("content"), str):
                        full_content += delta["content"]
                    if c0.get("finish_reason"):
                        last_finish = c0["finish_reason"]
                # /v1/responses-style stream: { type:"response.output_text.delta", delta:"..." }
                if isinstance(chunk, dict) and isinstance(chunk.get("delta"), str) and chunk.get("type", "").endswith(".delta"):
                    full_content += chunk["delta"]
    except httpx.HTTPError as e:
        return 0, f"stream error: {e}", {}
    # 包装成与非 stream chat completion 等价的 JSON
    fake_resp = {
        "choices": [{
            "message": {"role": "assistant", "content": full_content},
            "finish_reason": last_finish or ("length" if truncated else "stop"),
        }],
        "_stream_lines": line_count,
        "_truncated": truncated,
    }
    return final_status or 200, json.dumps(fake_resp, ensure_ascii=False), response_headers


def _parse_retry_after(headers: dict[str, str]) -> float | None:
    """Parse Retry-After as seconds, clamped to a practical upper bound."""
    value = (headers or {}).get("retry-after")
    if not value:
        return None
    value = value.strip()
    try:
        seconds = float(value)
    except ValueError:
        try:
            dt = parsedate_to_datetime(value)
        except (TypeError, ValueError, IndexError, OverflowError):
            return None
        seconds = dt.timestamp() - time.time()
    if seconds <= 0:
        return 0.0
    return min(seconds, MAX_RETRY_AFTER_SECONDS)


def _retry_delay(
    status: int,
    headers: dict[str, str],
    *,
    attempt_index: int,
    big_size_lock: bool,
) -> float | None:
    """Return delay before the next retry, or None if this status should not retry."""
    if status not in RETRYABLE_STATUS:
        return None
    if big_size_lock and status in BIG_SIZE_FAIL_FAST_STATUS:
        return None
    if big_size_lock and attempt_index >= 1:
        return None
    if not big_size_lock and attempt_index >= len(SMALL_RETRY_DELAYS_SECONDS):
        return None

    retry_after = _parse_retry_after(headers) if status in RETRY_AFTER_STATUSES else None
    if retry_after is not None:
        return retry_after

    if big_size_lock:
        return BIG_RETRY_DELAY_SECONDS

    return SMALL_RETRY_DELAYS_SECONDS[attempt_index] + random.uniform(0, RETRY_JITTER_SECONDS)


def _append_retry_note(
    notes_out: list[str] | None,
    *,
    status: int,
    delay: float,
    next_attempt: int,
    text: str,
) -> None:
    if notes_out is None:
        return
    detail = _error_detail(text)
    if detail:
        detail = f"；原因：{detail}"
    notes_out.append(f"HTTP {status} 可重试，等待 {delay:.1f}s 后第 {next_attempt} 次尝试{detail}")


async def _call_with_retry(
    ep: Endpoint,
    key: str,
    retry_pro: bool,
    stream: bool = False,
    big_size_lock: bool = False,
    notes_out: list[str] | None = None,
) -> tuple[int, str]:
    """pro 模型代理端瞬时限流多；stream=True 时 chat 走 SSE。

    所有调用包在 try/except 里：httpx 网络层异常（ReadError/ConnectError 等）转成 status=0 让重试逻辑接住。

    重试分两层：
      - 网络层异常（status==0）：连接根本没建立，无条件给 1 次免费重试（与 retry_pro 无关），
        2s 退避覆盖瞬时 DNS/TLS 抖动。
      - 上游 5xx / 429 / 408 / CF 5xx：仅在 retry_pro=True（pro 模型 或 size tier ∈ {2k, 4k}）
        时退避重试。优先尊重 Retry-After；否则 1K 用 4s / 8s + jitter 两次，≥2K 用 60s 单次。

    big_size_lock=True：整个调用（含网络层 + 上游重试）包在双层锁内：
      1) 进程内 Semaphore(1)：同 MCP 进程并发请求本地排队（零系统调用）。
      2) 跨进程 flock：多窗口 / 多 Claude Code 会话时所有 MCP 子进程共享一把
         系统级 advisory lock，整机任意时刻只有一个 ≥2K 请求打到 origin。
    """
    caller = _call_endpoint_stream if stream else _call_endpoint

    async def _attempt() -> tuple[int, str, dict[str, str]]:
        try:
            return await caller(ep, key)
        except Exception as e:  # noqa: BLE001
            return 0, f"{type(e).__name__}: {e}", {}

    async def _run() -> tuple[int, str]:
        status, text, headers = await _attempt()
        attempt_number = 1

        # 网络层瞬抖：无条件 1 次免费重试（独立于 retry_pro 预算）。
        if status == 0:
            _append_retry_note(
                notes_out,
                status=status,
                delay=NETWORK_RETRY_DELAY_SECONDS,
                next_attempt=attempt_number + 1,
                text=text,
            )
            await asyncio.sleep(NETWORK_RETRY_DELAY_SECONDS)
            status, text, headers = await _attempt()
            attempt_number += 1

        if not retry_pro:
            return status, text

        retry_attempt = 0
        while not (200 <= status < 300):
            delay = _retry_delay(
                status,
                headers,
                attempt_index=retry_attempt,
                big_size_lock=big_size_lock,
            )
            if delay is None:
                break
            _append_retry_note(
                notes_out,
                status=status,
                delay=delay,
                next_attempt=attempt_number + 1,
                text=text,
            )
            await asyncio.sleep(delay)
            retry_attempt += 1
            status, text, headers = await _attempt()
            attempt_number += 1

        return status, text

    if big_size_lock:
        async with _get_big_size_lock():                            # 进程内
            async with _big_size_file_lock_async(notes_out):        # 跨进程
                return await _run()
    return await _run()


__all__ = [
    "Endpoint",
    "_get_http_client",
    "_call_endpoint", "_call_endpoint_stream",
    "_parse_retry_after", "_retry_delay", "_append_retry_note",
    "_call_with_retry",
]
