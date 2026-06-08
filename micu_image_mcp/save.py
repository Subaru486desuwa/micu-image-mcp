"""图片落盘逻辑：base64 解码 / URL 下载 / Grok 尺寸归一化 / 文件名防覆盖 / 路径安全确认。"""
from __future__ import annotations

import asyncio
import base64
import io
import ipaddress
import os
import socket
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from .config import MAX_RESPONSE_BYTES, _SAVE_ROOT
from .extract import _extract_image_payload, _parse_response
from .io_safety import _detect_actual_size, _validate_image_bytes
from .routing import _size_note
from .sizes import _parse_size
from .http_client import _get_http_client


class ImageSaveError(Exception):
    """落盘前校验失败（响应过大 / 不是合法图片 / 路径越界）。"""


# PIL 归一化前的像素上限（防解压炸弹）。4K=8.3MP / 2K=4MP，64MP 留足余量且远低于 Pillow 默认 bomb 阈值。
_MAX_NORMALIZE_PIXELS = 64 * 1024 * 1024

# URL 下载墙钟总上限（防慢滴流上游无限挂起；httpx 的 120s 只卡单次 connect/read）。
_DOWNLOAD_TOTAL_TIMEOUT_SECONDS = 180.0

# response_format=url 后，对端返回的 url 成为主下载路径，下载前必须校验防 SSRF。
_ALLOWED_DOWNLOAD_SCHEMES = ("http", "https")


def _ip_is_blocked(ip: "ipaddress.IPv4Address | ipaddress.IPv6Address") -> bool:
    # IPv4-mapped IPv6（如 ::ffff:127.0.0.1）先解包再判，避免绕过。
    if isinstance(ip, ipaddress.IPv6Address) and ip.ipv4_mapped is not None:
        ip = ip.ipv4_mapped
    return (
        ip.is_private or ip.is_loopback or ip.is_link_local
        or ip.is_reserved or ip.is_multicast or ip.is_unspecified
    )


def _assert_download_url_safe(url: str) -> None:
    """SSRF 防护：拒绝非 http/https，以及 host 解析到私网/环回/链路本地/保留/多播地址。

    host 为 IP 字面量时直接判；为域名时 DNS 解析所有结果逐一校验（挡住直接指向内网的域名；
    残留 DNS-rebinding TOCTOU 由 follow_redirects=False 与即时下载收窄）。校验不过抛 ImageSaveError。
    """
    try:
        parts = urlsplit(url)
    except Exception as e:  # noqa: BLE001
        raise ImageSaveError(f"下载 URL 解析失败: {e!r}") from e
    if parts.scheme.lower() not in _ALLOWED_DOWNLOAD_SCHEMES:
        raise ImageSaveError(f"下载 URL scheme 非法（仅允许 http/https）: {parts.scheme!r}")
    host = parts.hostname
    if not host:
        raise ImageSaveError("下载 URL 缺少 host")
    candidates: list[str] = []
    try:
        candidates.append(str(ipaddress.ip_address(host)))
    except ValueError:
        port = parts.port or (443 if parts.scheme.lower() == "https" else 80)
        try:
            infos = socket.getaddrinfo(host, port, proto=socket.IPPROTO_TCP)
        except OSError as e:
            raise ImageSaveError(f"下载 URL host 解析失败 {host!r}: {e}") from e
        candidates = [info[4][0] for info in infos]
    if not candidates:
        raise ImageSaveError(f"下载 URL host 无法解析: {host!r}")
    for addr in candidates:
        try:
            ip = ipaddress.ip_address(addr.split("%")[0])  # 去掉 IPv6 zone id
        except ValueError as e:
            raise ImageSaveError(f"下载 URL 解析出非法地址 {addr!r}") from e
        if _ip_is_blocked(ip):
            raise ImageSaveError(
                f"下载 URL host {host!r} 指向受限地址 {ip}"
                f"（私网/环回/链路本地/保留），已拒绝（SSRF 防护）"
            )


def _normalized_image_bytes_sync(raw: bytes, requested_size: str, mode: str) -> tuple[bytes, tuple[int, int] | None]:
    target = _parse_size(requested_size)
    actual = _detect_actual_size(raw)
    if not target or not actual or mode == "backend" or actual == target:
        return raw, actual

    try:
        from PIL import Image, ImageOps  # type: ignore[import-not-found]
    except ImportError as e:
        raise ImageSaveError(
            "Grok 精确尺寸后处理需要 Pillow。请重新运行 install.py，或设置 MICU_GROK_SIZE_MODE=backend 关闭后处理。"
        ) from e

    tw, th = target
    with Image.open(io.BytesIO(raw)) as im:
        # 像素上限防解压炸弹：header 宣称的尺寸在 decode 前就能拿到。
        if im.width * im.height > _MAX_NORMALIZE_PIXELS:
            raise ImageSaveError(
                f"待归一化图 {im.width}x{im.height} 像素超过上限 {_MAX_NORMALIZE_PIXELS}；"
                f"疑似异常响应，已拒绝。可设 MICU_GROK_SIZE_MODE=backend 跳过本地后处理。"
            )
        im = ImageOps.exif_transpose(im)
        has_alpha = im.mode in ("RGBA", "LA") or ("transparency" in im.info)
        if has_alpha:
            im = im.convert("RGBA")
        else:
            im = im.convert("RGB")

        resample = Image.Resampling.LANCZOS
        if mode == "stretch":
            out = im.resize((tw, th), resample)
        elif mode == "cover":
            out = ImageOps.fit(im, (tw, th), method=resample, centering=(0.5, 0.5))
        else:
            fitted = ImageOps.contain(im, (tw, th), method=resample)
            if has_alpha:
                out = Image.new("RGBA", (tw, th), (255, 255, 255, 0))
            else:
                out = Image.new("RGB", (tw, th), (255, 255, 255))
            x = (tw - fitted.width) // 2
            y = (th - fitted.height) // 2
            out.paste(fitted, (x, y), fitted if fitted.mode == "RGBA" else None)

        buf = io.BytesIO()
        out.save(buf, format="PNG", optimize=True)
        return buf.getvalue(), (tw, th)


async def _maybe_normalize_image_bytes(
    raw: bytes,
    *,
    requested_size: str | None,
    mode: str | None,
    notes: list[str] | None,
    label: str,
) -> bytes:
    if not requested_size or not mode or mode == "backend":
        return raw
    before = _detect_actual_size(raw)
    normalized, after = await asyncio.to_thread(_normalized_image_bytes_sync, raw, requested_size, mode)
    if before and after and before != after and notes is not None:
        notes.append(
            f"{label} 后端返回 {before[0]}x{before[1]}，已按 MICU_GROK_SIZE_MODE={mode} "
            f"本地后处理为 {after[0]}x{after[1]}。"
        )
    return normalized


async def _save_validated_bytes(raw: bytes, save_dir: Path, basename: str, *, source_label: str) -> tuple[Path, tuple[int, int] | None, int]:
    """统一落盘逻辑：校验大小 + magic + 路径安全 + 防覆盖。

    返回 (path, actual_size, size_bytes)。size_bytes 直接用 len(raw) 而非额外 stat()。
    write_bytes 走 asyncio.to_thread 避免 4K 12MB 落盘阻塞事件循环。
    """
    if len(raw) > MAX_RESPONSE_BYTES:
        raise ImageSaveError(
            f"{source_label} 响应 {len(raw)/1024/1024:.1f}MB 超过单图上限 "
            f"{MAX_RESPONSE_BYTES/1024/1024:.0f}MB；可能是代理返回了错误内容"
        )
    err = _validate_image_bytes(raw, source_label)
    if err:
        raise ImageSaveError(err)
    # 由 magic 决定 ext
    if raw[:8] == b"\x89PNG\r\n\x1a\n":
        ext = "png"
    elif raw[:3] == b"\xff\xd8\xff":
        ext = "jpg"
    elif raw[:6] in (b"GIF87a", b"GIF89a"):
        ext = "gif"
    elif raw[:4] == b"RIFF":
        ext = "webp"
    else:
        ext = "png"  # 不该到这（_validate_image_bytes 应已拒）

    save_dir.mkdir(parents=True, exist_ok=True)

    def _atomic_write() -> Path:
        # 防覆盖用 O_CREAT|O_EXCL 原子创建（替代 exists()→write 的 TOCTOU），冲突则递增 _2 _3…。
        # 越界复查以 _SAVE_ROOT 为基准（而非 save_dir）：即便 save_dir 被换成指向根外的符号链接也挡得住。
        candidate = save_dir / f"{basename}.{ext}"
        counter = 2
        while True:
            try:
                candidate.resolve().relative_to(_SAVE_ROOT)
            except ValueError as e:
                raise ImageSaveError(f"落盘路径越界: {candidate}") from e
            try:
                fd = os.open(str(candidate), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o644)
            except FileExistsError:
                candidate = save_dir / f"{basename}_{counter}.{ext}"
                counter += 1
                if counter > 1000:
                    raise ImageSaveError(f"basename 冲突过多：{basename}")
                continue
            try:
                with os.fdopen(fd, "wb") as f:
                    f.write(raw)
            except BaseException:
                try:
                    os.unlink(candidate)
                except OSError:
                    pass
                raise
            return candidate

    path = await asyncio.to_thread(_atomic_write)
    return path, _detect_actual_size(raw), len(raw)


async def _save_image_b64(
    b64: str,
    save_dir: Path,
    basename: str,
    *,
    normalize_size: str | None = None,
    normalize_mode: str | None = None,
    notes: list[str] | None = None,
    normalize_label: str = "图片",
) -> tuple[Path, tuple[int, int] | None, int]:
    # 解码前先按 b64 长度估算解码后字节（约 3/4），超上限直接拒，避免先把超大响应解进内存再事后检查。
    est_decoded = len(b64) * 3 // 4
    if est_decoded > MAX_RESPONSE_BYTES:
        raise ImageSaveError(
            f"b64 响应解码后约 {est_decoded/1024/1024:.1f}MB 超过单图上限 "
            f"{MAX_RESPONSE_BYTES/1024/1024:.0f}MB；可能是代理返回了错误内容"
        )
    try:
        # 大图 base64 解码（4K 16MB → 12MB）走 to_thread，避免 30-50ms 事件循环阻塞
        raw = await asyncio.to_thread(base64.b64decode, b64, validate=False)
    except Exception as e:  # noqa: BLE001
        raise ImageSaveError(f"base64 解码失败: {e}") from e
    raw = await _maybe_normalize_image_bytes(
        raw,
        requested_size=normalize_size,
        mode=normalize_mode,
        notes=notes,
        label=normalize_label,
    )
    return await _save_validated_bytes(raw, save_dir, basename, source_label="b64 响应")


async def _save_image_url(
    url: str,
    save_dir: Path,
    basename: str,
    *,
    normalize_size: str | None = None,
    normalize_mode: str | None = None,
    notes: list[str] | None = None,
    normalize_label: str = "图片",
) -> tuple[Path, tuple[int, int] | None, int]:
    # SSRF 防护：下载前校验 url（DNS 解析走线程，不阻塞事件循环）。
    await asyncio.to_thread(_assert_download_url_safe, url)
    cx = _get_http_client()
    # stream 提前读 Content-Length 拒超大响应；follow_redirects=False 防经 3xx 重定向到内网绕过校验；
    # asyncio.timeout 给整段下载加墙钟上限，防慢滴流无限挂起独占大图锁。
    try:
        async with asyncio.timeout(_DOWNLOAD_TOTAL_TIMEOUT_SECONDS):
            async with cx.stream("GET", url, timeout=120.0, follow_redirects=False) as r:
                r.raise_for_status()
                cl = r.headers.get("content-length")
                if cl and cl.isdigit() and int(cl) > MAX_RESPONSE_BYTES:
                    raise ImageSaveError(
                        f"远端图 Content-Length={int(cl)/1024/1024:.1f}MB 超过 "
                        f"{MAX_RESPONSE_BYTES/1024/1024:.0f}MB 上限"
                    )
                chunks: list[bytes] = []
                total = 0
                async for chunk in r.aiter_bytes():
                    total += len(chunk)
                    if total > MAX_RESPONSE_BYTES:
                        raise ImageSaveError(
                            f"远端图实际下载 >{MAX_RESPONSE_BYTES/1024/1024:.0f}MB，已中断"
                        )
                    chunks.append(chunk)
                raw = b"".join(chunks)
    except TimeoutError as e:
        raise ImageSaveError(
            f"远端图下载超过 {_DOWNLOAD_TOTAL_TIMEOUT_SECONDS:.0f}s 墙钟上限，已中断"
        ) from e
    raw = await _maybe_normalize_image_bytes(
        raw,
        requested_size=normalize_size,
        mode=normalize_mode,
        notes=notes,
        label=normalize_label,
    )
    return await _save_validated_bytes(raw, save_dir, basename, source_label=f"远端图 {url[:80]}")


async def _save_first_payload_from_response(
    text: str,
    out_dir: Path,
    stem: str,
    notes: list[str],
    requested_size: str,
    *,
    normalize_size: str | None = None,
    normalize_mode: str | None = None,
    normalize_label: str = "图片",
) -> tuple[dict[str, Any] | None, str | None]:
    resp = _parse_response(text)
    b64, url = _extract_image_payload(resp)
    try:
        if b64:
            p, actual, size_bytes = await _save_image_b64(
                b64,
                out_dir,
                stem,
                normalize_size=normalize_size,
                normalize_mode=normalize_mode,
                notes=notes,
                normalize_label=normalize_label,
            )
        elif url:
            p, actual, size_bytes = await _save_image_url(
                url,
                out_dir,
                stem,
                normalize_size=normalize_size,
                normalize_mode=normalize_mode,
                notes=notes,
                normalize_label=normalize_label,
            )
        else:
            return None, "响应中未识别到图片"
    except Exception as e:  # noqa: BLE001
        return None, f"保存失败: {e}"

    saved_info: dict[str, Any] = {"path": str(p.resolve()), "size_bytes": size_bytes}
    if actual:
        saved_info["actual_size"] = f"{actual[0]}x{actual[1]}"
        saved_info["actual_megapixels"] = round(actual[0] * actual[1] / 1_000_000, 2)
        sn = _size_note(requested_size, actual)
        if sn and sn not in notes:
            notes.append(sn)
    return saved_info, None


__all__ = [
    "ImageSaveError",
    "_normalized_image_bytes_sync", "_maybe_normalize_image_bytes",
    "_save_validated_bytes", "_save_image_b64", "_save_image_url",
    "_save_first_payload_from_response",
]
