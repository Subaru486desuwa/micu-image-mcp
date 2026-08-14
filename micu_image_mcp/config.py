"""env 配置 + 模型/size/limit/retry 等所有顶层常量。

server.py 顶部 from .config import * 让原代码引用方式不变。
"""
from __future__ import annotations

import ipaddress
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

# API 响应格式策略：
#   auto（默认）— 先 url 请求并下载落盘，失败再重试 API response_format=b64_json
#   url — 仅 url； b64_json — 仅 b64_json
_rf_raw = os.environ.get("MICU_RESPONSE_FORMAT", "auto").strip().lower()
if _rf_raw == "url":
    API_RESPONSE_FORMAT = "url"
    RESPONSE_FORMATS_TO_TRY: tuple[str, ...] = ("url",)
elif _rf_raw == "b64_json":
    API_RESPONSE_FORMAT = "b64_json"
    RESPONSE_FORMATS_TO_TRY = ("b64_json",)
else:
    API_RESPONSE_FORMAT = "auto"
    RESPONSE_FORMATS_TO_TRY = ("url", "b64_json")

# URL 下载 SSRF：可信 CDN 主机 + fake-ip（198.18.0.0/15）放行，真内网永不放行。
_trusted_hosts_raw = os.environ.get("MICU_TRUSTED_DOWNLOAD_HOSTS", "oss.filenest.top").strip()
TRUSTED_DOWNLOAD_HOSTS: frozenset[str] = frozenset(
    h.strip().lower() for h in _trusted_hosts_raw.split(",") if h.strip()
) if _trusted_hosts_raw else frozenset({"oss.filenest.top"})
ALLOW_FAKE_IP_DOWNLOAD = os.environ.get(
    "MICU_ALLOW_FAKE_IP_DOWNLOAD", "1"
).strip().lower() in ("1", "true", "yes")
FAKE_IP_NETWORK = ipaddress.ip_network("198.18.0.0/15")

# save_dir 的安全根目录：tool 调用方无论传什么 save_dir，都不能写到此根之外。
# 默认 = 用户家目录下的 Pictures/micu-out；可用 MICU_SAVE_DIR_ROOT 覆盖。
_SAVE_ROOT = Path(os.environ.get(
    "MICU_SAVE_DIR_ROOT",
    str(Path.home() / "Pictures" / "micu-out"),
)).expanduser().resolve()

# DEFAULT_SAVE_DIR 必须默认与 _SAVE_ROOT 一致，否则手动起 server（不走 install.py）
# 时会触发 _resolve_save_dir 把 cwd/out 重定向到 _SAVE_ROOT，对用户是静默的坑。
DEFAULT_SAVE_DIR = Path(os.environ.get("MICU_SAVE_DIR", str(_SAVE_ROOT)))

# 输入图路径默认不受根限制（工具核心用途就是编辑磁盘上任意位置的图）。
# 安全敏感部署可设 MICU_INPUT_ROOT=<dir>，把输入图也关进白名单根，拒绝根外路径
# （防被 prompt 注入的 LLM 用任意本地路径读取并外发文件）。默认 None = 保持原有行为。
_INPUT_ROOT_RAW = os.environ.get("MICU_INPUT_ROOT", "").strip()
_INPUT_ROOT: Path | None = (
    Path(_INPUT_ROOT_RAW).expanduser().resolve() if _INPUT_ROOT_RAW else None
)

STANDARD_MODEL = "gpt-image-2"
QUALITY_MODEL = "gpt-image-2-openai"
# Compatibility names for callers that imported the old constants.  Their values now
# point at the two current routes; the removed gpt-image-2-pro ID is never accepted.
NONPRO_MODEL = STANDARD_MODEL
PRO_MODEL = QUALITY_MODEL
SUPPORTED_IMAGE_MODELS = (STANDARD_MODEL, QUALITY_MODEL)
VALID_IMAGE_QUALITIES = frozenset({"auto", "low", "medium", "high"})
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

# max edge ≥1600 视为 2K/4K，并切到高质量 OpenAI 线路。
HIGH_RES_EDGE = 1600
# 兼容旧导入名；当前 edits 线路与通用 size 上限一致，不再限制在 1K。
EDITS_MAX_EDGE = 3840

VALID_SIZES_1K = {"1024x1024", "1280x720", "720x1280", "1024x1536", "1536x1024"}
# 1920×1080 不满足 16 像素对齐；推荐 2048×1152。
VALID_SIZES_2K = {"2048x2048", "2048x1152", "1152x2048"}
VALID_SIZES_4K = {"3840x2160", "2160x3840"}

# 大小限制
MAX_N = 10
MIN_SIZE_EDGE = 256
MAX_SIZE_EDGE = 3840
SIZE_ALIGNMENT = 16
MIN_IMAGE_PIXELS = 655_360
MAX_IMAGE_PIXELS = 8_294_400
MAX_IMAGE_ASPECT_RATIO = 3.0

MAX_INPUT_FILE_BYTES = 4 * 1024 * 1024     # 单张输入图 4MB
MAX_TOTAL_INPUT_BYTES = 8 * 1024 * 1024    # 多图原始字节总和 8MB，给代理请求体编码留余量
MAX_RESPONSE_BYTES = 25 * 1024 * 1024      # 单张输出图最大 25MB（4K 实测最高 ~12MB）

# 安全 basename 字符集（保留点号给扩展名等）
_SAFE_BASENAME_RE = re.compile(r"^[A-Za-z0-9_\-.]+$")

# ---------- retry 策略常量（http_client.py 用，集中放这避免循环依赖）----------
RETRYABLE_STATUS = (0, 408, 409, 425, 429, 500, 502, 503, 504, 520, 521, 522, 523, 524, 525, 527)
# 兼容旧版 server_info / 外部 import；当前 Image2 模型不会跨 API 路由 fallback。
FALLBACK_STATUS: frozenset[int] = frozenset()
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
    "_TRUST_ENV", "_SAVE_ROOT", "DEFAULT_SAVE_DIR", "_INPUT_ROOT",
    "API_RESPONSE_FORMAT", "RESPONSE_FORMATS_TO_TRY",
    "TRUSTED_DOWNLOAD_HOSTS", "ALLOW_FAKE_IP_DOWNLOAD", "FAKE_IP_NETWORK",
    "STANDARD_MODEL", "QUALITY_MODEL", "PRO_MODEL", "NONPRO_MODEL",
    "SUPPORTED_IMAGE_MODELS", "VALID_IMAGE_QUALITIES",
    "GROK_MODEL_ALIASES", "GROK_AVAILABLE_MODELS",
    "GROK_ASPECT_RATIO_CHOICES", "GROK_SIZE_MODES",
    "HIGH_RES_EDGE", "EDITS_MAX_EDGE",
    "VALID_SIZES_1K", "VALID_SIZES_2K", "VALID_SIZES_4K",
    "MAX_N", "MIN_SIZE_EDGE", "MAX_SIZE_EDGE", "SIZE_ALIGNMENT",
    "MIN_IMAGE_PIXELS", "MAX_IMAGE_PIXELS", "MAX_IMAGE_ASPECT_RATIO",
    "MAX_INPUT_FILE_BYTES", "MAX_TOTAL_INPUT_BYTES", "MAX_RESPONSE_BYTES",
    "_SAFE_BASENAME_RE",
    "RETRYABLE_STATUS", "FALLBACK_STATUS", "RETRY_AFTER_STATUSES", "BIG_SIZE_FAIL_FAST_STATUS",
    "MAX_RETRY_AFTER_SECONDS", "NETWORK_RETRY_DELAY_SECONDS",
    "SMALL_RETRY_DELAYS_SECONDS", "BIG_RETRY_DELAY_SECONDS", "RETRY_JITTER_SECONDS",
]
