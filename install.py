#!/usr/bin/env python3
"""米醋画图 MCP 一键安装脚本。

跨平台（macOS / Linux / Windows）：
- 自动 pip install 依赖（实时输出 / 可选国内镜像）
- 交互问 API key + 输出目录（脱敏二次确认）
- 自动写入 Claude Code / Codex CLI 配置（已存在则备份再合并）
- 同步设置 MICU_SAVE_DIR_ROOT 沙箱根，避免自定义目录被沙箱拒
- 自检 server 能不能起来 + 给出脱敏摘要
- 检测 Claude Code / Codex 进程并提示先关再启

用法：
    python install.py
    python install.py --mirror tsinghua            # 用清华镜像装 pip 包
    python install.py --baseurl https://...        # 高级: 覆盖 baseurl
    python install.py --no-codex                   # 不写 Codex 配置
    python install.py --no-claude                  # 不写 Claude 配置
    python install.py --yes                        # 非交互, 全用环境变量
        MICU_API_KEY=... MICU_SAVE_DIR=... python install.py --yes
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

PY_MIN = (3, 10)

PIP_MIRRORS = {
    "tsinghua": "https://pypi.tuna.tsinghua.edu.cn/simple",
    "aliyun": "https://mirrors.aliyun.com/pypi/simple/",
    "tencent": "https://mirrors.cloud.tencent.com/pypi/simple",
    "ustc": "https://pypi.mirrors.ustc.edu.cn/simple/",
    "default": None,
}

DEFAULT_BASEURL = "https://www.micuapi.ai"


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
                "mcp[cli]>=1.0.0", "httpx>=0.27.0"]
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


def collect_config(non_interactive: bool, baseurl: str) -> tuple[str, str, str]:
    """返回 (api_key, save_dir, save_dir_root)."""
    home = Path.home()
    default_save = home / "Pictures" / "micu-out"

    if non_interactive:
        api_key = os.environ.get("MICU_API_KEY", "").strip()
        save_dir_raw = os.environ.get("MICU_SAVE_DIR", str(default_save)).strip()
        if not api_key:
            err("--yes 模式需要环境变量 MICU_API_KEY=sk-...")
    else:
        print("\n=== 配置米醋 MCP ===")
        info(f"baseurl: {baseurl}")
        info("API key 在米醋后台拿: https://www.micuapi.ai")
        while True:
            api_key = ask("米醋 API key (sk-...)", secret=True)
            if not api_key:
                warn("API key 不能为空")
                continue
            preview = mask_key(api_key)
            if not api_key.startswith("sk-"):
                warn(f"API key 不以 sk- 开头, 可能粘错: {preview}")
            else:
                info(f"输入的 key: {preview}")
            if ask_yes_no("确认这个 key?", default=True):
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
    return api_key, str(save_path), save_root


# ---------- 写客户端配置 ----------

def _backup(path: Path) -> Path | None:
    if not path.exists():
        return None
    bak = path.with_name(path.name + f".bak.{datetime.now().strftime('%Y%m%d_%H%M%S')}")
    shutil.copy2(path, bak)
    info(f"备份: {path.name} -> {bak.name}")
    return bak


def _build_env(api_key: str, save_dir: str, save_root: str, baseurl: str) -> dict:
    env = {
        "MICU_API_KEY": api_key,
        "MICU_SAVE_DIR": save_dir,
        "MICU_SAVE_DIR_ROOT": save_root,
    }
    if baseurl != DEFAULT_BASEURL:
        env["MICU_BASEURL"] = baseurl
    return env


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
    cfg.write_text(json.dumps(data, indent=2, ensure_ascii=False), encoding="utf-8")
    ok(f"写入 {cfg}")
    return cfg


def write_codex(server_path: str, env_dict: dict) -> Path:
    step("配置 Codex CLI")
    cfg_dir = Path.home() / ".codex"
    cfg = cfg_dir / "config.toml"
    cfg_dir.mkdir(parents=True, exist_ok=True)

    def tstr(s: str) -> str:
        return json.dumps(s, ensure_ascii=False)  # JSON 字符串字面量恰好也是合法 TOML basic string

    env_inline = ", ".join(f"{k} = {tstr(v)}" for k, v in env_dict.items())
    block = (
        "\n[mcp_servers.micu-image]\n"
        f"command = {tstr(sys.executable)}\n"
        f"args = [{tstr(server_path)}]\n"
        f"env = {{ {env_inline} }}\n"
    )

    if cfg.exists():
        existing = cfg.read_text(encoding="utf-8")
        if "[mcp_servers.micu-image]" in existing:
            warn("已存在 [mcp_servers.micu-image] 节, 跳过 (避免破坏其他配置)")
            warn(f"如需更新: 手动删旧节后重跑, 或编辑 {cfg}")
            return cfg
        _backup(cfg)
        with cfg.open("a", encoding="utf-8") as f:
            f.write(block)
    else:
        cfg.write_text(block.lstrip(), encoding="utf-8")
    ok(f"写入 {cfg}")
    return cfg


# ---------- 自检 ----------

def smoke_test(server_path: str, env_dict: dict) -> None:
    step("自检 server 启动")
    env = os.environ.copy()
    env.update(env_dict)
    init_msg = (
        b'{"jsonrpc":"2.0","id":1,"method":"initialize",'
        b'"params":{"protocolVersion":"2024-11-05","capabilities":{},'
        b'"clientInfo":{"name":"installer","version":"1"}}}\n'
    )
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
    try:
        out, errout = p.communicate(input=init_msg, timeout=10)
    except subprocess.TimeoutExpired:
        p.kill()
        out, errout = p.communicate()
    if b'"result"' in out and b'"protocolVersion"' in out:
        ok("server initialize 握手成功")
    else:
        warn("server 没正常握手, 但依赖装好了, 可以重启客户端再试")
        if errout:
            tail = errout[-300:].decode(errors="replace")
            info(f"stderr 末 300 字:\n{tail}")


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
    if claude_cfg:
        print(f"  Claude 配置 : {claude_cfg}")
    if codex_cfg:
        print(f"  Codex  配置 : {codex_cfg}")
    print()
    print("下一步:")
    print("  1. 重启 Claude Code / Codex CLI")
    print("  2. 让 LLM 说: \"调用 server_info\"")
    print("  3. 看到 baseurl / 路由规则就装好了")


# ---------- main ----------

def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="米醋画图 MCP 一键安装")
    p.add_argument("--no-claude", action="store_true", help="不写 Claude Code 配置")
    p.add_argument("--no-codex", action="store_true", help="不写 Codex CLI 配置")
    p.add_argument("--no-smoke", action="store_true", help="跳过自检")
    p.add_argument("--yes", action="store_true",
                   help="非交互模式 (从环境变量读 MICU_API_KEY / MICU_SAVE_DIR)")
    p.add_argument("--mirror", choices=list(PIP_MIRRORS.keys()), default="default",
                   help=f"pip 镜像 (默认: 官方源). 可选: {', '.join(k for k in PIP_MIRRORS if k != 'default')}")
    p.add_argument("--pypi-index", default=None, help="自定义 pip index URL (覆盖 --mirror)")
    p.add_argument("--baseurl", default=DEFAULT_BASEURL,
                   help=f"米醋代理 baseurl (默认 {DEFAULT_BASEURL})")
    return p.parse_args()


def main() -> None:
    args = parse_args()

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

    api_key, save_dir, save_root = collect_config(args.yes, args.baseurl)
    env_dict = _build_env(api_key, save_dir, save_root, args.baseurl)

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
