"""env 配置 + 模型/size/limit/retry 等所有顶层常量。

server.py 顶部 from .config import * 让原代码引用方式不变。
"""
from __future__ import annotations

import os
import re
from pathlib import Path


# 跨进程文件锁后端检测（POSIX fcntl / Windows msvcrt）
_LOCK_BACKEND: str  # "posix" | "windows" | "none"
try:
    import fcntl  # type: ignore[import-untyped]  # noqa: F401
    _LOCK_BACKEND = "posix"
except ImportError:
    try:
        import msvcrt  # type: ignore[import-untyped]  # noqa: F401
        _LOCK_BACKEND = "windows"
    except ImportError:
        _LOCK_BACKEND = "none"
_FILE_LOCK_AVAILABLE = _LOCK_BACKEND != "none"

# ---------- 配置（env 可覆盖）----------
DEFAULT_BASEURL = os.environ.get("MICU_BASEURL", "https://www.micuapi.ai")
API_KEY = os.environ.get("MICU_API_KEY", "")
DEFAULT_MODEL = os.environ.get("MICU_MODEL", "gpt-image-2")
GROK_BASEURL = os.environ.get("MICU_GROK_BASEURL", DEFAULT_BASEURL)
GROK_API_KEY = os.environ.get(
    "MICU_GROK_API_KEY",
    os.environ.get("XAI_API_KEY", os.environ.get("GROK_API_KEY", "")),
)
XAI_MODEL = os.environ.get("XAI_MODEL", os.environ.get("GROK_MODEL", "grok-imagine-image-lite"))
GROK_SIZE_MODE = os.environ.get("MICU_GROK_SIZE_MODE", "contain").strip().lower()
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
GROK_MODEL_ALIASES = {
    "grok-imagine-image",
    "grok-imagine-image-lite",
    "grok-imagine-image-quality",
    "grok-imagine-image-quality-20260403",
    "grok-imagine-image-quality-latest",
    "grok-imagine-image-pro",
    "grok-imagine-image-edit",
}
GROK_AVAILABLE_MODELS = [
    "grok-imagine-image-lite",
    "grok-imagine-image",
    "grok-imagine-image-pro",
    "grok-imagine-image-edit",
]
GROK_ASPECT_RATIO_CHOICES = {
    "1:1": 1.0,
    "16:9": 16 / 9,
    "9:16": 9 / 16,
    "4:3": 4 / 3,
    "3:4": 3 / 4,
    "3:2": 3 / 2,
    "2:3": 2 / 3,
    "2:1": 2 / 1,
    "1:2": 1 / 2,
    "19.5:9": 19.5 / 9,
    "9:19.5": 9 / 19.5,
    "20:9": 20 / 9,
    "9:20": 9 / 20,
    "auto": 1.0,
}
GROK_SIZE_MODES = {"backend", "contain", "cover", "stretch"}

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

# ---------- retry 策略常量（http_client.py 用，集中放这避免循环依赖）----------
RETRYABLE_STATUS = (0, 408, 409, 425, 429, 500, 502, 503, 504, 520, 521, 522, 523, 524, 525, 527)
RETRY_AFTER_STATUSES = {408, 409, 425, 429, 500, 502, 503, 504}
BIG_SIZE_FAIL_FAST_STATUS = {524}
MAX_RETRY_AFTER_SECONDS = 120.0
NETWORK_RETRY_DELAY_SECONDS = 2.0
SMALL_RETRY_DELAYS_SECONDS = (4.0, 8.0)
BIG_RETRY_DELAY_SECONDS = 60.0
RETRY_JITTER_SECONDS = 2.0

__all__ = [
    "_LOCK_BACKEND", "_FILE_LOCK_AVAILABLE",
    "DEFAULT_BASEURL", "API_KEY", "DEFAULT_MODEL",
    "GROK_BASEURL", "GROK_API_KEY", "XAI_MODEL", "GROK_SIZE_MODE",
    "_TRUST_ENV", "_SAVE_ROOT", "DEFAULT_SAVE_DIR",
    "PRO_MODEL", "NONPRO_MODEL",
    "GROK_MODEL_ALIASES", "GROK_AVAILABLE_MODELS",
    "GROK_ASPECT_RATIO_CHOICES", "GROK_SIZE_MODES",
    "HIGH_RES_EDGE", "EDITS_MAX_EDGE",
    "VALID_SIZES_1K", "VALID_SIZES_2K", "VALID_SIZES_4K",
    "MAX_N", "MIN_SIZE_EDGE", "MAX_SIZE_EDGE", "SIZE_ALIGNMENT",
    "MAX_INPUT_FILE_BYTES", "MAX_TOTAL_INPUT_BYTES", "MAX_RESPONSE_BYTES",
    "_SAFE_BASENAME_RE",
    "RETRYABLE_STATUS", "RETRY_AFTER_STATUSES", "BIG_SIZE_FAIL_FAST_STATUS",
    "MAX_RETRY_AFTER_SECONDS", "NETWORK_RETRY_DELAY_SECONDS",
    "SMALL_RETRY_DELAYS_SECONDS", "BIG_RETRY_DELAY_SECONDS", "RETRY_JITTER_SECONDS",
]
