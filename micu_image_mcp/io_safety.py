"""basename / save_dir / 图片字节 / mask 安全校验 + PNG header 解析。"""
from __future__ import annotations

import time
from pathlib import Path

from .config import (
    _SAVE_ROOT, DEFAULT_SAVE_DIR, _INPUT_ROOT,
    _SAFE_BASENAME_RE,
    MAX_INPUT_FILE_BYTES,
)


def _safe_basename(name: str | None) -> str | None:
    """剥掉所有路径分量，限制安全字符集；非法返回 None。"""
    if name is None:
        return None
    if not isinstance(name, str) or not name.strip():
        return None
    only = Path(name).name
    if only != name:
        return None  # 含 / 或 \ 直接拒
    if ".." in only or only.startswith("."):
        return None
    if not _SAFE_BASENAME_RE.match(only):
        return None
    if len(only) > 100:
        return None
    return only


def _resolve_save_dir(save_dir: str | None) -> tuple[Path | None, str | None]:
    """save_dir 限定在 _SAVE_ROOT 之下。返回 (resolved_dir, error_message)。"""
    try:
        _SAVE_ROOT.mkdir(parents=True, exist_ok=True)
    except Exception as e:  # noqa: BLE001
        return None, f"无法创建 save root {_SAVE_ROOT}: {e}"
    if save_dir is None:
        # 默认用环境变量 DEFAULT_SAVE_DIR；如果它在 _SAVE_ROOT 内就用，否则用 root 本身
        try:
            DEFAULT_SAVE_DIR.expanduser().resolve().relative_to(_SAVE_ROOT)
            return DEFAULT_SAVE_DIR.expanduser().resolve(), None
        except (ValueError, OSError):
            return _SAVE_ROOT, None
    p = Path(save_dir).expanduser()
    try:
        resolved = p.resolve()
        resolved.relative_to(_SAVE_ROOT)
    except (ValueError, OSError):
        return None, (
            f"save_dir 必须在安全根目录 {_SAVE_ROOT} 之下；收到 {save_dir!r}。"
            f"留空让 MCP 用默认目录，或先把 MICU_SAVE_DIR_ROOT 改到你想要的位置。"
        )
    return resolved, None


def _validate_image_bytes(raw: bytes, label: str = "image") -> str | None:
    """通过 magic bytes 校验是 PNG/JPEG/WebP/GIF；返回 None 合法，否则错误描述。"""
    if not raw or len(raw) < 16:
        return f"{label} 太小（{len(raw) if raw else 0} 字节），不像合法图片"
    # PNG
    if raw[:8] == b"\x89PNG\r\n\x1a\n":
        return None
    # JPEG
    if raw[:3] == b"\xff\xd8\xff":
        return None
    # WebP
    if raw[:4] == b"RIFF" and len(raw) >= 12 and raw[8:12] == b"WEBP":
        return None
    # GIF
    if raw[:6] in (b"GIF87a", b"GIF89a"):
        return None
    return f"{label} 不是受支持的图片格式（PNG/JPEG/WebP/GIF magic 不匹配）"


def _validate_image_path(image_path: str, label: str = "image_path") -> tuple[Path, bytes, str, str | None]:
    """读图 + 校验（大小 + magic）。返回 (path, bytes, mime, error_message)。
    error 非 None 时其他字段不可用。
    """
    err: str | None = None
    p = Path(image_path).expanduser()
    # 可选输入白名单根（MICU_INPUT_ROOT）：启用后拒绝根外路径，resolve 后比对可挡符号链接逃逸。
    # 防被 prompt 注入的 LLM 用任意本地路径读取并外发文件。默认 _INPUT_ROOT=None 不限制。
    if _INPUT_ROOT is not None:
        try:
            p.resolve().relative_to(_INPUT_ROOT)
        except (ValueError, OSError):
            return p, b"", "", (
                f"{label} 必须在 MICU_INPUT_ROOT={_INPUT_ROOT} 之下（已启用输入路径白名单）；收到 {image_path!r}"
            )
    if not p.is_file():
        return p, b"", "", f"{label} 不存在: {p}"
    try:
        sz = p.stat().st_size
    except OSError as e:
        return p, b"", "", f"{label} 无法 stat: {e}"
    if sz > MAX_INPUT_FILE_BYTES:
        return p, b"", "", (
            f"{label} 文件 {sz/1024/1024:.1f}MB 超过单文件上限 "
            f"{MAX_INPUT_FILE_BYTES/1024/1024:.0f}MB；请先压缩"
        )
    try:
        raw = p.read_bytes()
    except OSError as e:
        return p, b"", "", f"{label} 读取失败: {e}"
    err = _validate_image_bytes(raw, label)
    if err:
        return p, raw, "", err
    # 重型校验：能否真解出宽高（防只有头的伪文件 / 截断文件）
    actual = _detect_actual_size(raw)
    if actual is None:
        return p, raw, "", (
            f"{label} 头部像图片，但解析不出宽高（可能截断、损坏或伪造）"
        )
    if actual[0] < 16 or actual[1] < 16:
        return p, raw, "", f"{label} 尺寸 {actual[0]}x{actual[1]} 太小，不像正常图片"
    # 由 magic 决定 mime（不再信扩展名）
    if raw[:8] == b"\x89PNG\r\n\x1a\n":
        mime = "image/png"
    elif raw[:3] == b"\xff\xd8\xff":
        mime = "image/jpeg"
    elif raw[:4] == b"RIFF":
        mime = "image/webp"
    else:
        mime = "image/gif"
    return p, raw, mime, None


def _png_color_type(raw: bytes) -> int | None:
    """PNG IHDR 第 9 字节 (offset 25 from file start) 是 color type。

    color type 编码：
      0 = 灰度        2 = RGB         3 = 调色板
      4 = 灰度+alpha  6 = RGB+alpha
    含 alpha 通道：4 或 6。
    """
    if len(raw) < 26 or raw[:8] != b"\x89PNG\r\n\x1a\n":
        return None
    return raw[25]


def _validate_mask_against_image(
    mask_raw: bytes,
    image_size: tuple[int, int],
) -> str | None:
    """mask 必须满足：PNG + 与原图同尺寸 + 含 alpha 通道。"""
    if mask_raw[:8] != b"\x89PNG\r\n\x1a\n":
        return "mask_path 必须是 PNG（OpenAI 规范要求 alpha 通道）"
    mask_size = _detect_actual_size(mask_raw)
    if mask_size is None:
        return "mask PNG 头损坏，解析不出尺寸"
    if mask_size != image_size:
        return (
            f"mask 尺寸 {mask_size[0]}x{mask_size[1]} 必须与原图 "
            f"{image_size[0]}x{image_size[1]} 一致"
        )
    color_type = _png_color_type(mask_raw)
    if color_type not in (4, 6):
        type_desc = {0: "灰度", 2: "RGB", 3: "调色板"}.get(color_type, f"未知 ({color_type})")
        return (
            f"mask PNG color_type={color_type}（{type_desc}），缺 alpha 通道；"
            f"必须用 GA(4) 或 RGBA(6) 格式，alpha=0 标记编辑区"
        )
    return None


def _default_basename(prefix: str) -> str:
    """ns 时间戳避免秒级冲突。"""
    return f"{prefix}_{time.time_ns()}"


def _detect_actual_size(raw: bytes) -> tuple[int, int] | None:
    """从原始字节里读 PNG/JPEG/WebP 的实际像素尺寸，不依赖 PIL。"""
    if len(raw) < 24:
        return None
    # PNG: 8B 签名 + IHDR (length=13) + 'IHDR' + width(4B) + height(4B)
    if raw[:8] == b"\x89PNG\r\n\x1a\n" and raw[12:16] == b"IHDR":
        w = int.from_bytes(raw[16:20], "big")
        h = int.from_bytes(raw[20:24], "big")
        return w, h
    # JPEG: 扫 SOFn marker
    if raw[:3] == b"\xff\xd8\xff":
        i = 2
        while i < len(raw) - 9:
            if raw[i] != 0xFF:
                i += 1
                continue
            marker = raw[i + 1]
            i += 2
            if marker in (0xD8, 0xD9):
                continue
            if 0xD0 <= marker <= 0xD7:
                continue
            seg_len = int.from_bytes(raw[i:i + 2], "big")
            if marker in (0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7,
                          0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF):
                h = int.from_bytes(raw[i + 3:i + 5], "big")
                w = int.from_bytes(raw[i + 5:i + 7], "big")
                return w, h
            i += seg_len
        return None
    # WebP VP8/VP8L/VP8X
    if raw[:4] == b"RIFF" and raw[8:12] == b"WEBP":
        chunk = raw[12:16]
        if chunk == b"VP8 ":
            if len(raw) < 30:
                return None  # 截断：与其它分支一致返回 None 而非越界 IndexError
            w = int.from_bytes(raw[26:28], "little") & 0x3FFF
            h = int.from_bytes(raw[28:30], "little") & 0x3FFF
            return w, h
        if chunk == b"VP8L":
            if len(raw) < 25:
                return None
            b1, b2, b3, b4 = raw[21], raw[22], raw[23], raw[24]
            w = ((b2 & 0x3F) << 8 | b1) + 1
            h = ((b4 & 0x0F) << 10 | b3 << 2 | (b2 & 0xC0) >> 6) + 1
            return w, h
        if chunk == b"VP8X":
            if len(raw) < 30:
                return None
            w = (raw[24] | raw[25] << 8 | raw[26] << 16) + 1
            h = (raw[27] | raw[28] << 8 | raw[29] << 16) + 1
            return w, h
    return None


__all__ = [
    "_safe_basename", "_resolve_save_dir",
    "_validate_image_bytes", "_validate_image_path",
    "_png_color_type", "_validate_mask_against_image",
    "_default_basename", "_detect_actual_size",
]
