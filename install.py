#!/usr/bin/env python3
"""米醋画图 MCP 一键安装脚本。

跨平台（macOS / Linux / Windows）：
- 自动 pip install 依赖（实时输出 / 可选国内镜像）
- 交互问 API key + 输出目录（脱敏二次确认）
- 自动写入 Claude Code / Codex CLI 配置（已存在则备份再合并）
- 同步设置 MICU_SAVE_DIR_ROOT 沙箱根，避免自定义目录被沙箱拒
- 自检 server 能不能起来 + 给出脱敏摘要
- 检测 Claude Code / Codex 进程并提示先关再启
- 可选顺带配置米醋 Grok 图像通道 token

用法：
    python install.py
    python install.py --mirror tsinghua            # 用清华镜像装 pip 包
    python install.py --baseurl https://...        # 高级: 覆盖 baseurl
    python install.py --no-codex                   # 不写 Codex 配置
    python install.py --no-claude                  # 不写 Claude 配置
    python install.py --yes                        # 非交互, 全用环境变量
        MICU_API_KEY=... MICU_SAVE_DIR=... python install.py --yes
        MICU_GROK_API_KEY=... python install.py --yes
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from datetime import datetime
from pathlib import Path
from urllib.parse import urlsplit

PY_MIN = (3, 10)

PIP_MIRRORS = {
    "tsinghua": "https://pypi.tuna.tsinghua.edu.cn/simple",
    "aliyun": "https://mirrors.aliyun.com/pypi/simple/",
    "tencent": "https://mirrors.cloud.tencent.com/pypi/simple",
    "ustc": "https://pypi.mirrors.ustc.edu.cn/simple/",
    "default": None,
}

DEFAULT_BASEURL = "https://www.micuapi.ai"
DEFAULT_GROK_MODEL = "grok-imagine-image-lite"
DEFAULT_GROK_SIZE_MODE = "contain"
GROK_SIZE_MODES = {"backend", "contain", "cover", "stretch"}
IMAGE2_MODELS = ("gpt-image-2", "gpt-image-2-pro")
GROK_IMAGE_MODELS = (
    "grok-imagine-image-lite",
    "grok-imagine-image",
    "grok-imagine-image-pro",
    "grok-imagine-image-edit",
)


# ---------- 日志输出 ----------

def _print(tag: str, msg: str) -> None:
    print(f"[{tag}] {msg}")


def info(msg: str) -> None:
    _print("..", msg)


def ok(msg: str) -> None:
    _print("OK", msg)


def warn(msg: str) -> None:
    _print("!!", msg)


def err(msg: str) -> None:
    _print("ERR", msg)
    sys.exit(1)


def step(msg: str) -> None:
    print(f"\n>>> {msg}")


def mask_key(key: str) -> str:
    if len(key) <= 8:
        return "***"
    return f"{key[:5]}...{key[-4:]}"


def _read_config_text(path: Path) -> str:
    """读配置文本并归一化换行：CRLF/CR → LF，且保证以 \\n 结尾。

    正则按 ^...\\n 逐节匹配；Windows 的 CRLF 或末节无尾随换行都会让 subn 命中 0 次，
    导致 key 删不净 / 静默跳过。统一归一化堵住这两个坑。
    """
    text = path.read_text(encoding="utf-8").replace("\r\n", "\n").replace("\r", "\n")
    if text and not text.endswith("\n"):
        text += "\n"
    return text


def _chmod_600(path: Path) -> None:
    """限制为仅属主可读写（明文 key 防同机其他用户读取）。Windows 上 chmod 语义有限，忽略错误。"""
    try:
        os.chmod(path, 0o600)
    except OSError:
        pass


def _atomic_write_secure(path: Path, text: str) -> None:
    """原子写（temp + os.replace）+ chmod 600。

    避免写到一半中断留下截断的用户配置（如 ~/.claude.json 含全部 MCP server），
    并保证落盘文件不带宽松权限。
    """
    tmp = path.with_name(f"{path.name}.tmp.{os.getpid()}")
    try:
        os.unlink(tmp)  # 清掉同 pid 的陈旧 tmp（极少见），让 O_EXCL 不被绊住
    except OSError:
        pass
    # 以 0600 原子创建（O_EXCL）后再写，消除"先 0644 写明文 key、后 chmod"的宽松权限窗口。
    fd = os.open(str(tmp), os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            f.write(text)
    except BaseException:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise
    os.replace(tmp, path)


# ---------- 环境检查 ----------

def check_python() -> None:
    if sys.version_info < PY_MIN:
        cur = ".".join(str(v) for v in sys.version_info[:3])
        print(f"[ERR] 需要 Python >= {PY_MIN[0]}.{PY_MIN[1]}, 当前 {cur}")
        print("      下载: https://www.python.org/downloads/")
        sys.exit(1)
    ok(f"Python {sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}")


def check_pip() -> None:
    p = subprocess.run([sys.executable, "-m", "pip", "--version"],
                       capture_output=True, text=True)
    if p.returncode != 0:
        err("pip 不可用. 请先装 pip: https://pip.pypa.io/en/stable/installation/")
    ok(f"pip 可用: {p.stdout.strip()}")


def check_running_clients() -> None:
    """提示 Claude Code / Codex 在跑就先关掉.

    用 `pgrep -l` (无 -f), 只匹配进程名 (comm), 不扫描完整命令行/环境变量.
    避免被 npm MCP 子进程 PATH 里 'codex.system' 之类误命中.
    """
    if sys.platform == "win32":
        return
    matched: list[str] = []
    for pat in ("Claude", "codex"):
        try:
            p = subprocess.run(["pgrep", "-l", "-i", pat],
                               capture_output=True, text=True, timeout=3)
        except (FileNotFoundError, subprocess.TimeoutExpired):
            return
        for line in p.stdout.strip().splitlines():
            # pgrep -l 输出: "PID  comm"
            parts = line.split(maxsplit=1)
            if len(parts) != 2:
                continue
            comm = parts[1].lower()
            # 过滤包装器 / 子进程 / 自身 python 解释器
            if any(skip in comm for skip in ("node", "npm", "npx", "python", "install.py")):
                continue
            matched.append(line)
    if matched:
        pids = [line.split()[0] for line in matched]
        warn(f"检测到 Claude Code / Codex 相关进程 ({len(pids)} 个, PID: {', '.join(pids[:8])})")
        warn("装完后请重启客户端使新配置生效.")


# ---------- 依赖安装 ----------

def install_deps(repo_root: Path, mirror_url: str | None) -> None:
    step("安装依赖")
    extra = ["-i", mirror_url] if mirror_url else []
    if mirror_url:
        info(f"使用镜像: {mirror_url}")
    cmd = [sys.executable, "-m", "pip", "install", *extra, "-e", str(repo_root)]
    info(" ".join(cmd))
    rc = subprocess.run(cmd).returncode
    if rc != 0:
        warn("editable install 失败, 改装顶层依赖")
        cmd2 = [sys.executable, "-m", "pip", "install", *extra,
                "mcp[cli]>=1.0.0", "httpx>=0.27.0", "Pillow>=10.0.0"]
        info(" ".join(cmd2))
        rc2 = subprocess.run(cmd2).returncode
        if rc2 != 0:
            err("pip install 失败. 国内用户可加 --mirror tsinghua 重试")
    ok("依赖就绪")


# ---------- 收集配置 ----------

def ask(prompt: str, default: str | None = None, secret: bool = False) -> str:
    suffix = f" [{default}]" if default else ""
    if secret:
        from getpass import getpass
        v = getpass(f"{prompt}{suffix}: ").strip()
    else:
        v = input(f"{prompt}{suffix}: ").strip()
    return v or (default or "")


def ask_yes_no(prompt: str, default: bool = True) -> bool:
    hint = "[Y/n]" if default else "[y/N]"
    while True:
        v = input(f"{prompt} {hint}: ").strip().lower()
        if not v:
            return default
        if v in ("y", "yes"):
            return True
        if v in ("n", "no"):
            return False


def _format_model_list(models: list[str], limit: int = 8) -> str:
    if not models:
        return "(empty)"
    head = ", ".join(models[:limit])
    return head + (f", ... +{len(models) - limit}" if len(models) > limit else "")


def _model_ids_for_key(baseurl: str, api_key: str) -> tuple[list[str] | None, str | None, int | None]:
    """Best-effort /v1/models probe. Returns (ids, error, status_code)."""
    try:
        import httpx
    except ImportError:
        warn("httpx 不可用，跳过 key 分组校验")
        return None, "httpx unavailable", None

    url = baseurl.rstrip("/") + "/v1/models"
    # KEYLEAK-1：拒绝把 key 发往非 https 且非本地的 baseurl（防误把 key 发到攻击者 host）。
    _parts = urlsplit(baseurl)
    _host = (_parts.hostname or "").lower()
    _is_local = _host in ("localhost", "127.0.0.1", "::1") or _host.endswith(".localhost")
    if not (_parts.scheme == "https" or (_parts.scheme == "http" and _is_local)):
        return None, (
            f"拒绝把 key 发往非 https 且非本地的 baseurl（scheme={_parts.scheme!r} host={_host!r}）；"
            f"自定义代理请用 https，本地调试可用 http://localhost"
        ), None
    try:
        r = httpx.get(
            url,
            headers={"Authorization": f"Bearer {api_key}"},
            timeout=20,
            trust_env=False,
        )
    except Exception as e:  # noqa: BLE001 - installer should degrade gracefully here.
        return None, f"{type(e).__name__}: {e}", None

    if r.status_code != 200:
        body = r.text.replace("\n", " ")[:240]
        return None, f"HTTP {r.status_code}: {body}", r.status_code

    try:
        data = r.json()
    except ValueError as e:
        return None, f"invalid JSON: {e}", r.status_code
    ids = [
        item.get("id", "")
        for item in data.get("data", [])
        if isinstance(item, dict) and isinstance(item.get("id"), str)
    ]
    return ids, None, r.status_code


def _validate_key_group(
    *,
    label: str,
    baseurl: str,
    api_key: str,
    expected_models: tuple[str, ...],
    non_interactive: bool,
) -> bool:
    ids, error, status_code = _model_ids_for_key(baseurl, api_key)
    if error:
        msg = f"{label} key 分组校验失败: {error}"
        if non_interactive and status_code in (401, 403):
            err(msg)
        warn(msg)
        warn("这次不会阻止安装，但如果 key 分组不对，运行时会报 no available channel。")
        return True

    assert ids is not None
    matched = [m for m in expected_models if m in ids]
    if matched:
        ok(f"{label} key 可见模型: {_format_model_list(matched)}")
        return True

    image_like = [
        m for m in ids
        if "image" in m.lower() or m.startswith("grok-imagine")
    ]
    msg = (
        f"{label} key 看不到期望模型 {_format_model_list(list(expected_models))}; "
        f"当前图像相关模型: {_format_model_list(image_like)}"
    )
    if non_interactive:
        err(msg)
    warn(msg)
    warn("这通常表示粘错了米醋后台分组 token。")
    return False


def collect_grok_config(non_interactive: bool, baseurl: str, image2_key: str) -> dict[str, str]:
    if non_interactive:
        grok_key = os.environ.get(
            "MICU_GROK_API_KEY",
            os.environ.get("XAI_API_KEY", os.environ.get("GROK_API_KEY", "")),
        ).strip()
        if not grok_key:
            return {}
        if grok_key == image2_key:
            warn("MICU_GROK_API_KEY 与 MICU_API_KEY 相同；将用 /v1/models 校验是否真的同时包含两个分组。")
        _validate_key_group(
            label="Grok",
            baseurl=baseurl,
            api_key=grok_key,
            expected_models=GROK_IMAGE_MODELS,
            non_interactive=True,
        )
        return {
            "MICU_GROK_API_KEY": grok_key,
            "XAI_MODEL": os.environ.get("XAI_MODEL", os.environ.get("GROK_MODEL", DEFAULT_GROK_MODEL)).strip() or DEFAULT_GROK_MODEL,
            "MICU_GROK_SIZE_MODE": _clean_grok_size_mode(os.environ.get("MICU_GROK_SIZE_MODE", DEFAULT_GROK_SIZE_MODE)),
        }

    if not ask_yes_no("同时配置米醋 Grok 生图通道?", default=False):
        return {}
    print("\n=== 配置米醋 Grok 生图通道 ===")
    info("这里需要米醋后台的 Grok 图像分组 token；不要粘 Image2/gpt-image-2 分组 token。")
    info("baseurl 与 Image2 共用 MICU_BASEURL，但两个分组通常是两把不同的 key。")
    while True:
        grok_key = ask("米醋 Grok 分组 token (用于 grok-imagine-image, sk-...)", secret=True)
        if not grok_key:
            warn("Grok token 为空，已跳过")
            return {}
        preview = mask_key(grok_key)
        if grok_key == image2_key:
            warn("Grok token 与 Image2 key 相同；通常这表示粘错，除非后台明确同一 token 同时包含两个模型分组。")
        if not grok_key.startswith("sk-"):
            warn(f"Grok token 不以 sk- 开头，可能粘错: {preview}")
        else:
            info(f"输入的 Grok token: {preview}")
        if not ask_yes_no("确认这个 Grok 分组 token?", default=True):
            continue
        if _validate_key_group(
            label="Grok",
            baseurl=baseurl,
            api_key=grok_key,
            expected_models=GROK_IMAGE_MODELS,
            non_interactive=False,
        ):
            break
        if ask_yes_no("仍然使用这个 Grok token?", default=False):
            break
    grok_model = ask("Grok model", default=DEFAULT_GROK_MODEL) or DEFAULT_GROK_MODEL
    if grok_model not in GROK_IMAGE_MODELS:
        # 不硬阻（后台分组名可能变化），但拼错是常见坑，给 size_mode 同款软提示
        warn(f"Grok model {grok_model!r} 不在已知列表 {list(GROK_IMAGE_MODELS)}；若运行时报 no available channel 请核对拼写")
    grok_size_mode = ask("Grok size mode (contain/cover/stretch/backend)", default=DEFAULT_GROK_SIZE_MODE)
    return {
        "MICU_GROK_API_KEY": grok_key,
        "XAI_MODEL": grok_model or DEFAULT_GROK_MODEL,
        "MICU_GROK_SIZE_MODE": _clean_grok_size_mode(grok_size_mode),
    }


def _clean_grok_size_mode(value: str) -> str:
    mode = (value or DEFAULT_GROK_SIZE_MODE).strip().lower()
    if mode not in GROK_SIZE_MODES:
        warn(f"未知 MICU_GROK_SIZE_MODE={value!r}，已使用 {DEFAULT_GROK_SIZE_MODE}")
        return DEFAULT_GROK_SIZE_MODE
    return mode


def collect_config(non_interactive: bool, baseurl: str) -> tuple[dict[str, str], str, str]:
    """返回 (env_dict, save_dir, save_dir_root)."""
    home = Path.home()
    default_save = home / "Pictures" / "micu-out"

    if non_interactive:
        api_key = os.environ.get("MICU_API_KEY", "").strip()
        save_dir_raw = os.environ.get("MICU_SAVE_DIR", str(default_save)).strip()
        if not api_key:
            err("--yes 模式需要环境变量 MICU_API_KEY=sk-...")
        _validate_key_group(
            label="Image2",
            baseurl=baseurl,
            api_key=api_key,
            expected_models=IMAGE2_MODELS,
            non_interactive=True,
        )
    else:
        print("\n=== 配置米醋 MCP ===")
        info(f"baseurl: {baseurl}")
        info("Image2 key 在米醋后台获取，必须能看到 gpt-image-2 / gpt-image-2-pro。")
        info("不要在这里粘 Grok 专用分组 key；Grok 会在下一步单独配置。")
        while True:
            api_key = ask("米醋 Image2 分组 API key (用于 gpt-image-2, sk-...)", secret=True)
            if not api_key:
                warn("API key 不能为空")
                continue
            preview = mask_key(api_key)
            if not api_key.startswith("sk-"):
                warn(f"API key 不以 sk- 开头, 可能粘错: {preview}")
            else:
                info(f"输入的 Image2 key: {preview}")
            if not ask_yes_no("确认这个 Image2 分组 key?", default=True):
                continue
            if _validate_key_group(
                label="Image2",
                baseurl=baseurl,
                api_key=api_key,
                expected_models=IMAGE2_MODELS,
                non_interactive=False,
            ):
                break
            if ask_yes_no("仍然使用这个 Image2 key?", default=False):
                break
        save_dir_raw = ask("输出目录 (生成的图存这里)", default=str(default_save))

    save_path = Path(save_dir_raw).expanduser().resolve()
    try:
        save_path.mkdir(parents=True, exist_ok=True)
    except OSError as e:
        err(f"创建输出目录失败: {save_path}\n{e}")

    # MICU_SAVE_DIR_ROOT: 默认 = save_dir 自身, 让 server 沙箱不会拒
    save_root = str(save_path)
    ok(f"输出目录: {save_path}")
    ok(f"沙箱根目录: {save_root}")
    env = {
        "MICU_API_KEY": api_key,
        "MICU_SAVE_DIR": str(save_path),
        "MICU_SAVE_DIR_ROOT": save_root,
    }
    if baseurl != DEFAULT_BASEURL:
        env["MICU_BASEURL"] = baseurl
    env.update(collect_grok_config(non_interactive, baseurl, api_key))
    return env, str(save_path), save_root


# ---------- 写客户端配置 ----------

def _backup(path: Path) -> Path | None:
    if not path.exists():
        return None
    bak = path.with_name(path.name + f".bak.{datetime.now().strftime('%Y%m%d_%H%M%S')}")
    shutil.copy2(path, bak)
    _chmod_600(bak)  # 备份含明文 key，收紧到仅属主可读写（继承源文件 0644 会泄露给同机其他用户）
    info(f"备份: {path.name} -> {bak.name}")
    return bak


def write_claude(server_path: str, env_dict: dict) -> Path:
    step("配置 Claude Code")
    cfg = Path.home() / ".claude.json"
    data: dict = {}
    if cfg.exists():
        _backup(cfg)
        try:
            data = json.loads(cfg.read_text(encoding="utf-8"))
        except json.JSONDecodeError as e:
            warn(f"现有 ~/.claude.json 不是合法 JSON: {e}")
            warn("备份已留, 你可以手动修复后重跑, 或加 --no-claude 跳过.")
            err("退出, 避免破坏现有配置")
        if not isinstance(data, dict):
            err("~/.claude.json 顶层不是 object, 备份已留, 请手动检查")
    servers = data.setdefault("mcpServers", {})
    if "micu-image" in servers:
        info("已存在 micu-image 配置, 覆盖")
    servers["micu-image"] = {
        "command": sys.executable,
        "args": [server_path],
        "env": env_dict,
    }
    _atomic_write_secure(cfg, json.dumps(data, indent=2, ensure_ascii=False))
    ok(f"写入 {cfg}")
    return cfg


def write_codex(server_path: str, env_dict: dict) -> Path:
    step("配置 Codex CLI")
    cfg_dir = Path.home() / ".codex"
    cfg = cfg_dir / "config.toml"
    cfg_dir.mkdir(parents=True, exist_ok=True)
    try:
        os.chmod(cfg_dir, 0o700)  # 含明文 key 的配置目录收紧权限
    except OSError:
        pass

    def tstr(s: str) -> str:
        return json.dumps(s, ensure_ascii=False)  # JSON 字符串字面量恰好也是合法 TOML basic string

    env_lines = "\n".join(f"{k} = {tstr(v)}" for k, v in env_dict.items())
    block = (
        "\n[mcp_servers.micu-image]\n"
        f"command = {tstr(sys.executable)}\n"
        f"args = [{tstr(server_path)}]\n"
        "\n[mcp_servers.micu-image.env]\n"
        f"{env_lines}\n\n"
    )

    if cfg.exists():
        existing = _read_config_text(cfg)
        if "[mcp_servers.micu-image]" in existing:
            _backup(cfg)
            pattern = re.compile(
                r"(?m)^\[mcp_servers\.micu-image\]\n"
                r"(?:(?!^\[)[^\n]*\n)*"
                r"(?:^\[mcp_servers\.micu-image\.env\]\n(?:(?!^\[)[^\n]*\n)*)?"
            )
            updated, count = pattern.subn(block.lstrip(), existing, count=1)
            if count != 1:
                warn("已存在 [mcp_servers.micu-image]，但自动定位旧节失败，跳过以免破坏配置")
                warn(f"请手动编辑 {cfg}")
                return cfg
            _atomic_write_secure(cfg, updated)
            ok(f"更新 {cfg}")
            return cfg
        _backup(cfg)
        _atomic_write_secure(cfg, existing + block)
    else:
        _atomic_write_secure(cfg, block.lstrip())
    ok(f"写入 {cfg}")
    return cfg


# ---------- 自检 ----------

_EXPECTED_TOOLS = {"image_generate", "image_edit", "image_batch_edit",
                   "image_multi_reference", "server_info"}


def smoke_test(server_path: str, env_dict: dict) -> None:
    step("自检 server 启动")
    env = os.environ.copy()
    env.update(env_dict)
    init_msg = (
        b'{"jsonrpc":"2.0","id":1,"method":"initialize",'
        b'"params":{"protocolVersion":"2024-11-05","capabilities":{},'
        b'"clientInfo":{"name":"installer","version":"1"}}}\n'
    )
    initialized_notice = b'{"jsonrpc":"2.0","method":"notifications/initialized"}\n'
    tools_list_msg = b'{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}\n'

    try:
        p = subprocess.Popen(
            [sys.executable, server_path],
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as e:
        warn(f"启动失败: {e}")
        return
    payload = init_msg + initialized_notice + tools_list_msg
    try:
        out, errout = p.communicate(input=payload, timeout=15)
    except subprocess.TimeoutExpired:
        p.kill()
        out, errout = p.communicate()

    handshake_ok = b'"result"' in out and b'"protocolVersion"' in out
    if handshake_ok:
        ok("server initialize 握手成功")
    else:
        warn("server 没正常握手, 但依赖装好了, 可以重启客户端再试")
        if errout:
            tail = errout[-300:].decode(errors="replace")
            info(f"stderr 末 300 字:\n{tail}")
        return

    # 解析 tools/list 响应：fastmcp 在 stdout 按 line-delimited JSON 输出
    found_tools: set[str] = set()
    for line in out.splitlines():
        line = line.strip()
        if not line.startswith(b"{"):
            continue
        try:
            obj = json.loads(line)
        except ValueError:
            continue
        if obj.get("id") != 2:
            continue
        result = obj.get("result") or {}
        for t in result.get("tools") or []:
            name = t.get("name")
            if isinstance(name, str):
                found_tools.add(name)
        break

    if not found_tools:
        warn("tools/list 没拿到响应；可能 server 已退出或 fastmcp 改了协议输出顺序")
        return
    missing = _EXPECTED_TOOLS - found_tools
    extra = found_tools - _EXPECTED_TOOLS
    if missing:
        warn(f"tools/list 缺少预期 tool: {sorted(missing)}")
        warn("server.py 可能被改坏了；重装 / git pull 后再试")
    else:
        ok(f"tools/list OK，可用 tools: {sorted(found_tools)}")
    if extra:
        info(f"额外发现非预期 tool: {sorted(extra)}（多半是新加的）")


# ---------- 摘要 ----------

def summary(env_dict: dict, claude_cfg: Path | None, codex_cfg: Path | None,
            server_path: str) -> None:
    print("\n=== 完成 ===")
    print(f"  python      : {sys.executable}")
    print(f"  server.py   : {server_path}")
    print(f"  api key     : {mask_key(env_dict.get('MICU_API_KEY', ''))}")
    print(f"  save_dir    : {env_dict.get('MICU_SAVE_DIR', '')}")
    print(f"  save_root   : {env_dict.get('MICU_SAVE_DIR_ROOT', '')}")
    if "MICU_BASEURL" in env_dict:
        print(f"  baseurl     : {env_dict['MICU_BASEURL']}")
    if "MICU_GROK_API_KEY" in env_dict:
        print(f"  grok key    : {mask_key(env_dict.get('MICU_GROK_API_KEY', ''))}")
        print(f"  grok model  : {env_dict.get('XAI_MODEL', '')}")
        print(f"  grok size   : {env_dict.get('MICU_GROK_SIZE_MODE', DEFAULT_GROK_SIZE_MODE)}")
        print(f"  grok base   : {env_dict.get('MICU_BASEURL', DEFAULT_BASEURL)}")
    if claude_cfg:
        print(f"  Claude 配置 : {claude_cfg}")
    if codex_cfg:
        print(f"  Codex  配置 : {codex_cfg}")
    print()
    print("下一步:")
    print("  1. 重启 Claude Code / Codex CLI")
    print("  2. 让 LLM 说: \"调用 server_info\"")
    print("  3. 看到 baseurl / 路由规则就装好了")


# ---------- --reset 清理 ----------

def reset_claude() -> Path | None:
    cfg = Path.home() / ".claude.json"
    if not cfg.exists():
        info(f"无 {cfg}, 跳过 Claude 清理")
        return None
    try:
        data = json.loads(cfg.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        warn(f"{cfg} 不是合法 JSON ({e}), 跳过以免破坏配置")
        return None
    servers = data.get("mcpServers") if isinstance(data, dict) else None
    if not isinstance(servers, dict) or "micu-image" not in servers:
        info("~/.claude.json 中没找到 micu-image, 跳过")
        return None
    _backup(cfg)
    servers.pop("micu-image", None)
    _atomic_write_secure(cfg, json.dumps(data, indent=2, ensure_ascii=False))
    ok(f"已从 {cfg} 移除 mcpServers.micu-image")
    return cfg


def reset_codex() -> Path | None:
    cfg = Path.home() / ".codex" / "config.toml"
    if not cfg.exists():
        info(f"无 {cfg}, 跳过 Codex 清理")
        return None
    existing = _read_config_text(cfg)
    if "[mcp_servers.micu-image]" not in existing:
        info(f"{cfg} 中没找到 [mcp_servers.micu-image], 跳过")
        return None
    _backup(cfg)
    pattern = re.compile(
        r"(?m)^\[mcp_servers\.micu-image\]\n"
        r"(?:(?!^\[)[^\n]*\n)*"
        r"(?:^\[mcp_servers\.micu-image\.env\]\n(?:(?!^\[)[^\n]*\n)*)?"
    )
    updated, count = pattern.subn("", existing, count=1)
    if count != 1:
        warn(f"自动定位 [mcp_servers.micu-image] 节失败, 请手动编辑 {cfg}")
        return cfg
    updated = re.sub(r"\n{3,}", "\n\n", updated).lstrip("\n")
    _atomic_write_secure(cfg, updated)
    ok(f"已从 {cfg} 移除 [mcp_servers.micu-image]")
    return cfg


def do_reset(args: argparse.Namespace) -> None:
    print("=== 米醋画图 MCP --reset (移除已写入的 MCP 配置) ===\n")
    info("仅删除 micu-image 这一节; 其他 MCP server 不动. 删除前会备份原文件.")
    touched = []
    if not args.no_claude:
        c = reset_claude()
        if c is not None:
            touched.append(str(c))
    if not args.no_codex:
        c = reset_codex()
        if c is not None:
            touched.append(str(c))
    if not touched:
        info("没有任何文件被改动")
    else:
        ok(f"已处理: {touched}")
    print("\n注意: pip 安装的包仍保留. 如需彻底卸载, 运行:")
    print(f"  {sys.executable} -m pip uninstall -y micu-image-mcp")


# ---------- main ----------

def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="米醋画图 MCP 一键安装")
    p.add_argument("--no-claude", action="store_true", help="不写 Claude Code 配置")
    p.add_argument("--no-codex", action="store_true", help="不写 Codex CLI 配置")
    p.add_argument("--no-smoke", action="store_true", help="跳过自检")
    p.add_argument("--yes", action="store_true",
                   help="非交互模式 (从环境变量读 MICU_API_KEY / MICU_SAVE_DIR；可选 MICU_GROK_API_KEY)")
    p.add_argument("--mirror", choices=list(PIP_MIRRORS.keys()), default="default",
                   help=f"pip 镜像 (默认: 官方源). 可选: {', '.join(k for k in PIP_MIRRORS if k != 'default')}")
    p.add_argument("--pypi-index", default=None, help="自定义 pip index URL (覆盖 --mirror)")
    p.add_argument("--baseurl", default=DEFAULT_BASEURL,
                   help=f"米醋代理 baseurl (默认 {DEFAULT_BASEURL})")
    p.add_argument("--reset", action="store_true",
                   help="移除已写入的 micu-image MCP 配置 (Claude + Codex), 不动 pip 包")
    return p.parse_args()


def main() -> None:
    args = parse_args()

    if args.reset:
        do_reset(args)
        return

    print("=== 米醋画图 MCP 一键安装 ===\n")
    check_python()
    check_pip()
    check_running_clients()

    repo_root = Path(__file__).resolve().parent
    server_path = repo_root / "server.py"
    if not server_path.exists():
        err(f"找不到 server.py: {server_path}")
    info(f"仓库: {repo_root}")

    mirror_url = args.pypi_index or PIP_MIRRORS.get(args.mirror)
    install_deps(repo_root, mirror_url)

    env_dict, save_dir, save_root = collect_config(args.yes, args.baseurl)

    claude_cfg = write_claude(str(server_path), env_dict) if not args.no_claude else None
    codex_cfg = write_codex(str(server_path), env_dict) if not args.no_codex else None

    if not args.no_smoke:
        smoke_test(str(server_path), env_dict)

    summary(env_dict, claude_cfg, codex_cfg, str(server_path))


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\n\n[!!] 用户取消. 已写入的备份文件 (.bak.*) 保留供回滚.")
        sys.exit(130)
