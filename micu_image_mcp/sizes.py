"""size 数值校验 / 解析 / 档位划分 / 对齐。"""
from __future__ import annotations

import re

from .config import (
    GROK_SIZE_MODE, GROK_SIZE_MODES,
    MAX_N, MIN_SIZE_EDGE, MAX_SIZE_EDGE, SIZE_ALIGNMENT,
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
      - W/H 都在 [256, 4096]
      - W/H 必须是 8 的倍数（米醋实测约束）
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
        return None, f"size W/H 必须是 {SIZE_ALIGNMENT} 的倍数（米醋代理约束），收到 {size}"
    return f"{w}x{h}", None


def _validate_grok_size(size: str | None, *, allow_none: bool = True) -> tuple[str | None, str | None]:
    """Grok 路径只做格式校验，不套用 MICU 的 8 倍数和 4K 约束。"""
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
    """米醋代理实测 W/H 接受 8 的倍数（1080/720 等通过）。

    OpenAI 官方文档说 16 倍数，但米醋代理更宽容；用 8 对齐既兼容常见视频尺寸（1920x1080 / 720）
    又不会过度修正用户意图（不会把 1080 改成 1088）。
    """
    return max(16, round(n / 8) * 8)


def _parse_actual(s: str | None) -> tuple[int, int] | None:
    if not s:
        return None
    m = re.match(r"^(\d+)x(\d+)$", s)
    return (int(m.group(1)), int(m.group(2))) if m else None


__all__ = [
    "_parse_size", "_max_edge", "_size_tier", "_grok_size_mode",
    "_validate_size", "_validate_grok_size", "_validate_n",
    "_round_to_alignment", "_parse_actual",
]
