"""Standard-library-only bootstrap for the source-tree Python installer."""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import sysconfig
from pathlib import Path

PY_MIN = (3, 10)


def in_virtual_environment() -> bool:
    """Inspect this interpreter, never stale shell activation variables."""
    return (
        sys.prefix != sys.base_prefix
        or hasattr(sys, "real_prefix")
        or (Path(sys.prefix) / "conda-meta").is_dir()
    )


def externally_managed() -> bool:
    if in_virtual_environment():
        return False
    stdlib = sysconfig.get_path("stdlib", sysconfig.get_default_scheme())
    return (Path(stdlib) / "EXTERNALLY-MANAGED").is_file()


def venv_python(directory: Path) -> Path:
    # Resolving the executable itself can follow a symlink back to system Python.
    return directory / ("Scripts/python.exe" if os.name == "nt" else "bin/python")


def _venv_environ(python: Path) -> dict[str, str]:
    env = os.environ.copy()
    env.pop("PYTHONHOME", None)  # This overrides pyvenv.cfg even with an absolute command.
    env["VIRTUAL_ENV"] = str(python.parent.parent)
    env["PATH"] = str(python.parent) + os.pathsep + env.get("PATH", "")
    return env


def _succeeds(command: list[str], *, quiet: bool = False,
              env: dict[str, str] | None = None) -> bool:
    try:
        result = subprocess.run(command, capture_output=quiet, text=True,
                                timeout=120, check=False, env=env)
        return result.returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def _validate_venv(directory: Path) -> Path:
    python = venv_python(directory)
    message = (
        f"{directory} 不是可用的 Python >= 3.10 虚拟环境。"
        "已有文件保持不变；请修复该环境，或用 --venv-dir 指定新的目录。"
    )
    if not (directory / "pyvenv.cfg").is_file():
        raise SystemExit(f"[ERR] {message}")
    probe = (
        "import json,sys;print(json.dumps([list(sys.version_info[:2]),"
        "sys.prefix,sys.base_prefix]))"
    )
    try:
        result = subprocess.run([str(python), "-I", "-c", probe],
                                capture_output=True, text=True, timeout=15,
                                check=False)
        version, prefix, base_prefix = json.loads(result.stdout)
        valid = (
            result.returncode == 0
            and tuple(version) >= PY_MIN
            and prefix != base_prefix
            and Path(prefix).resolve() == directory
        )
    except (OSError, subprocess.TimeoutExpired, ValueError, TypeError):
        valid = False
    if not valid:
        raise SystemExit(f"[ERR] {message}")
    return python


def _create_venv(directory: Path, mirror_url: str | None) -> None:
    # Claim a new directory; never clear, repair or remove a pre-existing one.
    try:
        directory.mkdir(parents=True, exist_ok=False)
    except OSError as exc:
        raise SystemExit(f"[ERR] 无法创建虚拟环境目录 {directory}: {exc}") from exc
    print(f"[..] 创建虚拟环境: {directory}", flush=True)
    if _succeeds([sys.executable, "-m", "venv", str(directory)]):
        return
    uv = shutil.which("uv")
    if uv:
        print("[!!] python -m venv 失败，尝试 uv venv --seed", flush=True)
        command = [uv, "venv", "--seed", "--allow-existing", "--no-config",
                   "--python", sys.executable, str(directory)]
        if mirror_url:
            command += ["--index-url", mirror_url]
        # A shell's UV_VENV_CLEAR must not turn this fallback into a deletion.
        env = {**os.environ, "UV_VENV_CLEAR": "false"}
        if _succeeds(command, env=env):
            return
    raise SystemExit(
        f"[ERR] 创建虚拟环境失败: {directory}。请安装发行版的 venv/ensurepip 支持"
        "（Debian/Ubuntu 通常为 python3-venv），或安装 uv 后指定一个新目录重试。"
        "未向系统 Python 安装依赖；部分创建的目录已保留。"
    )


def _ensure_pip(python: Path, mirror_url: str | None) -> None:
    env = _venv_environ(python)
    if _succeeds([str(python), "-m", "pip", "--version"], quiet=True, env=env):
        return
    if _succeeds([str(python), "-m", "ensurepip", "--upgrade"], env=env):
        return
    uv = shutil.which("uv")
    if uv:
        command = [uv, "pip", "install", "--no-config", "--python", str(python), "pip"]
        if mirror_url:
            command += ["--index-url", mirror_url]
        if _succeeds(command, env=env):
            return
    raise SystemExit(
        f"[ERR] 虚拟环境缺少 pip: {python}。请为此环境安装 pip/ensurepip，"
        "或安装 uv 后重试。未向系统 Python 安装依赖。"
    )


def prepare_python(repo_root: Path, *, venv_dir: str | None = None,
                   break_system_packages: bool = False,
                   mirror_url: str | None = None) -> None:
    """Re-exec in the selected venv so pip, HTTP probes and configs agree.

    Must run before pip checks, dependency imports, prompts and config writes.
    Rust, reset and help paths must not call this function.
    """
    if break_system_packages:
        if venv_dir is not None:
            raise SystemExit("[ERR] --venv-dir 与 --break-system-packages 不能同时使用")
        print("[!!] 已显式启用 --break-system-packages；可能破坏系统 Python 包。", flush=True)
        return
    # An explicit destination takes precedence even in an unmanaged/active env.
    if venv_dir is None:
        if not externally_managed():
            return
        print("[..] 系统 Python 受 PEP 668 保护，改用仓库 .venv", flush=True)
    directory = Path(venv_dir).expanduser() if venv_dir is not None else repo_root / ".venv"
    directory = directory.resolve()
    if in_virtual_environment() and Path(sys.prefix).resolve() == directory:
        return  # The re-executed installer has arrived in its target environment.
    if not directory.exists():
        _create_venv(directory, mirror_url)
    python = _validate_venv(directory)
    _ensure_pip(python, mirror_url)
    print(f"[..] 使用虚拟环境 Python: {python}", flush=True)
    sys.stdout.flush()
    sys.stderr.flush()
    command = [str(python), str(repo_root / "install.py"), *sys.argv[1:]]
    try:
        os.execve(str(python), command, _venv_environ(python))
    except OSError as exc:
        raise SystemExit(f"[ERR] 无法启动虚拟环境 Python {python}: {exc}") from exc
