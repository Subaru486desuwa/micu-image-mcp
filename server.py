"""米醋 gpt-image-2 MCP server.

把 米醋画图 网页里跑通的代理路由策略移植到 MCP，让 Claude Code 直接调起来。
关键策略沿用米醋画图网页版（gpt-image2 工具）实测得到的路由 / 重试 / 限流规则。
"""
from __future__ import annotations

import asyncio
import base64
import json
import os
import random
import re
import time
from contextlib import asynccontextmanager
from dataclasses import dataclass
from email.utils import parsedate_to_datetime
from pathlib import Path
from typing import Any

import httpx
from mcp.server.fastmcp import FastMCP

# 跨进程文件锁：POSIX 用 fcntl.flock；Windows 用 msvcrt.locking。
# 都是 stdlib 0 依赖；进程崩溃 → 内核关 fd → 锁自动释放，不留死锁。
_LOCK_BACKEND: str  # "posix" | "windows" | "none"
try:
    import fcntl  # type: ignore[import-untyped]
    _LOCK_BACKEND = "posix"
except ImportError:
    try:
        import msvcrt  # type: ignore[import-untyped]
        _LOCK_BACKEND = "windows"
    except ImportError:
        _LOCK_BACKEND = "none"
_FILE_LOCK_AVAILABLE = _LOCK_BACKEND != "none"

# ---------- 配置（env 可覆盖）----------
DEFAULT_BASEURL = os.environ.get("MICU_BASEURL", "https://www.micuapi.ai")
API_KEY = os.environ.get("MICU_API_KEY", "")
DEFAULT_MODEL = os.environ.get("MICU_MODEL", "gpt-image-2")
# 米醋是国内站，不应走 shell 的 SOCKS/HTTP 代理；默认 trust_env=False。
# 设 MICU_USE_SHELL_PROXY=1 才让 httpx 拾取 HTTPS_PROXY/HTTP_PROXY/ALL_PROXY。
_TRUST_ENV = os.environ.get("MICU_USE_SHELL_PROXY", "").strip() in ("1", "true", "yes")

# save_dir 的安全根目录：tool 调用方无论传什么 save_dir，都不能写到此根之外。
# 默认 = 用户家目录下的 Pictures/micu-out；可用 MICU_SAVE_DIR_ROOT 覆盖。
_SAVE_ROOT = Path(os.environ.get(
    "MICU_SAVE_DIR_ROOT",
    str(Path.home() / "Pictures" / "micu-out"),
)).expanduser().resolve()

# DEFAULT_SAVE_DIR 必须默认与 _SAVE_ROOT 一致，否则手动起 server（不走 install.py）
# 时会触发 _resolve_save_dir 把 cwd/out 重定向到 _SAVE_ROOT，对用户是静默的坑。
DEFAULT_SAVE_DIR = Path(os.environ.get("MICU_SAVE_DIR", str(_SAVE_ROOT)))

PRO_MODEL = "gpt-image-2-pro"
NONPRO_MODEL = "gpt-image-2"

# 网页里实测出的阈值：max edge ≥1600 视为 2K/4K，必须走 pro，且图生图绕开 /v1/images/edits
HIGH_RES_EDGE = 1600
# 图生图代理后端实测：≥2K 全部 503/524，仅 1K 可用
EDITS_MAX_EDGE = 1536

VALID_SIZES_1K = {"1024x1024", "1280x720", "720x1280", "1024x1536", "1536x1024"}
# 注意：1920×1080 / 1080×1920 (2.07MP) 名义上 2K，但 ≤2.25MP 会被 origin 压到 ~1.57MP，
# 不列入"严格 1:1"推荐。想要真 2K 横屏请用 2048×1152。
VALID_SIZES_2K = {"2048x2048", "2048x1152", "1152x2048"}
VALID_SIZES_4K = {"3840x2160", "2160x3840"}

# 大小限制
MAX_N = 10
MIN_SIZE_EDGE = 256
MAX_SIZE_EDGE = 4096
SIZE_ALIGNMENT = 8  # 米醋实测接受 8 倍数（1080/720 通过，1500 等非 8 倍 400）

MAX_INPUT_FILE_BYTES = 4 * 1024 * 1024     # 单张输入图 4MB
MAX_TOTAL_INPUT_BYTES = 8 * 1024 * 1024    # 多图总和 8MB（base64 后约 11MB，逼近代理上限）
MAX_RESPONSE_BYTES = 25 * 1024 * 1024      # 单张输出图最大 25MB（4K 实测最高 ~12MB）

# 安全 basename 字符集（保留点号给扩展名等）
_SAFE_BASENAME_RE = re.compile(r"^[A-Za-z0-9_\-.]+$")


# ---------- 工具函数 ----------

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
    if tier in ("2k", "4k") and "pro" not in model.lower():
        notes.append(f"size={size} ({tier}) 仅 pro 支持，已自动切到 {PRO_MODEL}")
        model = PRO_MODEL
    return model, notes


def _bypass_edits(model: str, size: str) -> bool:
    """pro + ≥1600 边长，图生图必须绕开 /v1/images/edits（代理会压回 1.57MP）."""
    return "pro" in model.lower() and _max_edge(size) >= HIGH_RES_EDGE


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


def _validate_n(n: int) -> str | None:
    """校验张数。返回 None 表示合法，否则返回错误描述。"""
    if not isinstance(n, int) or isinstance(n, bool):
        return f"n 必须是整数，收到 {type(n).__name__}"
    if n < 1:
        return f"n 必须 ≥ 1，收到 {n}"
    if n > MAX_N:
        return f"n 必须 ≤ {MAX_N}，收到 {n}（防止意外 burn quota）"
    return None


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
    return f"{label} 不是 PNG/JPEG/WebP/GIF（前 16 字节: {raw[:16]!r}）"


def _validate_image_path(image_path: str, label: str = "image_path") -> tuple[Path, bytes, str, str | None]:
    """读图 + 校验（大小 + magic）。返回 (path, bytes, mime, error_message)。
    error 非 None 时其他字段不可用。
    """
    err: str | None = None
    p = Path(image_path).expanduser()
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
            w = int.from_bytes(raw[26:28], "little") & 0x3FFF
            h = int.from_bytes(raw[28:30], "little") & 0x3FFF
            return w, h
        if chunk == b"VP8L":
            b1, b2, b3, b4 = raw[21], raw[22], raw[23], raw[24]
            w = ((b2 & 0x3F) << 8 | b1) + 1
            h = ((b4 & 0x0F) << 10 | b3 << 2 | (b2 & 0xC0) >> 6) + 1
            return w, h
        if chunk == b"VP8X":
            w = (raw[24] | raw[25] << 8 | raw[26] << 16) + 1
            h = (raw[27] | raw[28] << 8 | raw[29] << 16) + 1
            return w, h
    return None


class ImageSaveError(Exception):
    """落盘前校验失败（响应过大 / 不是合法图片 / 路径越界）。"""


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
    # 防覆盖：基础路径已存在则追加 _2 _3 …
    path = save_dir / f"{basename}.{ext}"
    counter = 2
    while path.exists():
        path = save_dir / f"{basename}_{counter}.{ext}"
        counter += 1
        if counter > 1000:
            raise ImageSaveError(f"basename 冲突过多：{basename}")
    # 安全确认：path 必须在 save_dir 之下
    try:
        path.resolve().relative_to(save_dir.resolve())
    except ValueError as e:
        raise ImageSaveError(f"落盘路径越界: {path}") from e
    await asyncio.to_thread(path.write_bytes, raw)
    return path, _detect_actual_size(raw), len(raw)


async def _save_image_b64(b64: str, save_dir: Path, basename: str) -> tuple[Path, tuple[int, int] | None, int]:
    try:
        # 大图 base64 解码（4K 16MB → 12MB）走 to_thread，避免 30-50ms 事件循环阻塞
        raw = await asyncio.to_thread(base64.b64decode, b64, validate=False)
    except Exception as e:  # noqa: BLE001
        raise ImageSaveError(f"base64 解码失败: {e}") from e
    return await _save_validated_bytes(raw, save_dir, basename, source_label="b64 响应")


async def _save_image_url(url: str, save_dir: Path, basename: str) -> tuple[Path, tuple[int, int] | None, int]:
    cx = _get_http_client()
    # 用 stream 提前读 Content-Length 拒掉超大响应
    async with cx.stream("GET", url, timeout=120.0) as r:
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
    return await _save_validated_bytes(raw, save_dir, basename, source_label=f"远端图 {url[:80]}")


def _round_to_alignment(n: int) -> int:
    """米醋代理实测 W/H 接受 8 的倍数（1080/720 等通过）。

    OpenAI 官方文档说 16 倍数，但米醋代理更宽容；用 8 对齐既兼容常见视频尺寸（1920x1080 / 720）
    又不会过度修正用户意图（不会把 1080 改成 1088）。
    """
    return max(16, round(n / 8) * 8)


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


def _parse_actual(s: str | None) -> tuple[int, int] | None:
    if not s:
        return None
    m = re.match(r"^(\d+)x(\d+)$", s)
    return (int(m.group(1)), int(m.group(2))) if m else None


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


def _extract_image_payload(resp: dict | str) -> tuple[str | None, str | None]:
    """从米醋响应里提取 (b64, url)；二者至少有一个。"""
    if isinstance(resp, str):
        return None, None
    # /v1/images/generations & /v1/images/edits 标准格式
    data = resp.get("data") if isinstance(resp, dict) else None
    if isinstance(data, list) and data:
        item = data[0]
        if isinstance(item, dict):
            if item.get("b64_json"):
                return item["b64_json"], None
            if item.get("url"):
                return None, item["url"]
    # /v1/chat/completions fallback：图嵌在 markdown ![](url) 或 base64
    choices = resp.get("choices") if isinstance(resp, dict) else None
    if isinstance(choices, list) and choices:
        msg = (choices[0] or {}).get("message", {})
        content = msg.get("content")
        if isinstance(content, str):
            m = re.search(r"!\[[^\]]*\]\((data:image/[^;]+;base64,([A-Za-z0-9+/=\s]+))\)", content)
            if m:
                return m.group(2).strip(), None
            m = re.search(r"!\[[^\]]*\]\((https?://[^)]+)\)", content)
            if m:
                return None, m.group(1)
            m = re.search(r"\b(https?://\S+\.(?:png|jpe?g|webp|gif))\b", content, re.I)
            if m:
                return None, m.group(1)
    return None, None


# ---------- HTTP 调用 + 重试 ----------

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

    timeout 为 None（不设默认），由 caller 每次 .post() / .stream() 通过 timeout= 覆盖；
    这样 generations(600s) 与 image url 下载(120s) 可共用同一池。
    """
    global _HTTP_CLIENT
    if _HTTP_CLIENT is None:
        _HTTP_CLIENT = httpx.AsyncClient(
            timeout=None,
            trust_env=_TRUST_ENV,
            limits=httpx.Limits(max_keepalive_connections=20, max_connections=20),
        )
    return _HTTP_CLIENT


async def _call_endpoint(ep: Endpoint, key: str, timeout: float = 600.0) -> tuple[int, str, dict[str, str]]:
    """非 stream 调用。timeout 拉到 600s 给慢 origin 留余地（CF 120s 仍可能拦）。"""
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
        r = await cx.post(ep.url, headers=headers, data=data, files=files, timeout=timeout)
    else:
        headers["Content-Type"] = "application/json"
        r = await cx.post(ep.url, headers=headers, content=json.dumps(ep.json_body), timeout=timeout)
    return r.status_code, r.text, {k.lower(): v for k, v in r.headers.items()}


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
    full_text_parts: list[str] = []
    final_status = 0
    last_finish: str | None = None
    cx = _get_http_client()
    try:
        async with cx.stream("POST", ep.url, headers=headers, content=json.dumps(body), timeout=timeout) as r:
            final_status = r.status_code
            response_headers = {k.lower(): v for k, v in r.headers.items()}
            if not (200 <= r.status_code < 300):
                err_text = (await r.aread()).decode("utf-8", errors="replace")
                return r.status_code, err_text, response_headers
            async for raw_line in r.aiter_lines():
                if not raw_line:
                    continue
                line = raw_line.strip()
                full_text_parts.append(line)
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
            "finish_reason": last_finish or "stop",
        }],
        "_stream_lines": len(full_text_parts),
    }
    return final_status or 200, json.dumps(fake_resp, ensure_ascii=False), response_headers


RETRYABLE_STATUS = (0, 408, 409, 425, 429, 500, 502, 503, 504, 520, 521, 522, 523, 524, 525, 527)
RETRY_AFTER_STATUSES = {408, 409, 425, 429, 500, 502, 503, 504}
BIG_SIZE_FAIL_FAST_STATUS = {524}
MAX_RETRY_AFTER_SECONDS = 120.0
NETWORK_RETRY_DELAY_SECONDS = 2.0
SMALL_RETRY_DELAYS_SECONDS = (4.0, 8.0)
BIG_RETRY_DELAY_SECONDS = 60.0
RETRY_JITTER_SECONDS = 2.0


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

# ≥2K 在米醋 origin 走 pro 模型串行队列，单张渲染 ~50-80s。
# 客户端并发 N 张时第 2 张就要排队等前一张，累积容易撞 CF 120s 硬上限 → 524 雪球。
#
# 双层锁：
#   - 进程内 Semaphore(1)：同 MCP 进程内的并发请求快速本地排队，零系统调用。
#   - 跨进程文件锁（POSIX flock）：多个 Claude Code / Codex 窗口各自 spawn 独立 MCP
#     子进程时，让所有进程串行打 origin。多窗口开发是常态（用户实测 5 进程并发就撞 524）。
# Lazy init：避免 module 导入期与 fastmcp event loop 不一致。
_BIG_SIZE_LOCK: asyncio.Semaphore | None = None
# 跨进程锁文件位置：固定 ~/.cache 下的 user-scoped 路径。
# 不用 tempfile.gettempdir() 是因为 Mac launchd 给 GUI 进程的 TMPDIR 与 terminal 进程不同
# (/var/folders/<hash>/T/ vs /tmp/...)，会让 GUI 启动的 Claude Code 与 terminal MCP 锁不同文件。
_BIG_SIZE_FILE_LOCK_PATH = Path.home() / ".cache" / "micu-image" / "bigsize.lock"


def _get_big_size_lock() -> asyncio.Semaphore:
    global _BIG_SIZE_LOCK
    if _BIG_SIZE_LOCK is None:
        _BIG_SIZE_LOCK = asyncio.Semaphore(1)
    return _BIG_SIZE_LOCK


def _acquire_big_size_file_lock_blocking() -> int:
    """阻塞获取系统级跨进程锁；返回 fd，关 fd 即释放。

    POSIX: fcntl.flock(LOCK_EX)，原生阻塞。
    Windows: msvcrt.locking(LK_LOCK, 1)，单次阻塞超时 10s，循环直到拿到。
    """
    _BIG_SIZE_FILE_LOCK_PATH.parent.mkdir(parents=True, exist_ok=True)
    if _LOCK_BACKEND == "posix":
        fd = os.open(str(_BIG_SIZE_FILE_LOCK_PATH), os.O_WRONLY | os.O_CREAT, 0o644)
        fcntl.flock(fd, fcntl.LOCK_EX)
        return fd
    if _LOCK_BACKEND == "windows":
        # msvcrt.locking 必须锁文件中实际存在的字节，先确保有 1 字节
        fd = os.open(str(_BIG_SIZE_FILE_LOCK_PATH), os.O_RDWR | os.O_CREAT, 0o644)
        try:
            if os.fstat(fd).st_size == 0:
                os.write(fd, b"\0")
            os.lseek(fd, 0, os.SEEK_SET)
            # LK_LOCK 单次阻塞 10s 后 raise；循环直到拿到
            while True:
                try:
                    msvcrt.locking(fd, msvcrt.LK_LOCK, 1)
                    return fd
                except OSError:
                    continue
        except Exception:
            os.close(fd)
            raise
    raise RuntimeError("file lock backend unavailable")


def _release_big_size_file_lock(fd: int) -> None:
    try:
        if _LOCK_BACKEND == "posix":
            fcntl.flock(fd, fcntl.LOCK_UN)
        elif _LOCK_BACKEND == "windows":
            os.lseek(fd, 0, os.SEEK_SET)
            try:
                msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)
            except OSError:
                pass
    finally:
        try:
            os.close(fd)
        except OSError:
            pass


@asynccontextmanager
async def _big_size_file_lock_async(notes_out: list[str] | None = None):
    """跨进程串行 ≥2K 请求。Windows 无 fcntl 时退化为 no-op（仅进程内 Semaphore 生效）。

    notes_out: 可选 list[str]，等锁 >2s 时附加排队 note 让多窗口排队对用户可见。
    """
    if not _FILE_LOCK_AVAILABLE:
        yield
        return
    t0 = time.monotonic()
    fd = await asyncio.to_thread(_acquire_big_size_file_lock_blocking)
    wait_s = time.monotonic() - t0
    if notes_out is not None and wait_s > 2.0:
        notes_out.append(
            f"等待跨进程 ≥2K 锁 {wait_s:.1f}s（其他 Claude Code / Codex 窗口同时在跑 ≥2K，已串行）"
        )
    try:
        yield
    finally:
        await asyncio.to_thread(_release_big_size_file_lock, fd)


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


def _parse_response(text: str) -> dict | str:
    try:
        return json.loads(text)
    except Exception:  # noqa: BLE001
        return text


def _error_detail(text: str) -> str:
    try:
        j = json.loads(text)
        if isinstance(j, dict):
            err = j.get("error")
            if isinstance(err, dict) and err.get("message"):
                return str(err["message"])[:400]
            if j.get("message"):
                return str(j["message"])[:400]
        return text[:400]
    except Exception:  # noqa: BLE001
        return (text or "")[:400]


# ---------- MCP 主体 ----------

mcp = FastMCP("micu-image")


def _get_key(override: str | None) -> str:
    key = (override or "").strip() or API_KEY
    if not key:
        raise RuntimeError(
            "未配置 API key。请设置 MICU_API_KEY 环境变量，或在调用时传 api_key 参数。"
        )
    return key


def _get_baseurl() -> str:
    """baseurl 锁在启动时的 env，运行期 tool 不接受覆盖（防 API key 外泄到攻击者 host）。"""
    return DEFAULT_BASEURL


@mcp.tool()
async def image_generate(
    prompt: str,
    size: str | None = None,
    n: int = 1,
    model: str | None = None,
    save_dir: str | None = None,
    basename: str | None = None,
    api_key: str | None = None,
) -> dict[str, Any]:
    """文本生成图像（text-to-image）。米醋代理 + gpt-image-2 系列。

    [WHAT] 把一段文字 prompt 渲染成 1 张或 N 张图像，落盘到本地。

    [WHEN TO USE]
      - 用户要"画 / 生成 / 创建一张图"且没有提供任何参考图 → 用此 tool。
      - 如果用户提供了 1 张参考图要"修改 / 编辑 / 替换某部分" → 改用 image_edit。
      - 如果用户提供了多张参考图要"按它们的风格画一张新的" → 暂未支持（image_multi_reference 路线），可用 image_edit 多次接力。
      - 如果不知道怎么选 size：先调 server_info() 看 recommended_sizes。

    [SIZE 选取建议]
      - 默认 None：MCP 自动从 prompt 关键字推断（4K/UHD → 3840x2160；1080p/2K → 2048x1152；
        正方形/logo/头像 → 1024x1024；竖屏/9:16 → 1024x1536；横屏/16:9 → 1536x1024 等）。
        推断不出来 fallback 1024x1024。
      - 强烈推荐：如果你（LLM）已经从用户消息读出确定的 size 偏好，**直接显式传 size**，比关键字推断准。
      - 用户提到"高清/4K/海报/壁纸" → "3840x2160"（横）或 "2160x3840"（竖），自动用 pro。
      - 用户提到"FullHD/1080p/横屏视频封面" → "2048x1152"（横）或 "1152x2048"（竖），跨过 2.25MP 阈值。
      - **pro 与非 pro 价格一致** —— 想要真分辨率请直接拉高 size，1920×1080 这种 ≤2.25MP 的会被压成 ~1.57MP。
      - W 与 H 必须都是 8 的倍数（米醋实测约束；OpenAI 官方要 16，米醋更宽容）。
      - ≤2.25MP 的请求都被代理压到 ~1.57MP；要真实分辨率必须 ≥4MP（即 2048² 或更大）。

    [PROMPT 写法建议]
      - 中英文混合可。gpt-image-2 文本渲染近完美，可大段嵌字（中英标点都行）。
      - 越具体越好：风格 / 视角 / 光线 / 主体 / 细节程度。

    Args:
        prompt: 图像描述。1-2000 字符。例："A minimalist sushi mascot logo, soft pastel palette".
        size: "WxH" 字符串或 None。**留 None 让 MCP 从 prompt 推**（弱 LLM 兜底用）；
              强 LLM 已知偏好时**直接显式传**更准。W 和 H 都必须是 8 的倍数（米醋约束）。常用：
              "1024x1024" "1280x720" "1024x1536" "1536x1024" "720x1280"        ← 1K 档（被压到 1.57MP）
              "1920x1080" "1080x1920"                                          ← 名义 2K 但 ≤2.25MP，被压到 1.57MP
              "2048x2048" "2048x1152" "1152x2048"                              ← 真 2K 档（仅 pro，≥4MP 严格 1:1）
              "3840x2160" "2160x3840"                                          ← 4K 档（仅 pro，严格 1:1）
              默认 None（推断后兜底 1024x1024）。
        n: 张数 1-10。1K 时 N>1 自动 5 并发；≥2K 强制 N=1（代理限流）。默认 1。
        model: 显式指定模型。留空时按 size 自动选（max edge ≥1600 用 pro，否则 non-pro）。
              可选值："gpt-image-2"（快、便宜）/ "gpt-image-2-pro"（高细节、≥2K 必需）。
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
        "size W/H 必须是 8 的倍数" → 客户端入口拒，改 size 即可（OpenAI 端有时返回"divisible by 16" 提示，米醋 8 倍数已能过）。
        "HTTP 524: timeout" → 已自动重试 3 次仍失败，建议改小 size 或稍后再试。
        "未配置 API key" → 设置 MICU_API_KEY 环境变量或传 api_key 参数。
    """
    key = _get_key(api_key)
    baseurl = _get_baseurl()

    # === 入口校验（一条条 return 错误，不再静默 ok=False）===
    if not isinstance(prompt, str) or not prompt.strip():
        return {"ok": False, "error": "prompt 不能为空", "errors": ["prompt 不能为空"]}
    err_n = _validate_n(n)
    if err_n:
        return {"ok": False, "error": err_n, "errors": [err_n]}
    safe_stem = _safe_basename(basename) if basename is not None else None
    if basename is not None and safe_stem is None:
        msg = f"basename {basename!r} 含非法字符或路径分量；仅允许 [A-Za-z0-9_-.]，禁含 / 与 .."
        return {"ok": False, "error": msg, "errors": [msg]}
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
    cleaned_size, size_err = _validate_size(size, allow_none=False)
    if size_err:
        return {"ok": False, "error": size_err, "errors": [size_err]}
    size = cleaned_size  # type: ignore[assignment]

    eff_model, notes = _resolve_model(model, size)
    if inferred_note:
        notes.insert(0, inferred_note)
    tier = _size_tier(size)
    if tier in ("2k", "4k") and n > 1:
        notes.append(f"{tier.upper()} 强制 N=1，已忽略请求的 n={n}")
        n = 1
    is_pro = "pro" in eff_model.lower()
    stem = safe_stem or _default_basename("gen")

    # 实测：generations 端点对所有 size 都尊重宽高比；
    #   - ≤2.25MP 请求被代理统一处理到 ~1.57MP（≤1.57MP 是等比放大福利；1.57~2.25MP 是压缩降级，如 1920×1080→1672×941）
    #   - ≥~4MP 请求严格 1:1 输出（pro 2048² → 真 2048²，4K 也是真 4K）
    # ≥2K 失败兜底：chat stream（size 不生效，输出 ~1.57MP），见下方 _do_one。
    # 1K 不需要兜底（generations 1K 路径稳定）。
    # CF 524 = origin 处理 >120s，60s 退避大概率仍 524（origin 持续慢），fail fast 直走 fallback。
    ep = Endpoint(
        url=f"{baseurl}/v1/images/generations",
        json_body={
            "model": eff_model,
            "prompt": prompt,
            "n": 1,
            "size": size,
            "response_format": "b64_json",
        },
    )

    saved: list[dict] = []
    errors: list[str] = []
    # 客户端循环 N 次单图请求（米醋 image_generation tool 不接受 n 字段）。
    # ≥2K 一律给 retry_pro=True 让 524/超时被重试（不仅 pro，size tier 也触发）
    aggressive_retry = is_pro or tier in ("2k", "4k")
    # 并发策略（与 image_batch_edit 对齐）：
    #   - 1K + non-pro + n>1 → 5 并发（HTML 网页同款）
    #   - 1K + pro / ≥2K → 串行（pro 代理瞬时限流多；≥2K 已强制 N=1）
    can_concurrent = n > 1 and tier in ("small", "1k") and not is_pro
    concurrency = 5 if can_concurrent else 1
    big_size_lock = tier in ("2k", "4k")

    async def _do_one(idx: int) -> tuple[int, dict | None, str | None]:
        status, text = await _call_with_retry(
            ep, key, retry_pro=aggressive_retry, stream=False,
            big_size_lock=big_size_lock, notes_out=notes,
        )
        # ≥2K 撞 524（origin t2i+pro+2K 路径间歇死）→ chat stream fallback。
        # 代价：chat 路径 size 不生效，输出固定 ~1.57MP；但比空手回好（已透明告知）。
        if not (200 <= status < 300) and big_size_lock and status in RETRYABLE_STATUS:
            chat_ep = Endpoint(
                url=f"{baseurl}/v1/chat/completions",
                json_body={
                    "model": eff_model,
                    "messages": [{"role": "user", "content": prompt}],
                    "size": size,  # 米醋接受但 chat 路径下不生效
                },
            )
            chat_status, chat_text = await _call_with_retry(
                chat_ep, key, retry_pro=is_pro, stream=True,
            )
            if 200 <= chat_status < 300:
                fb_note = f"generations 主路径 HTTP {status}（origin {size} 路径今晚拥塞）→ fallback chat stream（size 不生效，实际输出 ~1.57MP）"
                if fb_note not in notes:
                    notes.append(fb_note)
                status, text = chat_status, chat_text
        if not (200 <= status < 300):
            return idx, None, f"#{idx + 1} HTTP {status}: {_error_detail(text)}"
        resp = _parse_response(text)
        b64, url = _extract_image_payload(resp)
        try:
            if b64:
                p, actual, size_bytes = await _save_image_b64(b64, out_dir, f"{stem}_{idx + 1}")
            elif url:
                p, actual, size_bytes = await _save_image_url(url, out_dir, f"{stem}_{idx + 1}")
            else:
                return idx, None, f"#{idx + 1} 响应里未找到图片"
        except Exception as e:  # noqa: BLE001
            return idx, None, f"#{idx + 1} 保存失败: {e}"
        entry: dict[str, Any] = {
            "index": idx + 1,
            "path": str(p.resolve()),
            "size_bytes": size_bytes,
        }
        if actual:
            entry["actual_size"] = f"{actual[0]}x{actual[1]}"
            entry["actual_megapixels"] = round(actual[0] * actual[1] / 1_000_000, 2)
        return idx, entry, None

    if concurrency > 1:
        sem = asyncio.Semaphore(concurrency)

        async def _wrap(idx: int):
            async with sem:
                return await _do_one(idx)

        results = await asyncio.gather(*(_wrap(i) for i in range(n)))
        notes.append(f"1K + non-pro + N={n} 已 {concurrency} 并发")
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
    """图像编辑（image-to-image，单张输入，**支持 1K + 真 2K**；4K 已禁用见下）。

    [WHAT] 接受 1 张本地图片 + 修改指令，输出修改后的图。

    [WHEN TO USE]
      - 用户提供 1 张图（路径或刚刚生成的图）且要"改 / 替换 / 加 / 去掉某部分" → 用此 tool。
      - 如果用户没提供图想从零生成 → 改用 image_generate。
      - 如果用户提供了多张图想"批量改"（每张做同样操作）→ 改用 image_batch_edit。
      - 如果用户用多张图作风格参考想画一张新的 → 用 image_multi_reference（≤1.57MP）。

    [4K 已禁用]
      origin 处理 4K + 参考图稳定 > 120s，撞 CF Proxy Read Timeout (524)。
      入口直接拒 4K size，不发请求。请改 2K（"2048x1152" / "1152x2048" / "2048x2048"），
      或两步法：先 1K/2K image_edit → image_generate(size="3840x2160", 描述同场景) 升 4K。

    [路由实现]（实测确定，双路径）
      - 1K（边长 ≤1536）：走 /v1/images/edits multipart，**支持 alpha mask**。
        失败 fallback 到 /v1/chat/completions stream。
      - 2K（边长 1600–2999）：走 /v1/images/generations + 米醋扩展字段 reference_image=data_url。
        size 真实生效（实测 2048² 真 2048²）。
        **此路径不支持 mask**（米醋扩展字段不接受 mask）；如需 mask 请降到 1K。
      - 自动锁 pro：max edge ≥1600 → gpt-image-2-pro。

    [MASK 工作原理]（仅 1K 路径）
      - mask_path 指向一张 PNG，尺寸应与 image_path 一致。
      - mask 中 **alpha=0（透明）** 的像素 = 要修改的区域。
      - alpha=255（不透明）的像素 = 要保持原样。
      - 不传 mask 则模型自由决定改哪里。

    Args:
        prompt: 修改指令，越具体越好。例："change the background to deep navy with stars, keep the subject pixel-identical".
        image_path: 输入图的绝对或相对路径。PNG / JPG / WebP 都支持。
        mask_path: 可选 alpha mask PNG 路径，透明区即编辑区。仅 1K 路径生效；≥2K 时被忽略。
        size: 输出 size。
              "1024x1024" "1280x720" "1024x1536" "1536x1024" "720x1280"  ← 1K（被压到 1.57MP，含 mask 支持）
              "1920x1080" "1080x1920"                                    ← 名义 2K 但 ≤2.25MP，会被压到 1.57MP
              "2048x2048" "2048x1152" "1152x2048"                        ← 真 2K（≥4MP 严格 1:1，pro 自动）
              "3840x2160" / "2160x3840"  ← 4K 已禁用（撞 CF 524 物理上限），传入直接拒
              默认 "1024x1024"。
        model: "gpt-image-2"（默认）/ "gpt-image-2-pro"（≥2K 自动切）。
        save_dir: 输出目录（必须在安全根目录之下）。默认 ~/Pictures/micu-out 或 MICU_SAVE_DIR。
        basename: 文件名前缀（仅 [A-Za-z0-9_-.]）。默认 "edit_<ns_ts>"。
        api_key: 覆盖 MICU_API_KEY；base_url 已锁在启动期 env，运行期不接受。

    Returns: dict 含：
        ok (bool): 是否成功。
        model (str): 实际用的模型。
        size (str): 请求 size。
        used_fallback (bool): True 表示主端点失败已切换到 chat/completions（fallback 下 size 可能不生效）。
        saved (dict): { path, size_bytes, actual_size, actual_megapixels }。
        notes (list[str]): 决策与提示。

    Examples:
        # 1K 换背景
        image_edit(prompt="replace background with a sunset beach", image_path="/p/portrait.jpg")

        # 1K 局部修改（mask 生效）
        image_edit(prompt="change hair color to silver", image_path="/p/x.png", mask_path="/p/x_mask.png")

        # 真 2K 升级（无 mask）
        image_edit(prompt="enhance to cinematic detail, preserve composition", image_path="/p/draft.png", size="2048x2048")

    Common errors:
        "image_path 不存在" → 检查路径，建议用绝对路径。
        "size=3840x2160 (4K) 在 image_edit 已禁用" → 4K image_edit 物理撞 CF 524 上限，请改 2K 或两步法。
        "HTTP 524" → 2K 单图正常 ~50s，撞了说明 origin 那阵特别忙；自动重试仍失败请稍后再试。
    """
    key = _get_key(api_key)
    baseurl = _get_baseurl()

    # === 入口校验 ===
    if not isinstance(prompt, str) or not prompt.strip():
        return {"ok": False, "error": "prompt 不能为空", "errors": ["prompt 不能为空"]}
    cleaned_size, size_err = _validate_size(size, allow_none=False)
    if size_err:
        return {"ok": False, "error": size_err, "errors": [size_err]}
    size = cleaned_size  # type: ignore[assignment]
    if (rej := _reject_4k_with_reference(size, "image_edit")):
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
    edge = _max_edge(size)
    is_high_res = edge >= HIGH_RES_EDGE

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
        if is_high_res:
            notes.append("≥2K 路径走 generations + reference_image，不支持 alpha mask；mask 已忽略。")
            mask_bytes = None

    stem = safe_stem or _default_basename("edit")
    is_pro = "pro" in eff_model.lower()

    # 大图 base64 编码（4K 12MB → 16MB）走 to_thread，避免 30-50ms 事件循环阻塞
    img_b64 = await asyncio.to_thread(lambda: base64.b64encode(img_bytes).decode())
    img_data_url = f"data:{img_mime};base64,{img_b64}"
    used_fallback = False

    if is_high_res:
        # ≥2K 路径：走 generations + reference_image 米醋扩展字段（实测 size 真实生效）
        gen_ep = Endpoint(
            url=f"{baseurl}/v1/images/generations",
            json_body={
                "model": eff_model,
                "prompt": prompt,
                "n": 1,
                "size": size,
                "reference_image": img_data_url,
                "response_format": "b64_json",
            },
        )
        notes.append(f"≥2K 路径：/v1/images/generations + reference_image（size 真实生效，无 mask 支持）")
        status, text = await _call_with_retry(
            gen_ep, key, retry_pro=True, stream=False, big_size_lock=True, notes_out=notes,
        )
        if not (200 <= status < 300) and status in RETRYABLE_STATUS:
            used_fallback = True
            notes.append(
                f"generations + reference_image 主路径 HTTP {status}，已 fallback chat stream"
                f"（size 可能不生效，实际输出可能回落 ~1.57MP）"
            )
            size_directive = (
                f"Output the full edited image at exactly {size} pixels if supported by this route."
                if _parse_size(size)
                else "Output the full edited image, same dimensions as the input if supported by this route."
            )
            header = "Edit the attached image as described. " + size_directive + "\n\nInstruction:\n" + prompt
            chat_ep = Endpoint(
                url=f"{baseurl}/v1/chat/completions",
                json_body={
                    "model": eff_model,
                    "messages": [{
                        "role": "user",
                        "content": [
                            {"type": "text", "text": header},
                            {"type": "image_url", "image_url": {"url": img_data_url}},
                        ],
                    }],
                    "size": size,  # 米醋接受但 chat 路径下可能不生效
                },
            )
            status, text = await _call_with_retry(chat_ep, key, retry_pro=is_pro, stream=True)
    else:
        # 1K 路径：走 edits multipart（含 mask）→ 失败 fallback chat stream
        edits_form: dict[str, Any] = {
            "model": eff_model,
            "prompt": prompt,
            "size": size,
            "response_format": "b64_json",
            "image": (img_p.name, img_bytes, img_mime),
        }
        if mask_bytes:
            edits_form["mask"] = ("mask.png", mask_bytes, "image/png")
        edits_ep = Endpoint(url=f"{baseurl}/v1/images/edits", multipart=edits_form)

        # chat fallback：把图嵌成 data URL
        size_directive = (
            f"Output the full edited image at exactly {size} pixels."
            if _parse_size(size)
            else "Output the full edited image, same dimensions as the input."
        )
        header = "Edit the attached image as described. " + size_directive + "\n\nInstruction:\n" + prompt
        chat_content: list[dict] = [
            {"type": "text", "text": header},
            {"type": "image_url", "image_url": {"url": img_data_url}},
        ]
        if mask_bytes:
            mask_b64 = await asyncio.to_thread(lambda: base64.b64encode(mask_bytes).decode())
            mask_data_url = f"data:image/png;base64,{mask_b64}"
            chat_content.insert(0, {
                "type": "text",
                "text": (
                    "You are given two images: the FIRST is the original; the SECOND is the alpha mask "
                    "where transparent (alpha=0) pixels mark the ONLY region to modify. Pixels outside "
                    "the mask region must remain pixel-identical to the original."
                ),
            })
            chat_content.append({"type": "image_url", "image_url": {"url": mask_data_url}})
        chat_ep = Endpoint(
            url=f"{baseurl}/v1/chat/completions",
            json_body={"model": eff_model, "messages": [{"role": "user", "content": chat_content}]},
        )

        status, text = await _call_with_retry(edits_ep, key, retry_pro=is_pro, stream=False)
        # 只对可恢复错误 fallback；400/401/403/413 等用户/鉴权错误不降级，避免掩盖真因
        if not (200 <= status < 300) and status in RETRYABLE_STATUS:
            used_fallback = True
            notes.append(f"edits 端点 HTTP {status}，已切到 /v1/chat/completions stream")
            status, text = await _call_with_retry(chat_ep, key, retry_pro=is_pro, stream=True)

    if not (200 <= status < 300):
        return {
            "ok": False,
            "model": eff_model,
            "size": size,
            "error": f"HTTP {status}: {_error_detail(text)}",
            "notes": notes,
        }

    resp = _parse_response(text)
    b64, url = _extract_image_payload(resp)
    try:
        if b64:
            p, actual, size_bytes = await _save_image_b64(b64, out_dir, stem)
        elif url:
            p, actual, size_bytes = await _save_image_url(url, out_dir, stem)
        else:
            return {
                "ok": False,
                "error": "响应中未识别到图片",
                "raw_excerpt": (text[:500] if isinstance(text, str) else str(resp)[:500]),
                "notes": notes,
            }
    except Exception as e:  # noqa: BLE001
        return {"ok": False, "error": f"保存失败: {e}", "notes": notes}

    saved_info: dict[str, Any] = {"path": str(p.resolve()), "size_bytes": size_bytes}
    if actual:
        saved_info["actual_size"] = f"{actual[0]}x{actual[1]}"
        saved_info["actual_megapixels"] = round(actual[0] * actual[1] / 1_000_000, 2)
        sn = _size_note(size, actual)
        if sn and sn not in notes:
            notes.append(sn)

    return {
        "ok": True,
        "model": eff_model,
        "size": size,
        "used_fallback": used_fallback,
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
      - non-pro 模型：5 并发（HTML 网页同款）。
      - pro 模型：串行 + 1.5s gap（代理对 pro 并发会拒）。
      - 任意一张失败不影响其他张；返回 results 里逐张标 ok/error。

    [LIMITS]
      - 同 image_edit：size 仅 1K 档（≤1536 边长），≥2K 拒绝。
      - image_paths 长度建议 2-20 张；过多请分批调用避免超时。

    Args:
        prompt: 应用到每张图的修改指令。例："add a subtle watermark in bottom-right".
        image_paths: 输入图路径列表（绝对或相对）。
        size: 输出 size，仅 1K 档。默认 "1024x1024"。
        model: "gpt-image-2" / "gpt-image-2-pro"。留空按 size 自动选。
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
    key = _get_key(api_key)
    baseurl = _get_baseurl()

    # === 入口校验 ===
    if not isinstance(prompt, str) or not prompt.strip():
        return {"ok": False, "error": "prompt 不能为空", "errors": ["prompt 不能为空"], "total": 0}
    if not isinstance(image_paths, list) or len(image_paths) == 0:
        msg = "image_paths 必须是非空 list"
        return {"ok": False, "error": msg, "errors": [msg], "total": 0}
    if len(image_paths) > 20:
        msg = f"image_paths 最多 20 张，收到 {len(image_paths)} 张（防止意外 burn quota）"
        return {"ok": False, "error": msg, "errors": [msg], "total": len(image_paths)}
    cleaned_size, size_err = _validate_size(size, allow_none=False)
    if size_err:
        return {"ok": False, "error": size_err, "errors": [size_err], "total": len(image_paths)}
    size = cleaned_size  # type: ignore[assignment]
    out_dir, dir_err = _resolve_save_dir(save_dir)
    if dir_err:
        return {"ok": False, "error": dir_err, "errors": [dir_err], "total": len(image_paths)}

    eff_model, notes = _resolve_model(model, size)
    is_pro = "pro" in eff_model.lower()
    edge = _max_edge(size)
    if edge >= HIGH_RES_EDGE:
        msg = (
            f"图生图代理后端 ≥2K 不稳定（503/524）；批处理只支持 1K（边长 ≤{EDITS_MAX_EDGE}）。"
            f"请改 size 到 1K，或改用 image_edit 单图（自动 ≥2K 走 generations + reference_image）。"
        )
        return {"ok": False, "error": msg, "errors": [msg], "total": len(image_paths)}

    out_dir.mkdir(parents=True, exist_ok=True)
    # ≥2K 已在前面提前拒绝，这里 bypass 必然 False；只看 is_pro
    concurrency = 1 if is_pro else 5
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
                api_key=key,
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
    """多图融合参考 → 输出 1 张新图（支持 1K + 不稳的真 2K；4K 已禁用）。

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

    [路由实现]（双路径 + 自动 fallback）
      - 主路径：/v1/images/generations + image_urls=[...]（米醋扩展字段，size 真实生效）
      - 兜底：/v1/chat/completions + 顶层 image_urls + stream:true SSE（永不撞 CF 524，但 size 不生效输出 ~1.57MP）
      - 自动锁 pro：max edge ≥1600 → gpt-image-2-pro
      - 主路径 5xx/524 失败 → 自动 fallback chat stream，notes 标注降级原因
      - 返回的 used_fallback 字段说明走的哪条路径

    [LIMITS]（当前真实状态，会变化）
      - image_paths 长度 2-10 张。
      - **1K 稳定**：主路径 ~30-100s，size=1024² 实际输出 ~1.57MP。
      - **2K 不稳定**：主路径 generations + image_urls 在米醋后端间歇 HTTP 500"系统繁忙"；
        触发 fallback 后改走 chat stream，size 字段被忽略，**实际仍输出 ~1.57MP**。
      - 单张参考图建议 ≤2MB；总 base64 体积 ≤8MB（米醋代理上限实测约 10MB）。

    Args:
        prompt: 综合指令。例："combine the colors from img1 and the composition from img2 into a sunset cityscape".
        image_paths: 2-10 张参考图路径（绝对或相对）。
        size: 输出 size。**真实生效**（不再像旧版 chat 路径那样被忽略）。
              推荐："1024x1024"（1.57MP 福利）/ "2048x2048"（真 2K，可能 fallback 到 1.57MP）。
              "3840x2160" / "2160x3840" 已禁用（撞 CF 524 物理上限），传入直接拒。
              默认 "1024x1024"。
        model: "gpt-image-2"（默认）/ "gpt-image-2-pro"（≥2K 必需，自动切换）。
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

        # 2K 综合参考（不稳，可能 fallback 到 1.57MP）
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
    key = _get_key(api_key)
    baseurl = _get_baseurl()

    # === 入口校验 ===
    if not isinstance(prompt, str) or not prompt.strip():
        return {"ok": False, "error": "prompt 不能为空", "errors": ["prompt 不能为空"]}
    if not isinstance(image_paths, list) or len(image_paths) < 2:
        msg = f"至少需要 2 张参考图（收到 {len(image_paths) if isinstance(image_paths, list) else 'non-list'}）。1 张请用 image_edit；0 张请用 image_generate。"
        return {"ok": False, "error": msg, "errors": [msg]}
    if len(image_paths) > 10:
        msg = f"参考图最多 10 张，当前 {len(image_paths)} 张。请减少或分批。"
        return {"ok": False, "error": msg, "errors": [msg]}
    cleaned_size, size_err = _validate_size(size, allow_none=False)
    if size_err:
        return {"ok": False, "error": size_err, "errors": [size_err]}
    size = cleaned_size  # type: ignore[assignment]
    if (rej := _reject_4k_with_reference(size, "image_multi_reference")):
        return {"ok": False, "error": rej, "errors": [rej]}
    safe_stem = _safe_basename(basename) if basename is not None else None
    if basename is not None and safe_stem is None:
        msg = f"basename {basename!r} 含非法字符或路径分量"
        return {"ok": False, "error": msg, "errors": [msg]}
    out_dir, dir_err = _resolve_save_dir(save_dir)
    if dir_err:
        return {"ok": False, "error": dir_err, "errors": [dir_err]}

    eff_model, notes = _resolve_model(model, size)
    is_pro = "pro" in eff_model.lower()
    stem = safe_stem or _default_basename("multiref")

    # 加载所有图：每张大小 + magic 校验，再算总字节
    image_urls: list[str] = []
    total_bytes = 0
    for idx, p_str in enumerate(image_paths):
        ip, raw, mime, err = _validate_image_path(p_str, f"image_paths[{idx}]")
        if err:
            return {"ok": False, "error": err, "errors": [err]}
        total_bytes += len(raw)
        if total_bytes > MAX_TOTAL_INPUT_BYTES:
            msg = (
                f"参考图累计 {total_bytes/1024/1024:.1f}MB 超过总量上限 "
                f"{MAX_TOTAL_INPUT_BYTES/1024/1024:.0f}MB（base64 后会膨胀 33%）。请压缩或减少。"
            )
            return {"ok": False, "error": msg, "errors": [msg]}
        # 大图 base64 编码走 to_thread，避免多图累加时长时间阻塞事件循环
        ref_b64 = await asyncio.to_thread(lambda r=raw: base64.b64encode(r).decode())
        image_urls.append(f"data:{mime};base64,{ref_b64}")

    # base64 inflates ~33%
    inflated_mb = total_bytes * 1.33 / 1024 / 1024
    if inflated_mb > 4:
        notes.append(f"参考图体积估 {inflated_mb:.1f}MB，部分 serverless 代理可能拒收（一般 4MB 上限）")

    # 双路径 + fallback：
    #   主路径：/v1/images/generations + image_urls （可拿真 2K/4K，但米醋间歇 500/524）
    #   兜底：/v1/chat/completions + 顶层 image_urls + stream （永不撞 CF 524，但 size 不生效，输出固定 ~1.57MP）
    full_prompt = (
        f"Reference images are provided. Synthesize their visual elements (style, palette, "
        f"composition, subjects) into ONE single new image per the instruction below. "
        f"Do NOT collage, tile, or montage the references side-by-side unless explicitly asked.\n\n"
        f"Instruction:\n{prompt}"
    )
    gen_ep = Endpoint(
        url=f"{baseurl}/v1/images/generations",
        json_body={
            "model": eff_model,
            "prompt": full_prompt,
            "n": 1,
            "size": size,
            "image_urls": image_urls,
            "response_format": "b64_json",
        },
    )

    aggressive_retry = is_pro or _size_tier(size) in ("2k", "4k")
    big_size_lock = _size_tier(size) in ("2k", "4k")
    status, text = await _call_with_retry(
        gen_ep, key, retry_pro=aggressive_retry, stream=False,
        big_size_lock=big_size_lock, notes_out=notes,
    )

    used_fallback = False
    # 只对可恢复的错误 fallback；400/401/403/413 等用户错误直接返回，避免掩盖真因
    if not (200 <= status < 300) and status in RETRYABLE_STATUS:
        # generations 失败 → 走 chat stream 兜底
        notes.append(f"generations 主路径 HTTP {status}（米醋多图 + 高分辨率间歇拒），已 fallback chat stream（size 不生效，输出 ~1.57MP）")
        used_fallback = True
        chat_ep = Endpoint(
            url=f"{baseurl}/v1/chat/completions",
            json_body={
                "model": eff_model,
                "messages": [{"role": "user", "content": full_prompt}],
                "image_urls": image_urls,
                "size": size,  # 米醋接受但 chat 路径下不生效
            },
        )
        status, text = await _call_with_retry(chat_ep, key, retry_pro=is_pro, stream=True)

    if not (200 <= status < 300):
        return {
            "ok": False,
            "model": eff_model,
            "n_references": len(image_paths),
            "used_fallback": used_fallback,
            "error": f"HTTP {status}: {_error_detail(text)}",
            "notes": notes,
        }

    resp = _parse_response(text)
    b64, url = _extract_image_payload(resp)
    try:
        if b64:
            p, actual, size_bytes = await _save_image_b64(b64, out_dir, stem)
        elif url:
            p, actual, size_bytes = await _save_image_url(url, out_dir, stem)
        else:
            return {
                "ok": False,
                "error": "响应中未识别到图片",
                "raw_excerpt": text[:500] if isinstance(text, str) else str(resp)[:500],
                "notes": notes,
            }
    except Exception as e:  # noqa: BLE001
        return {"ok": False, "error": f"保存失败: {e}", "notes": notes}

    saved_info: dict[str, Any] = {"path": str(p.resolve()), "size_bytes": size_bytes}
    if actual:
        saved_info["actual_size"] = f"{actual[0]}x{actual[1]}"
        saved_info["actual_megapixels"] = round(actual[0] * actual[1] / 1_000_000, 2)
        sn = _size_note(size, actual)
        if sn and sn not in notes:
            notes.append(sn)

    return {
        "ok": True,
        "model": eff_model,
        "size": size,
        "used_fallback": used_fallback,
        "n_references": len(image_paths),
        "saved": saved_info,
        "notes": notes,
    }


@mcp.tool()
def server_info() -> dict[str, Any]:
    """诊断 / 能力查询：在调任何生图 tool 之前，先调一次此 tool 拿到完整路由规则与 size 约束矩阵。

    Returns:
        base_url, default_model, default_save_dir, api_key_configured: 当前配置。
        size_rules: size 字段的硬约束 + 代理实际行为（已通过实测确定）。
        recommended_sizes: 各 tier 推荐 size（保证 W/H 都是 8 的倍数，米醋约束）。
        capability_matrix: 各 tool × 各 size tier 的可用性。
        retry_policy: 重试与并发策略。
    """
    return {
        "base_url": DEFAULT_BASEURL,
        "default_model": DEFAULT_MODEL,
        "available_models": [NONPRO_MODEL, PRO_MODEL],
        "default_save_dir": str(DEFAULT_SAVE_DIR),
        "api_key_configured": bool(API_KEY),
        "size_rules": {
            "format": "WxH 字符串（如 '1024x1024'）",
            "alignment": f"W 与 H 都必须是 {SIZE_ALIGNMENT} 的整数倍（米醋实测约束，OpenAI 官方要 16）",
            "edge_range": f"W/H 必须在 [{MIN_SIZE_EDGE}, {MAX_SIZE_EDGE}] 范围内",
            "compress_below_2_25mp": (
                "请求总像素 ≤ 2.25MP（如 1024² / 1280×720 / 1500² / 1920×1080）会被代理"
                "等比放大或压缩到 ~1.57MP（福利档），实际输出 ≠ 请求 size。"
            ),
            "exact_above_4mp": (
                "请求总像素 ≥ 4MP（如 2048² / 3840×2160）严格按 size 1:1 输出。"
            ),
            "auto_pro_threshold": (
                f"max edge ≥ {HIGH_RES_EDGE} → 自动锁 {PRO_MODEL}（{NONPRO_MODEL} 在该档代理会拒）。"
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
            "1k_福利档_约1.57MP": sorted(VALID_SIZES_1K),
            "2k_仅pro_严格1_1": sorted(VALID_SIZES_2K),
            "4k_仅pro_严格1_1": sorted(VALID_SIZES_4K),
            "tip": "想拿到精确分辨率请选 2K/4K 档；选 1K 档会被代理统一拉到 1.57MP。",
        },
        "capability_matrix": {
            "image_generate": {
                "1k": "可用，single 30s，N>1 自动 5 并发",
                "2k_pro": "可用，single 40-60s，N=1 强制；origin 拥塞撞 524 时自动 fallback 到 chat stream（输出 ~1.57MP，notes 里有标记）",
                "4k_pro": "可用，single 50-80s，N=1 强制；偶尔 524 自动重试",
            },
            "image_edit": {
                "1k": "可用，~10s，edits multipart + 可选 alpha mask",
                "2k_pro": "可用，generations + reference_image 字段，~50s 真 1:1（不支持 mask）",
                "4k_pro": "已禁用：origin 处理 4K + 参考图稳定 > 120s 撞 CF Proxy Read Timeout (524)；入口直接拒。请改 2K 或两步法（1K/2K 出图 → image_generate 升 4K）",
            },
            "image_batch_edit": {
                "1k_non_pro": "5 并发",
                "1k_pro": "串行 + 1.5s gap",
                ">=2k": "拒绝",
            },
            "image_multi_reference": {
                "1k": "稳定可用，2-10 张参考图融合输出 1 张，~30-100s",
                "2k_pro": "可用但 origin 间歇 500（米醋 image_urls + ≥2K 状态不稳定），失败自动 fallback 到 chat stream 但 size 会被忽略输出 1.57MP",
                "4k_pro": "已禁用：origin 处理 4K 多图融合稳定 > 120s 撞 CF 524；入口直接拒。建议两步法：先 1K/2K 出综合图 → image_generate 升 4K",
            },
        },
        "retry_policy": {
            "retryable_status": list(RETRYABLE_STATUS),
            "retry_after": (
                f"HTTP {sorted(RETRY_AFTER_STATUSES)} 若返回 Retry-After，会优先按该值等待；"
                f"单次最多等待 {MAX_RETRY_AFTER_SECONDS:.0f}s，避免被异常 header 卡死"
            ),
            "schedule_1k": (
                "网络层异常先免费等待 2s 重试 1 次；之后可恢复错误按 "
                "4s + jitter、8s + jitter 最多再重试 2 次。每次重试会写入 notes"
            ),
            "schedule_2k_4k": (
                "双层锁内：网络层异常先免费等待 2s 重试 1 次；之后可恢复错误最多等待 60s "
                "重试 1 次。CF 524 fail fast 不重试（origin 已超过 Cloudflare 120s 上限），"
                "让 caller 尽快走 fallback。锁让整机任意时刻只有一个 ≥2K 请求打到 origin，"
                "避免多客户端并发 + origin pro 队列堆叠 → CF 524 雪球。锁等待 >2s 时 notes 会提示在排队"
            ),
            "trigger": "model 含 'pro' 或 size tier ∈ {2k, 4k}",
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
        },
    }


def main() -> None:
    mcp.run()


if __name__ == "__main__":
    main()
