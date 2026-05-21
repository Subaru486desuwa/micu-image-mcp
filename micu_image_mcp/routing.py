"""model 路由 / 端点选择 / aspect-ratio 映射 / prompt 关键字推断 / size note。"""
from __future__ import annotations

import re

from .config import (
    DEFAULT_MODEL, PRO_MODEL,
    GROK_MODEL_ALIASES, GROK_ASPECT_RATIO_CHOICES,
    HIGH_RES_EDGE,
)
from .sizes import _parse_size, _max_edge, _size_tier, _round_to_alignment


def _is_grok_model(model: str | None) -> bool:
    if not model:
        return False
    m = model.strip().lower()
    return m in GROK_MODEL_ALIASES or m.startswith("grok-")


def _reject_4k_with_reference(size: str, tool: str) -> str | None:
    """≥4K image_edit / image_multi_reference 在米醋后端稳定 > 120s，撞 CF Proxy Read Timeout (524)；入口直接拒。

    image_generate 4K 是无参考的纯文生图，~50-80s 能过，不在此拦截范围。
    """
    if _size_tier(size) != "4k":
        return None
    return (
        f"size={size} (4K) 在 {tool} 已禁用：origin 处理 4K + 参考图稳定 > 120s，"
        f"撞 Cloudflare Proxy Read Timeout 物理上限。请改用 2K："
        f'横屏 "2048x1152" / 竖屏 "1152x2048" / 方形 "2048x2048"。'
        f'若必须 4K，可两步法：先 1K/2K 出综合图 → 再用 image_generate(size="3840x2160") '
        f"描述同场景升 4K（人物 ID 不保证一致）。"
    )


def _resolve_model(requested_model: str | None, size: str) -> tuple[str, list[str]]:
    """根据 size 自动选 model；返回 (effective_model, notes)."""
    notes: list[str] = []
    tier = _size_tier(size)
    model = requested_model or DEFAULT_MODEL
    if _is_grok_model(model):
        return model, notes
    if tier in ("2k", "4k") and "pro" not in model.lower():
        notes.append(f"size={size} ({tier}) 仅 pro 支持，已自动切到 {PRO_MODEL}")
        model = PRO_MODEL
    return model, notes


def _bypass_edits(model: str, size: str) -> bool:
    """pro + ≥1600 边长，图生图必须绕开 /v1/images/edits（代理会压回 1.57MP）."""
    return "pro" in model.lower() and _max_edge(size) >= HIGH_RES_EDGE


def _size_note(requested: str, actual: tuple[int, int] | None) -> str | None:
    if not actual:
        return None
    p = _parse_size(requested)
    if not p:
        return None
    rw, rh = p
    aw, ah = actual
    if (aw, ah) == (rw, rh):
        return None
    rmp = rw * rh / 1_000_000
    amp = aw * ah / 1_000_000
    # origin 把所有 ≤2.25MP 的请求统一处理到 ~1.57MP（看请求大小是放大还是压缩）
    if rmp <= 2.25 and 1.3 <= amp <= 1.8:
        if rmp <= 1.57:
            return (
                f"ℹ 实际 {aw}×{ah} ({amp:.2f}MP) > 请求 {rw}×{rh} ({rmp:.2f}MP)：米醋对 ≤2.25MP 的请求等比放大到 ~1.57MP（福利档）。"
            )
        return (
            f"⚠ 实际 {aw}×{ah} ({amp:.2f}MP) < 请求 {rw}×{rh} ({rmp:.2f}MP)：米醋对 ≤2.25MP 的请求统一压到 ~1.57MP（福利档降级）。"
            f"想拿到真分辨率请改用 ≥4MP 的 size（如 2048×1152、1152×2048、2048×2048、3840×2160）。"
        )
    return (
        f"⚠ 实际 {aw}×{ah} ({amp:.2f}MP) ≠ 请求 {rw}×{rh} ({rmp:.2f}MP)；如非 chat 路径请检查模型与 size 是否匹配。"
    )


def _grok_aspect_ratio(size: str) -> str:
    p = _parse_size(size)
    if not p:
        return "1:1"
    w, h = p
    ratio = w / h
    best_name = "1:1"
    best_delta = float("inf")
    for name, value in GROK_ASPECT_RATIO_CHOICES.items():
        delta = abs(ratio - value)
        if delta < best_delta:
            best_delta = delta
            best_name = name
    return best_name


def _grok_resolution(size: str) -> str:
    return "2k" if _max_edge(size) >= HIGH_RES_EDGE else "1k"


def _infer_size_from_prompt(prompt: str) -> tuple[str, str] | None:
    """从 prompt 关键字推断 size。返回 (size_str, reason) 或 None（推断失败）。

    优先级：明确像素 > K 缩写 > aspect 关键字 > 默认。
    弱 LLM 兜底用；强 LLM 一般直接传 size 不走这里。
    """
    p = prompt.lower()

    # 1) 明确像素 "1920x1080" / "1920×1080" / "3840 x 2160"
    m = re.search(r"(\d{3,4})\s*[x×]\s*(\d{3,4})", p)
    if m:
        w, h = int(m.group(1)), int(m.group(2))
        w16, h16 = _round_to_alignment(w), _round_to_alignment(h)
        if w16 != w or h16 != h:
            return f"{w16}x{h16}", f"prompt 含像素 {w}x{h}，对齐 8 倍数为 {w16}x{h16}"
        return f"{w16}x{h16}", f"prompt 含明确像素 {w}x{h}"

    # 2) aspect 与朝向
    vertical_kw = ("9:16", "竖屏", "竖版", "vertical", "portrait", "phone wallpaper",
                   "tiktok", "reels", "stories", "手机壁纸")
    horizontal_kw = ("16:9", "横屏", "横版", "landscape", "widescreen", "desktop wallpaper",
                     "wallpaper", "壁纸", "banner", "封面", "cover")
    square_kw = ("正方形", "square", "avatar", "头像", "icon", "logo", "profile pic",
                 "头像图", "图标")
    poster_kw = ("poster", "海报", "2:3", "movie poster")
    photo32_kw = ("3:2", "photograph", "照片")

    is_vert = any(k in p for k in vertical_kw)
    is_horiz = any(k in p for k in horizontal_kw)
    is_square = any(k in p for k in square_kw)
    is_poster = any(k in p for k in poster_kw)
    is_photo32 = any(k in p for k in photo32_kw)

    # 3) K 缩写（这些是 ≥2K 档，pro 模型，严格 1:1）
    if re.search(r"\b4k\b|uhd|ultra[\s-]?hd|超高清", p):
        return ("2160x3840", "prompt 含 4K 关键字 + 竖屏") if is_vert else \
               ("3840x2160", "prompt 含 4K 关键字（默认横屏）")
    if re.search(r"\b2k\b|1080p|full[\s-]?hd|\bfhd\b", p):
        # 不选 1920×1080 / 1080×1920：≤2.25MP 会被 origin 压到 ~1.57MP；2048×1152 跨 2.25MP 阈值拿到真分辨率
        return ("1152x2048", "prompt 含 2K/1080p 关键字 + 竖屏（用 1152×2048 跨 2.25MP 阈值，避开福利档降级）") if is_vert else \
               ("2048x1152", "prompt 含 2K/1080p 关键字（默认横屏；用 2048×1152 跨 2.25MP 阈值，避开福利档降级）")
    if re.search(r"720p|\bhd\b", p):
        return ("720x1280", "prompt 含 720p 关键字 + 竖屏") if is_vert else \
               ("1280x720", "prompt 含 720p 关键字")

    # 4) 形状关键字（1K 档）
    if is_square:
        return "1024x1024", "prompt 含正方形/logo/头像关键字"
    if is_poster:
        return "1024x1536", "prompt 含海报/2:3 关键字"
    if is_photo32:
        return "1536x1024", "prompt 含照片/3:2 关键字"
    if is_vert:
        return "1024x1536", "prompt 含竖屏关键字（1K 默认）"
    if is_horiz:
        return "1536x1024", "prompt 含横屏关键字（1K 默认）"

    return None


__all__ = [
    "_is_grok_model", "_reject_4k_with_reference",
    "_resolve_model", "_bypass_edits",
    "_size_note", "_grok_aspect_ratio", "_grok_resolution",
    "_infer_size_from_prompt",
]
