"""米醋 gpt-image-2 MCP server (entry point).

历史上本文件是单文件 ~2500 行。重构后所有辅助函数 / 配置常量 / HTTP / 锁 / 保存 /
extract / routing / validation 均迁移到 micu_image_mcp/ 内部 package。

本文件只保留 3 件事：
  1. import 必要符号到 server module 命名空间（外部 tests / tools 用 `import server`）
  2. mcp = FastMCP("micu-image") + 5 个 @mcp.tool() 函数（带完整 docstring 供 LLM schema）
  3. main() entry

为了让 tests/_common.py 等历史代码继续通过 `import server; server._validate_size` 拿到
符号，所有原 `_underscore` 私有名都在此 re-export。
"""
from __future__ import annotations

import asyncio
import base64
import time
from typing import Any

from mcp.server.fastmcp import FastMCP

# ---------- 从 internal package re-export (语义零漂移) ----------
from micu_image_mcp.config import (
    _LOCK_BACKEND, _FILE_LOCK_AVAILABLE,
    DEFAULT_BASEURL, API_KEY, DEFAULT_MODEL,
    API_RESPONSE_FORMAT, TRUSTED_DOWNLOAD_HOSTS, ALLOW_FAKE_IP_DOWNLOAD,
    RESPONSE_FORMATS_TO_TRY,
    GROK_BASEURL, GROK_API_KEY, XAI_MODEL, GROK_SIZE_MODE,
    _TRUST_ENV, _SAVE_ROOT, DEFAULT_SAVE_DIR,
    STANDARD_MODEL, QUALITY_MODEL, PRO_MODEL, NONPRO_MODEL, SUPPORTED_IMAGE_MODELS,
    VALID_IMAGE_QUALITIES,
    GROK_MODEL_ALIASES, GROK_AVAILABLE_MODELS,
    GROK_ASPECT_RATIO_CHOICES, GROK_SIZE_MODES,
    HIGH_RES_EDGE, EDITS_MAX_EDGE,
    VALID_SIZES_1K, VALID_SIZES_2K, VALID_SIZES_4K,
    MAX_N, MIN_SIZE_EDGE, MAX_SIZE_EDGE, SIZE_ALIGNMENT,
    MIN_IMAGE_PIXELS, MAX_IMAGE_PIXELS, MAX_IMAGE_ASPECT_RATIO,
    MAX_INPUT_FILE_BYTES, MAX_TOTAL_INPUT_BYTES, MAX_RESPONSE_BYTES,
    _SAFE_BASENAME_RE,
    RETRYABLE_STATUS, FALLBACK_STATUS, RETRY_AFTER_STATUSES, BIG_SIZE_FAIL_FAST_STATUS,
    MAX_RETRY_AFTER_SECONDS, NETWORK_RETRY_DELAY_SECONDS,
    SMALL_RETRY_DELAYS_SECONDS, BIG_RETRY_DELAY_SECONDS, RETRY_JITTER_SECONDS,
)
from micu_image_mcp.sizes import (
    _parse_size, _max_edge, _size_tier, _grok_size_mode,
    _validate_size, _validate_grok_size, _validate_n, _validate_quality,
    _round_to_alignment, _parse_actual,
)
from micu_image_mcp.routing import (
    _is_grok_model, _model_error, _is_quality_model, _reject_4k_with_reference,
    _resolve_model, _bypass_edits,
    _size_note, _grok_aspect_ratio, _grok_resolution,
    _infer_size_from_prompt,
)
from micu_image_mcp.io_safety import (
    _safe_basename, _resolve_save_dir,
    _validate_image_bytes, _validate_image_path,
    _png_color_type, _validate_mask_against_image,
    _default_basename, _detect_actual_size,
)
from micu_image_mcp.locks import (
    _BIG_SIZE_FILE_LOCK_PATH,
    _get_big_size_lock,
    _acquire_big_size_file_lock_blocking,
    _release_big_size_file_lock,
    _big_size_file_lock_async,
)
from micu_image_mcp.extract import (
    _parse_response, _error_detail,
    _extract_image_payload, _extract_image_payloads,
)
from micu_image_mcp.http_client import (
    Endpoint,
    _get_http_client,
    _call_endpoint, _call_endpoint_stream,
    _parse_retry_after, _retry_delay, _append_retry_note,
    _call_with_retry,
)
from micu_image_mcp.save import (
    ImageSaveError,
    _normalized_image_bytes_sync, _maybe_normalize_image_bytes,
    _save_validated_bytes, _save_image_b64, _save_image_url,
    _save_extracted_payload, _save_first_payload_from_response,
)


# ---------- MCP 主体 ----------

mcp = FastMCP("micu-image")


def _get_key(override: str | None) -> str:
    key = (override or "").strip() or API_KEY
    if not key:
        raise RuntimeError(
            "未配置 API key。请设置 MICU_API_KEY 环境变量，或在调用时传 api_key 参数。"
        )
    return key


def _get_grok_key(override: str | None) -> str:
    key = (override or "").strip() or GROK_API_KEY
    if not key:
        raise RuntimeError(
            "未配置米醋 Grok API key。请设置 MICU_GROK_API_KEY 环境变量，或在调用时传 api_key 参数。"
        )
    return key


def _get_baseurl() -> str:
    """baseurl 锁在启动时的 env，运行期 tool 不接受覆盖（防 API key 外泄到攻击者 host）。"""
    return DEFAULT_BASEURL


def _get_grok_baseurl() -> str:
    return GROK_BASEURL


async def _call_and_save_with_format_fallback(
    *,
    build_ep: "Callable[[str], Endpoint]",
    key: str,
    retry_pro: bool,
    stream: bool,
    big_size_lock: bool,
    notes: list[str],
    out_dir: Path,
    stem: str,
    requested_size: str,
    normalize_size: str | None = None,
    normalize_mode: str | None = None,
    normalize_label: str = "图片",
) -> tuple[dict[str, Any] | None, str | None]:
    """按 RESPONSE_FORMATS_TO_TRY 顺序调 API 并落盘；默认先 url，保存失败再 b64_json。

    返回 (saved_info, error)。HTTP 错误不会切换 API 路由。
    """
    last_err: str | None = None
    for fmt_i, fmt in enumerate(RESPONSE_FORMATS_TO_TRY):
        if fmt_i > 0:
            notes.append(f"URL 落盘失败（{last_err}）→ 重试 API response_format={fmt}")
        ep = build_ep(fmt)
        status, text = await _call_with_retry(
            ep, key, retry_pro=retry_pro, stream=stream,
            big_size_lock=big_size_lock, notes_out=notes,
        )
        if not (200 <= status < 300):
            last_err = f"HTTP {status}: {_error_detail(text)}"
            continue
        saved_info, save_err = await _save_first_payload_from_response(
            text,
            out_dir,
            stem,
            notes,
            requested_size,
            normalize_size=normalize_size,
            normalize_mode=normalize_mode,
            normalize_label=normalize_label,
        )
        if saved_info:
            if fmt_i > 0:
                notes.append(f"已通过 response_format={fmt} 成功落盘")
            return saved_info, None
        last_err = save_err or "保存失败"
    return None, last_err


@mcp.tool()
async def image_generate(
    prompt: str,
    size: str | None = None,
    n: int = 1,
    model: str | None = None,
    quality: str | None = None,
    save_dir: str | None = None,
    basename: str | None = None,
    api_key: str | None = None,
) -> dict[str, Any]:
    """文本生成图像（text-to-image）。米醋代理 + gpt-image-2 系列。

    [WHAT] 把一段文字 prompt 渲染成 1 张或 N 张图像，落盘到本地。

    [WHEN TO USE]
      - 用户要"画 / 生成 / 创建一张图"且没有提供任何参考图 → 用此 tool。
      - 如果用户提供了 1 张参考图要"修改 / 编辑 / 替换某部分" → 改用 image_edit。
      - 如果用户提供了多张参考图要"按它们的风格画一张新的" → 用 image_multi_reference。
      - 如果不知道怎么选 size：先调 server_info() 看 recommended_sizes。

    [SIZE 选取建议]
      - 默认 None：MCP 自动从 prompt 关键字推断（4K/UHD → 3840x2160；1080p/2K → 2048x1152；
        正方形/logo/头像 → 1024x1024；竖屏/9:16 → 1024x1536；横屏/16:9 → 1536x1024 等）。
        推断不出来 fallback 1024x1024。
      - 强烈推荐：如果你（LLM）已经从用户消息读出确定的 size 偏好，**直接显式传 size**，比关键字推断准。
      - 用户提到"高清/4K/海报/壁纸" → "3840x2160"（横）或 "2160x3840"（竖），自动用 gpt-image-2-openai。
      - 用户提到"FullHD/1080p/横屏视频封面" → "2048x1152"（横）或 "1152x2048"（竖）；
        这两个尺寸均满足当前 16 像素对齐和总像素约束。
      - W 与 H 必须都是 16 的倍数；最长边 ≤3840；长宽比 ≤3:1；总像素 655,360-8,294,400。
      - **2K/4K 自动走高质量线路**：≥2K 自动切 gpt-image-2-openai。2026-08-14 实测其 1536×1024、
        2048×1152、3840×2160 均按请求像素返回；gpt-image-2 的自定义宽高可能被后端重映射。

    [PROMPT 写法建议]
      - 中英文混合可。gpt-image-2 文本渲染近完美，可大段嵌字（中英标点都行）。
      - 越具体越好：风格 / 视角 / 光线 / 主体 / 细节程度。

    Args:
        prompt: 图像描述。1-2000 字符。例："A minimalist sushi mascot logo, soft pastel palette".
        size: "WxH" 字符串或 None。**留 None 让 MCP 从 prompt 推**（弱 LLM 兜底用）；
              强 LLM 已知偏好时**直接显式传**更准。W 和 H 都必须是 16 的倍数。常用：
              "1024x1024" "1280x720" "1024x1536" "1536x1024" "720x1280"        ← 1K 档
              "2048x2048" "2048x1152" "1152x2048"                              ← 2K 档（自动 gpt-image-2-openai）
              "3840x2160" "2160x3840"                                          ← 4K 档（自动 gpt-image-2-openai）
              默认 None（推断后兜底 1024x1024）。
        n: 张数 1-10。1K 时 N>1 自动 5 并发；≥2K 强制 N=1（代理限流）。默认 1。
        model: 显式指定模型。留空时按 size 自动选（max edge ≥1600 用 gpt-image-2-openai，否则 gpt-image-2）。
              可选值："gpt-image-2"（标准线路）/ "gpt-image-2-openai"（高质量线路）。
        quality: 可选质量参数："auto" / "low" / "medium" / "high"；留空则使用后端默认值。
        save_dir: 输出目录。**必须在安全根目录 MICU_SAVE_DIR_ROOT 之下**（默认 ~/Pictures/micu-out）；
                  传 root 之外路径会被拒。留空使用默认。
        basename: 文件名前缀（不带扩展名），仅允许 [A-Za-z0-9_\\-.]。
                  含 / .. 或路径分量会被拒。默认 "gen_<ns_timestamp>"。
        api_key: 覆盖 MICU_API_KEY 环境变量。一般留空。
                 注意：base_url 已锁在启动时 env，运行期不接受 tool 参数（防 key 外泄到攻击者 host）。

    Returns: dict 含以下字段：
        ok (bool): 至少有 1 张成功才为 True。
        model (str): 实际用的模型 id。
        size (str): 请求的 size。
        requested_n (int): 实际生成的张数。
        saved (list[dict]): 每张成功的图。每项含 path（绝对路径）/ size_bytes / actual_size（PNG header 读出的真实像素）/ actual_megapixels。
        errors (list[str]): 失败请求的错误描述。
        notes (list[str]): 路由 / 自动决策 / 实测尺寸偏差的说明。

    Examples:
        # 最简：默认 1024x1024 单张
        image_generate(prompt="a red apple on white")

        # 4K 壁纸
        image_generate(prompt="cyberpunk Tokyo at night", size="3840x2160")

        # 一次出 4 张候选（1K 自动并发）
        image_generate(prompt="cute sticker of a cat", size="1024x1024", n=4)

    Common errors and what to do:
        "size W/H 必须是 16 的倍数" → 客户端入口拒；例如 1920×1080 应改为 1920×1088 或推荐的 2048×1152。
        "HTTP 524: timeout" → 已自动重试 3 次仍失败，建议改小 size 或稍后再试。
        "未配置 API key" → 设置 MICU_API_KEY 环境变量或传 api_key 参数。
    """
    # === 入口校验（一条条 return 错误，不再静默 ok=False）===
    if not isinstance(prompt, str) or not prompt.strip():
        return {"ok": False, "error": "prompt 不能为空", "errors": ["prompt 不能为空"]}
    err_n = _validate_n(n)
    if err_n:
        return {"ok": False, "error": err_n, "errors": [err_n]}
    if model_err := _model_error(DEFAULT_MODEL if model is None else model):
        return {"ok": False, "error": model_err, "errors": [model_err]}
    safe_stem = _safe_basename(basename) if basename is not None else None
    if basename is not None and safe_stem is None:
        msg = f"basename {basename!r} 含非法字符或路径分量；仅允许 [A-Za-z0-9_-.]，禁含 / 与 .."
        return {"ok": False, "error": msg, "errors": [msg]}
    cleaned_quality, quality_err = _validate_quality(quality)
    if quality_err:
        return {"ok": False, "error": quality_err, "errors": [quality_err]}
    out_dir, dir_err = _resolve_save_dir(save_dir)
    if dir_err:
        return {"ok": False, "error": dir_err, "errors": [dir_err]}

    # size=None 时从 prompt 关键字推断；推断不出用 1024x1024 默认。
    inferred_note: str | None = None
    if size is None:
        guess = _infer_size_from_prompt(prompt)
        if guess:
            size, reason = guess
            inferred_note = f"size=None → 推断 {size}（{reason}）"
        else:
            size = "1024x1024"
            inferred_note = "size=None → 无关键字命中，用默认 1024x1024"
    use_grok = _is_grok_model(model or DEFAULT_MODEL)
    if use_grok:
        cleaned_size, size_err = _validate_grok_size(size, allow_none=False)
    else:
        cleaned_size, size_err = _validate_size(size, allow_none=False)
    if size_err:
        return {"ok": False, "error": size_err, "errors": [size_err]}
    size = cleaned_size  # type: ignore[assignment]

    eff_model, notes = _resolve_model(model, size)
    if inferred_note:
        notes.insert(0, inferred_note)
    use_grok = _is_grok_model(eff_model)
    if use_grok:
        key = _get_grok_key(api_key)
        baseurl = _get_grok_baseurl()
        if n > 1:
            notes.append(f"Grok 路径保留请求的 n={n}")
    else:
        key = _get_key(api_key)
        baseurl = _get_baseurl()
    tier = _size_tier(size)
    if not use_grok and tier in ("2k", "4k") and n > 1:
        notes.append(f"{tier.upper()} 强制 N=1，已忽略请求的 n={n}")
        n = 1
    is_quality = _is_quality_model(eff_model)
    stem = safe_stem or _default_basename("gen")

    if use_grok:
        if tier == "4k":
            notes.append(f"Grok 路径仅支持 1K / 2K，已将 size={size} 映射到 resolution=2k")
        size_mode = _grok_size_mode()
        if size_mode == "backend":
            notes.append("Grok 尺寸模式 MICU_GROK_SIZE_MODE=backend：保留后端原始像素。")
        else:
            notes.append(
                f"Grok 后端只按 resolution/aspect_ratio 生成；保存前会按 MICU_GROK_SIZE_MODE={size_mode} "
                f"本地归一化到请求 size={size}。"
            )
        aspect_ratio = _grok_aspect_ratio(size)
        last_grok_err: str | None = None
        for fmt_i, fmt in enumerate(RESPONSE_FORMATS_TO_TRY):
            if fmt_i > 0:
                notes.append(f"URL 落盘失败（{last_grok_err}）→ 重试 API response_format={fmt}")
            grok_ep = Endpoint(
                url=f"{baseurl}/v1/images/generations",
                json_body={
                    "model": eff_model,
                    "prompt": prompt,
                    "n": n,
                    "resolution": _grok_resolution(size),
                    "aspect_ratio": aspect_ratio,
                    "response_format": fmt,
                    **({"quality": cleaned_quality} if cleaned_quality is not None else {}),
                },
            )
            status, text = await _call_with_retry(
                grok_ep, key, retry_pro=True, stream=False,
                big_size_lock=False, notes_out=notes,
            )
            if not (200 <= status < 300):
                last_grok_err = f"HTTP {status}: {_error_detail(text)}"
                continue

            resp = _parse_response(text)
            payloads = _extract_image_payloads(resp)
            saved: list[dict[str, Any]] = []
            errors: list[str] = []
            for idx, (b64, url) in enumerate(payloads[:n]):
                try:
                    p, actual, size_bytes = await _save_extracted_payload(
                        b64,
                        url,
                        out_dir,
                        f"{stem}_{idx + 1}",
                        notes,
                        normalize_size=size,
                        normalize_mode=size_mode,
                        normalize_label="Grok 文生图",
                    )
                except Exception as e:  # noqa: BLE001
                    errors.append(f"#{idx + 1} 保存失败: {e}")
                    continue
                entry: dict[str, Any] = {
                    "index": idx + 1,
                    "path": str(p.resolve()),
                    "size_bytes": size_bytes,
                }
                if actual:
                    entry["actual_size"] = f"{actual[0]}x{actual[1]}"
                    entry["actual_megapixels"] = round(actual[0] * actual[1] / 1_000_000, 2)
                    sn = _size_note(size, actual)
                    if sn and sn not in notes:
                        notes.append(sn)
                saved.append(entry)
            if saved:
                if fmt_i > 0:
                    notes.append(f"已通过 response_format={fmt} 成功落盘")
                if len(payloads) < n:
                    notes.append(f"Grok 仅返回 {len(payloads)} 张（< 请求 n={n}）；以 saved 实际张数为准。")
                return {
                    "ok": True,
                    "model": eff_model,
                    "size": size,
                    "requested_n": n,
                    "used_fallback": False,
                    "saved": saved,
                    "errors": errors,
                    "notes": notes,
                }
            last_grok_err = "; ".join(errors) if errors else "保存失败"
        return {
            "ok": False,
            "error": last_grok_err,
            "errors": [last_grok_err or "保存失败"],
            "model": eff_model,
            "size": size,
            "requested_n": n,
            "notes": notes,
        }

    # 当前实测：高质量线路的常用 1K/2K/4K 尺寸精确输出；标准线路的部分自定义尺寸会被重映射。
    # 当前 GPT Image 2 模型只走 Images API；不再把图像模型错误发送到 chat/completions。
    saved: list[dict] = []
    errors: list[str] = []
    # 客户端循环 N 次单图请求（米醋 image_generation tool 不接受 n 字段）。
    # ≥2K 一律给 retry_pro=True 让 524/超时被重试（内部参数名为历史兼容名）
    aggressive_retry = is_quality or tier in ("2k", "4k")
    # 并发策略（与 image_batch_edit 对齐）：
    #   - 1K + 标准线路 + n>1 → 5 并发（HTML 网页同款）
    #   - 1K + 高质量线路 / ≥2K → 串行（高质量线路瞬时限流多；≥2K 已强制 N=1）
    can_concurrent = n > 1 and tier in ("small", "1k") and not is_quality
    concurrency = 5 if can_concurrent else 1
    big_size_lock = tier in ("2k", "4k")
    used_fallback = False  # 兼容既有返回结构；当前模型不会跨到 chat/completions。

    async def _do_one(idx: int) -> tuple[int, dict | None, str | None]:
        last_err: str | None = None
        for fmt_i, fmt in enumerate(RESPONSE_FORMATS_TO_TRY):
            if fmt_i > 0:
                notes.append(f"URL 落盘失败（{last_err}）→ 重试 API response_format={fmt}")
            ep = Endpoint(
                url=f"{baseurl}/v1/images/generations",
                json_body={
                    "model": eff_model,
                    "prompt": prompt,
                    "n": 1,
                    "size": size,
                    "response_format": fmt,
                    **({"quality": cleaned_quality} if cleaned_quality is not None else {}),
                },
            )
            status, text = await _call_with_retry(
                ep, key, retry_pro=aggressive_retry, stream=False,
                big_size_lock=big_size_lock, notes_out=notes,
            )
            if not (200 <= status < 300):
                last_err = f"#{idx + 1} HTTP {status}: {_error_detail(text)}"
                continue
            resp = _parse_response(text)
            b64, url = _extract_image_payload(resp)
            try:
                p, actual, size_bytes = await _save_extracted_payload(
                    b64, url, out_dir, f"{stem}_{idx + 1}", notes,
                )
            except Exception as e:  # noqa: BLE001
                last_err = f"#{idx + 1} 保存失败: {e}"
                continue
            if fmt_i > 0:
                notes.append(f"已通过 response_format={fmt} 成功落盘")
            entry: dict[str, Any] = {
                "index": idx + 1,
                "path": str(p.resolve()),
                "size_bytes": size_bytes,
            }
            if actual:
                entry["actual_size"] = f"{actual[0]}x{actual[1]}"
                entry["actual_megapixels"] = round(actual[0] * actual[1] / 1_000_000, 2)
            return idx, entry, None
        return idx, None, last_err or f"#{idx + 1} 保存失败"

    if concurrency > 1:
        sem = asyncio.Semaphore(concurrency)

        async def _wrap(idx: int):
            async with sem:
                return await _do_one(idx)

        results = await asyncio.gather(*(_wrap(i) for i in range(n)))
        notes.append(f"1K + 标准线路 + N={n} 已 {concurrency} 并发")
    else:
        results = []
        for i in range(n):
            results.append(await _do_one(i))

    results.sort(key=lambda r: r[0])
    for _idx, entry, err in results:
        if entry:
            saved.append(entry)
            sn = _size_note(size, _parse_actual(entry.get("actual_size")))
            if sn and sn not in notes:
                notes.append(sn)
        if err:
            errors.append(err)

    return {
        "ok": bool(saved),
        "model": eff_model,
        "size": size,
        "requested_n": n,
        "used_fallback": used_fallback,
        "size_honored": bool(saved) and all(
            _parse_actual(entry.get("actual_size")) == _parse_size(size) for entry in saved
        ),
        "saved": saved,
        "errors": errors,
        "notes": notes,
    }


@mcp.tool()
async def image_edit(
    prompt: str,
    image_path: str,
    mask_path: str | None = None,
    size: str = "1024x1024",
    model: str | None = None,
    save_dir: str | None = None,
    basename: str | None = None,
    api_key: str | None = None,
) -> dict[str, Any]:
    """图像编辑（image-to-image，单张输入）。1K/2K 可用；4K 参考图编辑仍禁用。

    [WHAT] 接受 1 张本地图片 + 修改指令，输出修改后的图。

    [WHEN TO USE]
      - 用户提供 1 张图（路径或刚刚生成的图）且要"改 / 替换 / 加 / 去掉某部分" → 用此 tool。
      - 如果用户没提供图想从零生成 → 改用 image_generate。
      - 如果用户提供了多张图想"批量改"（每张做同样操作）→ 改用 image_batch_edit。
      - 如果用户用多张图作风格参考想画一张新的 → 用 image_multi_reference。

    [尺寸能力]（2026-08-14 实测）
      - 1K：gpt-image-2 与 gpt-image-2-openai 的 1024×1024 edits 均成功并精确返回。
      - 2K：自动切 gpt-image-2-openai；2048×1152 edits 成功并精确返回。
      - 4K 仍禁用（origin 处理 4K + 参考图稳定 > 120s 撞 CF 524）。要真 4K 走两步法：
        先 image_edit 出 1K/2K → image_generate(size="3840x..."，描述同场景) 升分辨率。

    [4K 已禁用]
      入口直接拒 4K size，不发请求（origin 处理 4K + 参考图稳定 > 120s 撞 CF 524）。
      请改 2K（"2048x1152" / "1152x2048" / "2048x2048"），2K 带参考图为 best-effort 真 2K。

    [路由实现]（实测确定）
      - 所有尺寸统一走 /v1/images/edits multipart（米醋唯一真正消费输入图的端点）。
        Images API 返回错误时直接报错，不把图像模型转发到不兼容的 /v1/chat/completions。
      - mask 现已在所有尺寸支持（不再区分 1K/2K）。

    [MASK 工作原理]
      - mask_path 指向一张 PNG，尺寸应与 image_path 一致。
      - mask 中 **alpha=0（透明）** 的像素 = 要修改的区域。
      - alpha=255（不透明）的像素 = 要保持原样。
      - 不传 mask 则模型自由决定改哪里。

    Args:
        prompt: 修改指令，越具体越好。例："change the background to deep navy with stars, keep the subject pixel-identical".
        image_path: 输入图的绝对或相对路径。PNG / JPG / WebP 都支持。
        mask_path: 可选 alpha mask PNG 路径，透明区即编辑区。所有尺寸均生效。
        size: 输出 size。W/H 必须是 16 的倍数；总像素和长宽比规则见 server_info。
              "1024x1024" "1280x720" "1024x1536" "1536x1024" "720x1280"  ← 1K 档
              "2048x2048" "2048x1152" "1152x2048"                        ← 2K 档（自动高质量线路）
              "3840x2160" / "2160x3840"  ← 4K 已禁用（撞 CF 524 物理上限），传入直接拒
              默认 "1024x1024"。
        model: "gpt-image-2"（默认）/ "gpt-image-2-openai"（高质量线路，≥2K 自动切）。
        save_dir: 输出目录（必须在安全根目录之下）。默认 ~/Pictures/micu-out 或 MICU_SAVE_DIR。
        basename: 文件名前缀（仅 [A-Za-z0-9_-.]）。默认 "edit_<ns_ts>"。
        api_key: 覆盖 MICU_API_KEY；base_url 已锁在启动期 env，运行期不接受。

    Returns: dict 含：
        ok (bool): 是否成功。
        model (str): 实际用的模型。
        size (str): 请求 size。
        used_fallback (bool): 为兼容既有返回结构保留；当前 Image2 模型固定为 False。
        saved (dict): { path, size_bytes, actual_size, actual_megapixels }。
        notes (list[str]): 决策与提示。

    Examples:
        # 换背景
        image_edit(prompt="replace background with a sunset beach", image_path="/p/portrait.jpg")

        # 局部修改（mask 生效）
        image_edit(prompt="change hair color to silver", image_path="/p/x.png", mask_path="/p/x_mask.png")

        # 升细节（2K 自动使用高质量线路）
        image_edit(prompt="enhance to cinematic detail, preserve composition", image_path="/p/draft.png", size="2048x2048")

    Common errors:
        "image_path 不存在" → 检查路径，建议用绝对路径。
        "size=3840x2160 (4K) 在 image_edit 已禁用" → 4K image_edit 物理撞 CF 524 上限，请改 2K 或两步法。
        "HTTP 524" → 2K 单图正常 ~50s，撞了说明 origin 那阵特别忙；自动重试仍失败请稍后再试。
    """
    # === 入口校验 ===
    if not isinstance(prompt, str) or not prompt.strip():
        return {"ok": False, "error": "prompt 不能为空", "errors": ["prompt 不能为空"]}
    if model_err := _model_error(DEFAULT_MODEL if model is None else model):
        return {"ok": False, "error": model_err, "errors": [model_err]}
    use_grok_request = _is_grok_model(model or DEFAULT_MODEL)
    if use_grok_request:
        cleaned_size, size_err = _validate_grok_size(size, allow_none=False)
    else:
        cleaned_size, size_err = _validate_size(size, allow_none=False)
    if size_err:
        return {"ok": False, "error": size_err, "errors": [size_err]}
    size = cleaned_size  # type: ignore[assignment]
    if not use_grok_request and (rej := _reject_4k_with_reference(size, "image_edit")):
        return {"ok": False, "error": rej, "errors": [rej]}
    safe_stem = _safe_basename(basename) if basename is not None else None
    if basename is not None and safe_stem is None:
        msg = f"basename {basename!r} 含非法字符或路径分量"
        return {"ok": False, "error": msg, "errors": [msg]}
    out_dir, dir_err = _resolve_save_dir(save_dir)
    if dir_err:
        return {"ok": False, "error": dir_err, "errors": [dir_err]}

    # 输入图：大小 + magic 校验
    img_p, img_bytes, img_mime, img_err = _validate_image_path(image_path, "image_path")
    if img_err:
        return {"ok": False, "error": img_err, "errors": [img_err]}

    eff_model, notes = _resolve_model(model, size)
    if _is_grok_model(eff_model):
        key = _get_grok_key(api_key)
        baseurl = _get_grok_baseurl()
        stem = safe_stem or _default_basename("edit")
        size_mode = _grok_size_mode()
        notes.append(
            "Grok 图生图走 /v1/images/generations + reference_image；"
            "size 只映射为 resolution/aspect_ratio，实际像素由 Grok/MICU 返回决定。"
        )
        if size_mode == "backend":
            notes.append("Grok 尺寸模式 MICU_GROK_SIZE_MODE=backend：保留后端原始像素。")
        else:
            notes.append(
                f"Grok 保存前会按 MICU_GROK_SIZE_MODE={size_mode} 本地归一化到请求 size={size}。"
            )
        if mask_path:
            notes.append("Grok reference_image 路径当前不支持 mask，已忽略 mask_path。")
        img_b64 = await asyncio.to_thread(lambda: base64.b64encode(img_bytes).decode())
        img_data_url = f"data:{img_mime};base64,{img_b64}"
        saved_info, save_err = await _call_and_save_with_format_fallback(
            build_ep=lambda fmt: Endpoint(
                url=f"{baseurl}/v1/images/generations",
                json_body={
                    "model": eff_model,
                    "prompt": prompt,
                    "n": 1,
                    "resolution": _grok_resolution(size),
                    "aspect_ratio": _grok_aspect_ratio(size),
                    "reference_image": img_data_url,
                    "response_format": fmt,
                },
            ),
            key=key,
            retry_pro=True,
            stream=False,
            big_size_lock=False,
            notes=notes,
            out_dir=out_dir,
            stem=stem,
            requested_size=size,
            normalize_size=size,
            normalize_mode=size_mode,
            normalize_label="Grok 图生图",
        )
        if save_err:
            return {"ok": False, "model": eff_model, "size": size, "error": save_err, "errors": [save_err], "notes": notes}
        return {
            "ok": True,
            "model": eff_model,
            "size": size,
            "used_fallback": False,
            "saved": saved_info,
            "notes": notes,
        }
    key = _get_key(api_key)
    baseurl = _get_baseurl()

    mask_bytes: bytes | None = None
    if mask_path:
        _mp, mask_raw, _mm, mask_err = _validate_image_path(mask_path, "mask_path")
        if mask_err:
            return {"ok": False, "error": mask_err, "errors": [mask_err]}
        # 强校验：PNG + 与原图同尺寸 + 含 alpha 通道
        img_size = _detect_actual_size(img_bytes)
        if img_size is None:
            msg = "原图无法解析尺寸，mask 校验跳过；请检查 image_path 是否完整"
            return {"ok": False, "error": msg, "errors": [msg]}
        mask_err2 = _validate_mask_against_image(mask_raw, img_size)
        if mask_err2:
            return {"ok": False, "error": mask_err2, "errors": [mask_err2]}
        mask_bytes = mask_raw

    stem = safe_stem or _default_basename("edit")
    is_quality = _is_quality_model(eff_model)

    aggressive_retry = is_quality or _size_tier(size) in ("2k", "4k")
    big_size_lock = _size_tier(size) in ("2k", "4k")
    last_err: str | None = None
    saved_info: dict[str, Any] | None = None

    for fmt_i, fmt in enumerate(RESPONSE_FORMATS_TO_TRY):
        if fmt_i > 0:
            notes.append(f"URL 落盘失败（{last_err}）→ 重试 API response_format={fmt}")
        edits_form: dict[str, Any] = {
            "model": eff_model,
            "prompt": prompt,
            "size": size,
            "response_format": fmt,
            "image": (img_p.name, img_bytes, img_mime),
        }
        if mask_bytes:
            edits_form["mask"] = ("mask.png", mask_bytes, "image/png")
        edits_ep = Endpoint(url=f"{baseurl}/v1/images/edits", multipart=edits_form)
        status, text = await _call_with_retry(
            edits_ep, key, retry_pro=aggressive_retry, stream=False,
            big_size_lock=big_size_lock, notes_out=notes,
        )
        if not (200 <= status < 300):
            last_err = f"HTTP {status}: {_error_detail(text)}"
            continue
        saved_info, save_err = await _save_first_payload_from_response(
            text, out_dir, stem, notes, size,
        )
        if saved_info:
            if fmt_i > 0:
                notes.append(f"已通过 response_format={fmt} 成功落盘")
            break
        last_err = save_err or "保存失败"

    if not saved_info:
        return {
            "ok": False,
            "model": eff_model,
            "size": size,
            "error": last_err or "保存失败",
            "notes": notes,
        }

    return {
        "ok": True,
        "model": eff_model,
        "size": size,
        "used_fallback": False,
        "saved": saved_info,
        "notes": notes,
    }


@mcp.tool()
async def image_batch_edit(
    prompt: str,
    image_paths: list[str],
    size: str = "1024x1024",
    model: str | None = None,
    save_dir: str | None = None,
    api_key: str | None = None,
) -> dict[str, Any]:
    """批量图像编辑：N 张输入图 → N 张输出图，每张独立应用同一指令。

    [WHAT] 对 image_paths 里的每一张图分别调用 image_edit，统一 prompt 与 size，结果合并返回。

    [WHEN TO USE]
      - 用户提供多张图且每张要做"同样的修改"（如批量加水印 / 统一换底 / 统一调色）→ 用此 tool。
      - 如果是"用多张图作风格参考画 1 张新图" → 这不是此 tool，暂未实现。
      - 如果只有 1 张图 → 用 image_edit。

    [并发策略]
      - gpt-image-2：5 并发（HTML 网页同款）。
      - gpt-image-2-openai：串行 + 1.5s gap（高质量线路并发更容易被限流）。
      - 任意一张失败不影响其他张；返回 results 里逐张标 ok/error。

    [LIMITS]
      - 同 image_edit：size 仅 1K 档（≤1536 边长），≥2K 拒绝。
      - image_paths 长度建议 2-20 张；过多请分批调用避免超时。

    Args:
        prompt: 应用到每张图的修改指令。例："add a subtle watermark in bottom-right".
        image_paths: 输入图路径列表（绝对或相对）。
        size: 输出 size，仅 1K 档。默认 "1024x1024"。
        model: "gpt-image-2" / "gpt-image-2-openai"。留空按 size 自动选。
        save_dir: 输出目录（必须在安全根目录之下）。文件名 batch_<ts>_<idx>.png。
        api_key: 覆盖 MICU_API_KEY；base_url 已锁在启动期 env，运行期不接受。

    Returns: dict 含：
        ok (bool): True 表示至少 1 张成功。
        total (int): 输入图总数。
        succeeded (int): 成功张数。
        failed (int): 失败张数。
        concurrency (int): 实际用的并发度（5 或 1）。
        results (list[dict]): 每张图的详细结果（含 input 路径、saved.path、可能的 error）。

    Examples:
        image_batch_edit(
            prompt="convert to pencil sketch style",
            image_paths=["/p/a.jpg", "/p/b.jpg", "/p/c.jpg"],
            size="1024x1024",
        )
    """
    # === 入口校验 ===
    if not isinstance(prompt, str) or not prompt.strip():
        return {"ok": False, "error": "prompt 不能为空", "errors": ["prompt 不能为空"], "total": 0}
    if not isinstance(image_paths, list) or len(image_paths) == 0:
        msg = "image_paths 必须是非空 list"
        return {"ok": False, "error": msg, "errors": [msg], "total": 0}
    if len(image_paths) > 20:
        msg = f"image_paths 最多 20 张，收到 {len(image_paths)} 张（防止意外 burn quota）"
        return {"ok": False, "error": msg, "errors": [msg], "total": len(image_paths)}
    if model_err := _model_error(DEFAULT_MODEL if model is None else model):
        return {"ok": False, "error": model_err, "errors": [model_err], "total": len(image_paths)}
    use_grok_request = _is_grok_model(model or DEFAULT_MODEL)
    if use_grok_request:
        cleaned_size, size_err = _validate_grok_size(size, allow_none=False)
    else:
        cleaned_size, size_err = _validate_size(size, allow_none=False)
    if size_err:
        return {"ok": False, "error": size_err, "errors": [size_err], "total": len(image_paths)}
    size = cleaned_size  # type: ignore[assignment]
    out_dir, dir_err = _resolve_save_dir(save_dir)
    if dir_err:
        return {"ok": False, "error": dir_err, "errors": [dir_err], "total": len(image_paths)}

    eff_model, notes = _resolve_model(model, size)
    if _is_grok_model(eff_model):
        msg = "Grok 模型当前不支持 image_batch_edit 批量逐张编辑；请改用 image_edit 单图、image_multi_reference 多图参考，或改用 gpt-image-2 / gpt-image-2-openai。"
        return {"ok": False, "error": msg, "errors": [msg], "total": len(image_paths)}
    is_quality = _is_quality_model(eff_model)
    edge = _max_edge(size)
    if edge >= HIGH_RES_EDGE:
        msg = (
            f"图生图代理后端 ≥2K 不稳定（503/524）；批处理只支持 1K（边长 ≤{EDITS_MAX_EDGE}）。"
            f"请改 size 到 1K，或改用 image_edit 单图处理 2K。"
        )
        return {"ok": False, "error": msg, "errors": [msg], "total": len(image_paths)}

    out_dir.mkdir(parents=True, exist_ok=True)
    # ≥2K 已在前面提前拒绝，这里 bypass 必然 False；高质量线路保持串行。
    concurrency = 1 if is_quality else 5
    inter_gap = 1.5 if concurrency == 1 else 0.0

    async def _run_one(idx: int, path_str: str) -> dict:
        try:
            r = await image_edit(
                prompt=prompt,
                image_path=path_str,
                size=size,
                model=eff_model,
                save_dir=str(out_dir),
                basename=f"batch_{time.time_ns()}_{idx + 1}",
                api_key=api_key,
            )
            r["index"] = idx + 1
            r["input"] = path_str
            return r
        except Exception as e:  # noqa: BLE001
            return {"ok": False, "index": idx + 1, "input": path_str, "error": str(e)}

    results: list[dict] = []
    if concurrency == 1:
        for i, p in enumerate(image_paths):
            if i > 0 and inter_gap:
                await asyncio.sleep(inter_gap)
            results.append(await _run_one(i, p))
    else:
        sem = asyncio.Semaphore(concurrency)

        async def _wrap(i: int, p: str) -> dict:
            async with sem:
                return await _run_one(i, p)

        results = await asyncio.gather(*(_wrap(i, p) for i, p in enumerate(image_paths)))
        results.sort(key=lambda x: x.get("index", 0))

    ok_count = sum(1 for r in results if r.get("ok"))
    return {
        "ok": ok_count > 0,
        "model": eff_model,
        "size": size,
        "concurrency": concurrency,
        "total": len(image_paths),
        "succeeded": ok_count,
        "failed": len(image_paths) - ok_count,
        "results": results,
        "notes": notes,
    }


@mcp.tool()
async def image_multi_reference(
    prompt: str,
    image_paths: list[str],
    size: str = "1024x1024",
    model: str | None = None,
    save_dir: str | None = None,
    basename: str | None = None,
    api_key: str | None = None,
) -> dict[str, Any]:
    """多图融合参考 → 输出 1 张新图（1K 稳定 + 2K best-effort 真 2K；4K 已禁用）。

    [WHAT] 输入 2-10 张参考图 + prompt，模型综合所有图的视觉信息后画 1 张全新的图。
    与 image_batch_edit 的本质区别：batch 是 N 进 N 出（每张独立改），此 tool 是 N 进 1 出（综合参考）。

    [WHEN TO USE]
      - 用户："这几张是同一产品的不同角度，按这个风格画一个新角度" → 用此 tool。
      - 用户："这些是我喜欢的风格，画一张类似风格的 X" → 用此 tool。
      - 用户："这是 logo 主图，这是辅助图，做成海报" → 用此 tool。
      - 如果用户只想"逐张修改" → 改用 image_batch_edit。
      - 如果用户只有 1 张图 → 改用 image_edit。
      - 如果用户没提供任何参考图 → 改用 image_generate。

    [4K 已禁用]
      origin 处理 4K 多图融合稳定 > 120s 撞 CF 524；入口直接拒。
      想要真 4K 多图融合：两步法 — 此 tool 出 1K/2K 综合图 → image_generate(size="3840x2160") 描述同场景升 4K。

    [路由实现]
      - 固定走 /v1/images/edits + 多个 image[] 字段。**米醋唯一真正消费输入图的端点**
        （实测 image_tokens 线性 = 560×N）。旧的 generations + image_urls 被米醋静默忽略
        （image_tokens=0，等于纯文生图，参考图不起作用），已弃用。
      - 自动切高质量线路：max edge ≥1600 → gpt-image-2-openai
      - Images API 返回错误时直接报错，不把图像模型转发到不兼容的 /v1/chat/completions。

    [LIMITS]（当前真实状态，会变化）
      - image_paths 长度 2-10 张。
      - 1K 档：多图 N=2..10 历史实测成功，参考图真消费；实际像素以 saved.actual_size 为准。
      - 2K 档：**best-effort 真 2K**（自动切 gpt-image-2-openai + edits/image[]，历史实测约 2/3 成功真返回 2048×2048，
        size_honored=true；失败时直接返回上游错误）。较慢（单次 2-4 分钟，并发更久）。
      - 4K 已禁用（撞 CF 524）。要真 4K：两步法 —— 本 tool 出 1K/2K 综合图 →
        image_generate(size="3840x2160") 描述同场景升分辨率（4K 升分辨率本身也常 524，best-effort）。
      - 主路径 ~30-100s（2K 更慢）；米醋多图间歇拒/断流时会按重试策略处理，仍失败则报错。
      - 单张参考图建议 ≤2MB；总输入 ≤8MB（米醋代理上限实测约 10MB）。

    Args:
        prompt: 综合指令。例："combine the colors from img1 and the composition from img2 into a sunset cityscape".
        image_paths: 2-10 张参考图路径（绝对或相对）。
        size: 输出 size。1K 档可用；2K 档（"2048x2048" 等）为 best-effort 真 2K（历史约 2/3 成功，
              成功时以 saved.actual_size 和 size_honored 核对真实像素）。真 4K 用两步法（见 [LIMITS]）。
              "3840x2160" / "2160x3840" 已禁用（撞 CF 524 物理上限），传入直接拒。
              默认 "1024x1024"。
        model: "gpt-image-2"（默认）/ "gpt-image-2-openai"（高质量线路，≥2K 自动切换）。
        save_dir: 输出目录（必须在安全根目录之下）。
        basename: 文件名前缀（仅 [A-Za-z0-9_-.]，含 / .. 会被拒）。默认 "multiref_<ns_ts>"。
        api_key: 覆盖 MICU_API_KEY；base_url 已锁在启动期 env，运行期不接受。

    Returns: dict 含：
        ok (bool): 是否成功。
        model (str): 实际用的模型。
        n_references (int): 实际嵌入的参考图张数。
        saved (dict): { path, size_bytes, actual_size, actual_megapixels }。
        notes (list[str]): 决策与提示。

    Examples:
        # 1K 综合参考
        image_multi_reference(
            prompt="combine these into a single cinematic poster",
            image_paths=["/p/sketch.png", "/p/character.png", "/p/background.png"],
        )

        # 2K 综合参考（高质量线路）
        image_multi_reference(
            prompt="merge the architecture style from img1 with the lighting from img2",
            image_paths=["/p/img1.jpg", "/p/img2.jpg"],
            size="2048x2048",
        )

    Common errors:
        "至少需要 2 张参考图" → 1 张请用 image_edit。
        "请求体超 X MB" → 减少图片数量或先压缩。
        "size=3840x2160 (4K) 在 image_multi_reference 已禁用" → 4K 多图融合物理撞 CF 524；
            两步法：先 1K/2K 出综合图 → image_generate(size="3840x2160") 升 4K。
    """
    # === 入口校验 ===
    if not isinstance(prompt, str) or not prompt.strip():
        return {"ok": False, "error": "prompt 不能为空", "errors": ["prompt 不能为空"]}
    if not isinstance(image_paths, list) or len(image_paths) < 2:
        msg = f"至少需要 2 张参考图（收到 {len(image_paths) if isinstance(image_paths, list) else 'non-list'}）。1 张请用 image_edit；0 张请用 image_generate。"
        return {"ok": False, "error": msg, "errors": [msg]}
    if len(image_paths) > 10:
        msg = f"参考图最多 10 张，当前 {len(image_paths)} 张。请减少或分批。"
        return {"ok": False, "error": msg, "errors": [msg]}
    if model_err := _model_error(DEFAULT_MODEL if model is None else model):
        return {"ok": False, "error": model_err, "errors": [model_err]}
    use_grok_request = _is_grok_model(model or DEFAULT_MODEL)
    if use_grok_request:
        cleaned_size, size_err = _validate_grok_size(size, allow_none=False)
    else:
        cleaned_size, size_err = _validate_size(size, allow_none=False)
    if size_err:
        return {"ok": False, "error": size_err, "errors": [size_err]}
    size = cleaned_size  # type: ignore[assignment]
    if not use_grok_request and (rej := _reject_4k_with_reference(size, "image_multi_reference")):
        return {"ok": False, "error": rej, "errors": [rej]}
    safe_stem = _safe_basename(basename) if basename is not None else None
    if basename is not None and safe_stem is None:
        msg = f"basename {basename!r} 含非法字符或路径分量"
        return {"ok": False, "error": msg, "errors": [msg]}
    out_dir, dir_err = _resolve_save_dir(save_dir)
    if dir_err:
        return {"ok": False, "error": dir_err, "errors": [dir_err]}

    eff_model, notes = _resolve_model(model, size)
    if _is_grok_model(eff_model):
        key = _get_grok_key(api_key)
        baseurl = _get_grok_baseurl()
        stem = safe_stem or _default_basename("multiref")
        size_mode = _grok_size_mode()
        image_urls: list[str] = []
        total_bytes = 0
        for idx, p_str in enumerate(image_paths):
            _ip, raw, mime, err = _validate_image_path(p_str, f"image_paths[{idx}]")
            if err:
                return {"ok": False, "error": err, "errors": [err]}
            total_bytes += len(raw)
            if total_bytes > MAX_TOTAL_INPUT_BYTES:
                msg = (
                    f"参考图累计 {total_bytes/1024/1024:.1f}MB 超过总量上限 "
                    f"{MAX_TOTAL_INPUT_BYTES/1024/1024:.0f}MB（base64 后会膨胀 33%）。请压缩或减少。"
                )
                return {"ok": False, "error": msg, "errors": [msg]}
            ref_b64 = await asyncio.to_thread(lambda r=raw: base64.b64encode(r).decode())
            image_urls.append(f"data:{mime};base64,{ref_b64}")
        notes.append(
            "Grok 多图参考走 /v1/images/generations + image_urls；"
            "size 只映射为 resolution/aspect_ratio，实际像素由 Grok/MICU 返回决定。"
        )
        if size_mode == "backend":
            notes.append("Grok 尺寸模式 MICU_GROK_SIZE_MODE=backend：保留后端原始像素。")
        else:
            notes.append(
                f"Grok 保存前会按 MICU_GROK_SIZE_MODE={size_mode} 本地归一化到请求 size={size}。"
            )
        full_prompt = (
            "Reference images are provided. Synthesize their visual elements into ONE single new image. "
            "Do NOT collage, tile, or montage the references side-by-side unless explicitly asked.\n\n"
            f"Instruction:\n{prompt}"
        )
        saved_info, save_err = await _call_and_save_with_format_fallback(
            build_ep=lambda fmt: Endpoint(
                url=f"{baseurl}/v1/images/generations",
                json_body={
                    "model": eff_model,
                    "prompt": full_prompt,
                    "n": 1,
                    "resolution": _grok_resolution(size),
                    "aspect_ratio": _grok_aspect_ratio(size),
                    "image_urls": image_urls,
                    "response_format": fmt,
                },
            ),
            key=key,
            retry_pro=True,
            stream=False,
            big_size_lock=False,
            notes=notes,
            out_dir=out_dir,
            stem=stem,
            requested_size=size,
            normalize_size=size,
            normalize_mode=size_mode,
            normalize_label="Grok 多图参考",
        )
        if save_err:
            return {
                "ok": False,
                "model": eff_model,
                "size": size,
                "n_references": len(image_paths),
                "error": save_err,
                "errors": [save_err],
                "notes": notes,
            }
        return {
            "ok": True,
            "model": eff_model,
            "size": size,
            "used_fallback": False,
            "n_references": len(image_paths),
            "saved": saved_info,
            "notes": notes,
        }
    key = _get_key(api_key)
    baseurl = _get_baseurl()
    is_quality = _is_quality_model(eff_model)
    stem = safe_stem or _default_basename("multiref")

    # 加载所有图：每张大小 + magic 校验，再算总字节；edits multipart 使用原始字节（image[]）。
    ref_files: list[tuple[str, bytes, str]] = []
    total_bytes = 0
    for idx, p_str in enumerate(image_paths):
        ip, raw, mime, err = _validate_image_path(p_str, f"image_paths[{idx}]")
        if err:
            return {"ok": False, "error": err, "errors": [err]}
        total_bytes += len(raw)
        if total_bytes > MAX_TOTAL_INPUT_BYTES:
            msg = (
                f"参考图累计 {total_bytes/1024/1024:.1f}MB 超过总量上限 "
                f"{MAX_TOTAL_INPUT_BYTES/1024/1024:.0f}MB。请压缩或减少。"
            )
            return {"ok": False, "error": msg, "errors": [msg]}
        ref_files.append((ip.name, raw, mime))

    # 固定走 /v1/images/edits + 多个 image[]：该字段会真正消费参考图。
    # 旧 generations+image_urls 会被静默忽略（image_tokens=0），chat/completions 也不是当前图像模型端点。
    full_prompt = (
        f"Reference images are provided. Synthesize their visual elements (style, palette, "
        f"composition, subjects) into ONE single new image per the instruction below. "
        f"Do NOT collage, tile, or montage the references side-by-side unless explicitly asked.\n\n"
        f"Instruction:\n{prompt}"
    )
    aggressive_retry = is_quality or _size_tier(size) in ("2k", "4k")
    big_size_lock = _size_tier(size) in ("2k", "4k")
    last_err: str | None = None
    saved_info: dict[str, Any] | None = None

    for fmt_i, fmt in enumerate(RESPONSE_FORMATS_TO_TRY):
        if fmt_i > 0:
            notes.append(f"URL 落盘失败（{last_err}）→ 重试 API response_format={fmt}")
        edits_ep = Endpoint(
            url=f"{baseurl}/v1/images/edits",
            multipart={
                "model": eff_model,
                "prompt": full_prompt,
                "size": size,
                "response_format": fmt,
                "image[]": ref_files,
            },
        )
        status, text = await _call_with_retry(
            edits_ep, key, retry_pro=aggressive_retry, stream=False,
            big_size_lock=big_size_lock, notes_out=notes,
        )
        if not (200 <= status < 300):
            last_err = f"HTTP {status}: {_error_detail(text)}"
            continue
        saved_info, save_err = await _save_first_payload_from_response(
            text, out_dir, stem, notes, size,
        )
        if saved_info:
            if fmt_i > 0:
                notes.append(f"已通过 response_format={fmt} 成功落盘")
            break
        last_err = save_err or "保存失败"

    if not saved_info:
        return {
            "ok": False,
            "model": eff_model,
            "n_references": len(image_paths),
            "used_fallback": False,
            "error": last_err or "保存失败",
            "notes": notes,
        }

    actual = _parse_actual(saved_info.get("actual_size"))
    return {
        "ok": True,
        "model": eff_model,
        "size": size,
        "used_fallback": False,
        "size_honored": bool(actual and _parse_size(size) and actual == _parse_size(size)),
        "n_references": len(image_paths),
        "saved": saved_info,
        "notes": notes,
    }


@mcp.tool()
def server_info() -> dict[str, Any]:
    """返回当前 Image2 模型、参数约束、路由和安全边界。"""
    return {
        "base_url": DEFAULT_BASEURL,
        "grok_base_url": GROK_BASEURL,
        "default_model": DEFAULT_MODEL,
        "grok_default_model": XAI_MODEL,
        "grok_size_mode": _grok_size_mode(),
        "available_models": list(SUPPORTED_IMAGE_MODELS),
        "grok_available_models": [],
        "grok_channel_enabled": False,
        "grok_channel_status": "暂时关闭，待服务器支持后再启用",
        "default_save_dir": str(DEFAULT_SAVE_DIR),
        "api_key_configured": bool(API_KEY),
        "grok_api_key_configured": bool(GROK_API_KEY),
        "response_format": API_RESPONSE_FORMAT,
        "response_formats_to_try": list(RESPONSE_FORMATS_TO_TRY),
        "trusted_download_hosts": sorted(TRUSTED_DOWNLOAD_HOSTS),
        "allow_fake_ip_download": ALLOW_FAKE_IP_DOWNLOAD,
        "size_rules": {
            "format": "WxH 字符串（如 '1024x1024'）",
            "alignment": f"W 与 H 都必须是 {SIZE_ALIGNMENT} 的整数倍",
            "edge_range": f"W/H 必须在 [{MIN_SIZE_EDGE}, {MAX_SIZE_EDGE}] 范围内",
            "pixel_range": f"总像素必须在 [{MIN_IMAGE_PIXELS:,}, {MAX_IMAGE_PIXELS:,}] 范围内",
            "max_aspect_ratio": f"最长边/最短边 ≤ {MAX_IMAGE_ASPECT_RATIO:g}",
            "quality_values": sorted(VALID_IMAGE_QUALITIES),
            "auto_quality_route": f"max edge ≥ {HIGH_RES_EDGE} → 自动切 {QUALITY_MODEL}",
            "live_observation_2026_08_14": (
                "gpt-image-2-openai 的 1536×1024、2048×1152、3840×2160 按请求像素返回；"
                "gpt-image-2 的自定义尺寸可能被后端重映射，调用方应核对 saved.actual_size。"
            ),
        },
        "safety_constraints": {
            "n_range": f"image_generate 的 n ∈ [1, {MAX_N}]，超出立即拒（防 burn quota）",
            "save_dir_root": (
                f"所有输出强制落在 MICU_SAVE_DIR_ROOT={_SAVE_ROOT} 之下；"
                "传 root 之外路径会被拒"
            ),
            "basename_charset": "basename 仅允许 [A-Za-z0-9_-.]，禁含 / .. 和路径分量",
            "input_size_limits": (
                f"单输入图 ≤{MAX_INPUT_FILE_BYTES//1024//1024}MB；"
                f"image_multi_reference 总和 ≤{MAX_TOTAL_INPUT_BYTES//1024//1024}MB"
            ),
            "input_image_validation": "所有输入图按 magic bytes 校验为 PNG/JPEG/WebP/GIF；非图片立即拒（防本地任意文件外传）",
            "response_size_limit": f"远端响应 ≤{MAX_RESPONSE_BYTES//1024//1024}MB；超过中断不落盘",
            "base_url_locked": "base_url 锁在启动期 MICU_BASEURL env，运行期 tool 不接受参数（防 key 外泄到攻击者 host）",
        },
        "recommended_sizes": {
            "1k_standard": sorted(VALID_SIZES_1K),
            "2k_quality": sorted(VALID_SIZES_2K),
            "4k_quality": sorted(VALID_SIZES_4K),
            "tip": (
                "1K 默认走 gpt-image-2；2K/4K 自动走 gpt-image-2-openai。"
                "服务端仍可能调整实际像素，始终核对 saved.actual_size。"
            ),
            "two_step_tip": (
                "带参考图的 4K 编辑仍禁用：先用 image_edit/image_multi_reference 出 1K/2K，"
                "再用 image_generate(size=\"3840x2160\") 生成高分辨率版本。"
            ),
            "grok_tip": "Grok 生图渠道暂时关闭，待服务器支持后再启用。",
            "grok_actual_size_tip": "Grok 生图渠道暂时关闭，当前不会生成 Grok 输出。",
        },
        "capability_matrix": {
            "image_generate": {
                "1k": "gpt-image-2；N>1 最多 5 并发",
                "2k_quality": "自动切 gpt-image-2-openai；N 强制为 1；实测 2048×1152 精确返回",
                "4k_quality": "自动切 gpt-image-2-openai；N 强制为 1；实测 3840×2160 精确返回",
            },
            "grok_image_generate": {
                "1k": "暂时关闭，待服务器支持后再启用",
                "2k": "暂时关闭，待服务器支持后再启用",
                "4k": "暂时关闭，待服务器支持后再启用",
            },
            "image_edit": {
                "1k": "两条当前 Image2 线路均已通过 edits 实测；支持可选 alpha mask",
                "2k_quality": "自动切 gpt-image-2-openai；实测 2048×1152 精确返回",
                "4k": "已禁用：历史上 origin 处理 4K + 参考图稳定 >120s 撞 CF",
            },
            "image_batch_edit": {
                "1k_standard": "5 并发",
                "1k_quality": "串行 + 1.5s gap",
                ">=2k": "拒绝",
                "grok": "暂时关闭，待服务器支持后再启用",
            },
            "image_multi_reference": {
                "1k": "2-10 张参考图融合输出 1 张，走 edits + image[]",
                "2k_quality": "自动切 gpt-image-2-openai；失败时直接返回 Images API 错误",
                "4k": "已禁用：历史上 origin 处理 4K 多图融合稳定 >120s 撞 CF",
            },
        },
        "retry_policy": {
            "retryable_status": list(RETRYABLE_STATUS),
            "fallback_status": list(FALLBACK_STATUS),
            "route_fallback": "当前 GPT Image 2 模型只走 Images API，不 fallback 到 chat/completions",
            "schedule_1k": "上游 5xx → 4s+jitter → 重试 → 8s+jitter → 重试（退避重试最多 2 次，共 ≤3 次尝试）；网络层异常（status=0）另有 1 次免费重试不计入此预算（故最坏 ≤4 次）。注：带 Retry-After 的 408/429/5xx 会按头部值 sleep（上限 120s），此时单次等待可能远大于 4s/8s",
            "schedule_2k_4k": "双层锁内：可恢复 5xx → 60s → 重试 1 次（共 2 次尝试）；CF 524 fail fast 不重试（origin 持续慢，等也无用）。单次 attempt 无字节挂起最长 600s，故 2K 最坏 ≈ 两次 600s attempt + 一次 60s 退避；其间整机所有 ≥2K 请求经跨进程锁串行等待。锁等待 >2s 时 notes 会提示在排队",
            "trigger": f"model == {QUALITY_MODEL!r} 或 size tier ∈ {{2k, 4k}}",
            "concurrency_2k_4k": (
                "双层锁: (1) 进程内 asyncio.Semaphore(1) 同 MCP 进程内并发本地排队; "
                "(2) 跨进程文件锁 @ ~/.cache/micu-image/bigsize.lock，POSIX 用 fcntl.flock，"
                "Windows 用 msvcrt.locking —— 多 Claude Code/Codex 窗口各自独立 MCP 子进程时"
                "跨进程串行打 origin，整机任意时刻只有一张 ≥2K 在 origin 排队。"
            ),
        },
        "response_handling": {
            "saved_to_disk": "所有生成的图片落盘到 save_dir（默认 cwd/out 或 MICU_SAVE_DIR）",
            "actual_size_field": "返回的 saved[].actual_size 是从 PNG/JPEG header 读出的真实像素，可与请求 size 对比验证",
            "extract_paths": "支持 data[].b64_json / data[].url / chat content markdown 三种响应格式",
            "grok_extract_paths": "Grok 生图渠道暂时关闭，当前不会执行 Grok 响应提取。",
        },
    }


def main() -> None:
    mcp.run()


if __name__ == "__main__":
    main()
