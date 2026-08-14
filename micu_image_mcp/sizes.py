"""size 数值校验 / 解析 / 档位划分 / 对齐。"""
from __future__ import annotations

import re

from .config import (
    GROK_SIZE_MODE, GROK_SIZE_MODES,
    MAX_N, MIN_SIZE_EDGE, MAX_SIZE_EDGE, SIZE_ALIGNMENT,
    MIN_IMAGE_PIXELS, MAX_IMAGE_PIXELS, MAX_IMAGE_ASPECT_RATIO,
    VALID_IMAGE_QUALITIES,
)


def _parse_size(size: str) -> tuple[int, int] | None:
    m = re.match(r"^(\d+)x(\d+)$", size.strip().lower())
    return (int(m.group(1)), int(m.group(2))) if m else None


def _max_edge(size: str) -> int:
    p = _parse_size(size)
    return max(p) if p else 0


def _size_tier(size: str) -> str:
    e = _max_edge(size)
    if e == 0:
        return "unknown"
    if e < 1024:
        return "small"
    if e < 1600:
        return "1k"
    if e < 3000:
        return "2k"
    return "4k"


def _grok_size_mode() -> str:
    """Grok 后端不保证精确 WxH；这里控制保存前的本地尺寸归一化。"""
    if GROK_SIZE_MODE in GROK_SIZE_MODES:
        return GROK_SIZE_MODE
    return "contain"


# ---------- validation helpers（GPT 审查 + 用户实测发现的 bug 修复）----------

def _validate_size(size: str | None, *, allow_none: bool = True) -> tuple[str | None, str | None]:
    """校验 size 字段。返回 (cleaned_size, error_message)；error 非 None 表示拒绝。

    规则：
      - None 允许（image_generate 走 prompt 推断兜底）
      - 必须形如 "WxH"，W/H 都为正整数
      - W/H 都在 [256, 3840]
      - W/H 必须是 16 的倍数
      - 长宽比不超过 3:1
      - 总像素在 [655,360, 8,294,400]
    """
    if size is None:
        if allow_none:
            return None, None
        return None, "size 不能为 None（此 tool 必须传明确 size）"
    if not isinstance(size, str):
        return None, f"size 必须是字符串，收到 {type(size).__name__}"
    s = size.strip().lower()
    m = re.match(r"^(\d+)x(\d+)$", s)
    if not m:
        return None, f"size 格式错误：必须是 'WxH'（如 '1024x1024'），收到 {size!r}"
    w, h = int(m.group(1)), int(m.group(2))
    if w <= 0 or h <= 0:
        return None, f"size W/H 必须为正数，收到 {size}"
    if w < MIN_SIZE_EDGE or h < MIN_SIZE_EDGE:
        return None, f"size 边长太小（最小 {MIN_SIZE_EDGE}），收到 {size}"
    if w > MAX_SIZE_EDGE or h > MAX_SIZE_EDGE:
        return None, f"size 边长太大（最大 {MAX_SIZE_EDGE}），收到 {size}"
    if w % SIZE_ALIGNMENT != 0 or h % SIZE_ALIGNMENT != 0:
        return None, f"size W/H 必须是 {SIZE_ALIGNMENT} 的倍数，收到 {size}"
    ratio = max(w, h) / min(w, h)
    if ratio > MAX_IMAGE_ASPECT_RATIO:
        return None, f"size 长宽比不能超过 {MAX_IMAGE_ASPECT_RATIO:g}:1，收到 {size}"
    pixels = w * h
    if pixels < MIN_IMAGE_PIXELS:
        return None, f"size 总像素太少（最小 {MIN_IMAGE_PIXELS:,}），收到 {size}"
    if pixels > MAX_IMAGE_PIXELS:
        return None, f"size 总像素太多（最大 {MAX_IMAGE_PIXELS:,}），收到 {size}"
    return f"{w}x{h}", None


def _validate_quality(quality: str | None) -> tuple[str | None, str | None]:
    """Validate the GPT Image 2 quality enum without silently dropping bad types."""
    if quality is None:
        return None, None
    if not isinstance(quality, str):
        return None, f"quality 必须是字符串，收到 {type(quality).__name__}"
    cleaned = quality.strip().lower()
    if not cleaned:
        return None, None
    if cleaned not in VALID_IMAGE_QUALITIES:
        choices = " / ".join(sorted(VALID_IMAGE_QUALITIES))
        return None, f"quality 不支持 {quality!r}；可选 {choices}"
    return cleaned, None


def _validate_grok_size(size: str | None, *, allow_none: bool = True) -> tuple[str | None, str | None]:
    """Grok 路径只做格式校验，不套用 Image2 的对齐、像素数和长宽比约束。"""
    if size is None:
        if allow_none:
            return None, None
        return None, "size 不能为 None（此 tool 必须传明确 size）"
    if not isinstance(size, str):
        return None, f"size 必须是字符串，收到 {type(size).__name__}"
    s = size.strip().lower()
    m = re.match(r"^(\d+)x(\d+)$", s)
    if not m:
        return None, f"size 格式错误：必须是 'WxH'（如 '1024x1024'），收到 {size!r}"
    w, h = int(m.group(1)), int(m.group(2))
    if w <= 0 or h <= 0:
        return None, f"size W/H 必须为正数，收到 {size}"
    return f"{w}x{h}", None


def _validate_n(n: int) -> str | None:
    """校验张数。返回 None 表示合法，否则返回错误描述。"""
    if not isinstance(n, int) or isinstance(n, bool):
        return f"n 必须是整数，收到 {type(n).__name__}"
    if n < 1:
        return f"n 必须 ≥ 1，收到 {n}"
    if n > MAX_N:
        return f"n 必须 ≤ {MAX_N}，收到 {n}（防止意外 burn quota）"
    return None


def _round_to_alignment(n: int) -> int:
    """Round an inferred edge to the current 16-pixel API alignment."""
    return max(SIZE_ALIGNMENT, round(n / SIZE_ALIGNMENT) * SIZE_ALIGNMENT)


def _parse_actual(s: str | None) -> tuple[int, int] | None:
    if not s:
        return None
    m = re.match(r"^(\d+)x(\d+)$", s)
    return (int(m.group(1)), int(m.group(2))) if m else None


__all__ = [
    "_parse_size", "_max_edge", "_size_tier", "_grok_size_mode",
    "_validate_size", "_validate_grok_size", "_validate_n", "_validate_quality",
    "_round_to_alignment", "_parse_actual",
]
