from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys

import pytest

import install


def _completed(returncode: int = 0, stdout: str = "", stderr: str = "") -> subprocess.CompletedProcess:
    return subprocess.CompletedProcess(args=[], returncode=returncode, stdout=stdout, stderr=stderr)


def _python_env(command: str, venv=None) -> install.PythonEnv:
    return install.PythonEnv(
        command=command,
        pip_cmd=(command, "-m", "pip"),
        install_extra=(),
        version_cmd=(command, "-m", "pip", "--version"),
        venv=venv,
    )


def test_installer_ignores_legacy_grok_environment(monkeypatch, tmp_path):
    monkeypatch.setenv("MICU_API_KEY", "sk-image2-test")
    monkeypatch.setenv("MICU_GROK_API_KEY", "sk-grok-test")
    monkeypatch.setenv("MICU_SAVE_DIR", str(tmp_path))
    monkeypatch.setattr(install, "_validate_key_group", lambda **_kwargs: True)

    env, _save_dir, _save_root = install.collect_config(
        non_interactive=True,
        baseurl=install.DEFAULT_BASEURL,
    )

    assert env["MICU_API_KEY"] == "sk-image2-test"
    assert "MICU_GROK_API_KEY" not in env
    assert "XAI_MODEL" not in env


def test_phase_a_runtime_is_python_by_default_and_rust_only_when_explicit(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    server = repo / "server.py"
    server.write_text("", encoding="utf-8")
    python_args = argparse.Namespace(runtime="python", rust_binary=None)
    resolved = install.resolve_runtime_command(python_args, repo)
    assert resolved.runtime == "python"
    assert resolved.command == install.sys.executable
    assert resolved.args == [str(server)]

    binary = tmp_path / ("micu-image-mcp.exe" if install.sys.platform == "win32" else "micu-image-mcp")
    binary.write_bytes(b"binary")
    os.chmod(binary, 0o755)
    rust_args = argparse.Namespace(runtime="rust", rust_binary=str(binary))
    rust = install.resolve_runtime_command(rust_args, repo)
    assert rust.runtime == "rust"
    assert rust.command == str(binary.resolve())
    assert rust.args == []


def test_phase_a_writers_preserve_other_mcp_servers(monkeypatch, tmp_path):
    monkeypatch.setattr(install.Path, "home", classmethod(lambda cls: tmp_path))
    claude = tmp_path / ".claude.json"
    claude.write_text(
        json.dumps({"theme": "dark", "mcpServers": {"other": {"command": "other"}}}),
        encoding="utf-8",
    )
    install.write_claude("/bin/micu", [], {"MICU_API_KEY": "secret"})
    merged = json.loads(claude.read_text(encoding="utf-8"))
    assert merged["theme"] == "dark"
    assert merged["mcpServers"]["other"]["command"] == "other"
    assert merged["mcpServers"]["micu-image"]["command"] == "/bin/micu"
    assert merged["mcpServers"]["micu-image"]["args"] == []


# ---------- PEP 668 / 虚拟环境 ----------

def test_in_virtualenv_ignores_stale_virtual_env_var(monkeypatch, tmp_path):
    """VIRTUAL_ENV 可能是别的项目残留的；判断只能看解释器自己的 prefix。"""
    monkeypatch.setenv("VIRTUAL_ENV", str(tmp_path / "other-project" / ".venv"))
    monkeypatch.setenv("CONDA_PREFIX", str(tmp_path / "conda"))
    monkeypatch.setattr(install.sys, "prefix", str(tmp_path))
    monkeypatch.setattr(install.sys, "base_prefix", str(tmp_path), raising=False)
    assert install._in_virtualenv() is False


def test_in_virtualenv_detects_real_venv_by_prefix(monkeypatch, tmp_path):
    monkeypatch.delenv("VIRTUAL_ENV", raising=False)
    monkeypatch.delenv("CONDA_PREFIX", raising=False)
    monkeypatch.setattr(install.sys, "prefix", str(tmp_path / ".venv"))
    monkeypatch.setattr(install.sys, "base_prefix", str(tmp_path), raising=False)
    assert install._in_virtualenv() is True


def test_is_externally_managed_follows_pep668_marker(monkeypatch, tmp_path):
    monkeypatch.setattr(install.sysconfig, "get_paths", lambda: {"stdlib": str(tmp_path)})
    assert install._is_externally_managed() is False
    (tmp_path / "EXTERNALLY-MANAGED").write_text("do not pip install here\n", encoding="utf-8")
    assert install._is_externally_managed() is True


def test_venv_python_layout_per_platform(monkeypatch, tmp_path):
    monkeypatch.setattr(install.os, "name", "nt")
    assert install._venv_python(tmp_path) == tmp_path / "Scripts" / "python.exe"
    monkeypatch.setattr(install.os, "name", "posix")
    assert install._venv_python(tmp_path) == tmp_path / "bin" / "python"


def _stub_managed_system(monkeypatch) -> None:
    monkeypatch.setattr(install, "_in_virtualenv", lambda: False)
    monkeypatch.setattr(install, "_is_externally_managed", lambda: True)


def test_resolve_python_env_creates_venv_when_system_python_is_managed(monkeypatch, tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    vdir = repo / install.VENV_DIR_NAME
    _stub_managed_system(monkeypatch)
    # 第一次探测: .venv 还不存在; 创建之后再探测: 拿到版本
    probes = iter([None, (3, 12, 0)])
    monkeypatch.setattr(install, "_probe_python", lambda py: next(probes))
    created = []
    monkeypatch.setattr(install, "_create_venv",
                        lambda d: created.append(d) or install._venv_python(d))
    monkeypatch.setattr(install, "_run_quiet", lambda cmd, timeout=60: _completed(0, "pip 26.1.2"))

    env = install.resolve_python_env(repo, non_interactive=True, venv_dir=None, break_system=False)

    assert created == [vdir]
    assert env.command == str(install._venv_python(vdir))
    assert env.venv == vdir
    assert env.pip_cmd == (str(install._venv_python(vdir)), "-m", "pip")
    assert env.install_extra == ()


def test_resolve_python_env_reuses_working_venv_without_recreating(monkeypatch, tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    vpy = install._venv_python(repo / install.VENV_DIR_NAME)
    _stub_managed_system(monkeypatch)
    monkeypatch.setattr(install, "_probe_python", lambda py: (3, 13, 1))
    monkeypatch.setattr(install, "_run_quiet", lambda cmd, timeout=60: _completed(0, "pip 26.1.2"))

    def _boom(_dir):
        raise AssertionError("可用的 .venv 不该被重建")

    monkeypatch.setattr(install, "_create_venv", _boom)

    env = install.resolve_python_env(repo, non_interactive=True, venv_dir=None, break_system=False)
    assert env.command == str(vpy)


def test_resolve_python_env_honours_custom_venv_dir(monkeypatch, tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    custom = tmp_path / "venvs" / "micu"
    _stub_managed_system(monkeypatch)
    monkeypatch.setattr(install, "_probe_python", lambda py: (3, 12, 0))
    monkeypatch.setattr(install, "_run_quiet", lambda cmd, timeout=60: _completed(0, "pip 26.1.2"))

    env = install.resolve_python_env(repo, non_interactive=True, venv_dir=custom, break_system=False)

    assert env.venv == custom
    assert env.command == str(install._venv_python(custom))
    assert not (repo / install.VENV_DIR_NAME).exists()


def test_resolve_python_env_falls_back_to_uv_when_venv_has_no_pip(monkeypatch, tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    vpy = install._venv_python(repo / install.VENV_DIR_NAME)
    _stub_managed_system(monkeypatch)
    monkeypatch.setattr(install, "_probe_python", lambda py: (3, 12, 0))
    monkeypatch.setattr(install, "_run_quiet",
                        lambda cmd, timeout=60: _completed(1, "", "No module named pip"))
    monkeypatch.setattr(install.shutil, "which", lambda name: "/usr/bin/uv" if name == "uv" else None)

    env = install.resolve_python_env(repo, non_interactive=True, venv_dir=None, break_system=False)

    assert env.pip_cmd == ("/usr/bin/uv", "pip")
    assert env.install_extra == ("--python", str(vpy))


def test_resolve_python_env_keeps_system_python_when_not_managed(monkeypatch, tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.setattr(install, "_in_virtualenv", lambda: False)
    monkeypatch.setattr(install, "_is_externally_managed", lambda: False)

    env = install.resolve_python_env(repo, non_interactive=True, venv_dir=None, break_system=False)

    assert env.command == sys.executable
    assert env.venv is None
    assert not (repo / install.VENV_DIR_NAME).exists()


def test_resolve_python_env_break_system_packages_skips_venv(monkeypatch, tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    _stub_managed_system(monkeypatch)

    env = install.resolve_python_env(repo, non_interactive=True, venv_dir=None, break_system=True)

    assert env.command == sys.executable
    assert env.venv is None
    assert env.install_extra == ("--break-system-packages",)
    assert not (repo / install.VENV_DIR_NAME).exists()


def test_resolve_python_env_rejects_broken_venv_in_non_interactive(monkeypatch, tmp_path, capsys):
    repo = tmp_path / "repo"
    install._venv_python(repo / install.VENV_DIR_NAME).parent.mkdir(parents=True)
    _stub_managed_system(monkeypatch)
    monkeypatch.setattr(install, "_probe_python", lambda py: None)

    with pytest.raises(SystemExit) as exc:
        install.resolve_python_env(repo, non_interactive=True, venv_dir=None, break_system=False)

    assert exc.value.code == 1
    assert "--venv-dir" in capsys.readouterr().out


def test_install_deps_installs_into_selected_env(monkeypatch, tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    calls: list[list[str]] = []

    def _fake_run(cmd, **_kwargs):
        calls.append(list(cmd))
        return _completed(0)

    monkeypatch.setattr(install.subprocess, "run", _fake_run)
    env = _python_env(str(tmp_path / ".venv" / "bin" / "python"), tmp_path / ".venv")

    install.install_deps(env, repo, "https://pypi.tuna.tsinghua.edu.cn/simple")

    assert calls == [[*env.pip_cmd, "install",
                      "-i", "https://pypi.tuna.tsinghua.edu.cn/simple", "-e", str(repo)]]


def test_install_deps_failure_hints_pep668_for_system_python(monkeypatch, tmp_path, capsys):
    repo = tmp_path / "repo"
    repo.mkdir()
    monkeypatch.setattr(install.subprocess, "run", lambda cmd, **_kwargs: _completed(1))
    monkeypatch.setattr(install, "_is_externally_managed", lambda: True)

    with pytest.raises(SystemExit):
        install.install_deps(_python_env(sys.executable), repo, None)

    out = capsys.readouterr().out
    assert "PEP 668" in out
    assert "--venv-dir" in out


def test_runtime_command_uses_selected_python_env(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    (repo / "server.py").write_text("", encoding="utf-8")
    args = argparse.Namespace(runtime="python", rust_binary=None)
    env = _python_env(str(tmp_path / ".venv" / "bin" / "python"), tmp_path / ".venv")

    resolved = install.resolve_runtime_command(args, repo, env)

    assert resolved.command == env.command
    assert resolved.args == [str(repo / "server.py")]


# ---------- /v1/models 探测走目标解释器 ----------

def test_probe_uses_target_python_and_keeps_key_out_of_argv(monkeypatch):
    seen: dict[str, object] = {}

    def _fake_run(cmd, **kwargs):
        seen["cmd"] = list(cmd)
        seen["input"] = kwargs.get("input")
        return _completed(0, json.dumps({"status": 200, "ids": ["gpt-image-2"]}) + "\n")

    monkeypatch.setattr(install.subprocess, "run", _fake_run)

    ids, error, status = install._model_ids_for_key(
        install.DEFAULT_BASEURL, "sk-probe-key", "/venv/bin/python"
    )

    assert (ids, error, status) == (["gpt-image-2"], None, 200)
    assert seen["cmd"][0] == "/venv/bin/python"
    assert "sk-probe-key" not in seen["cmd"]
    assert seen["input"] == "sk-probe-key\n"


def test_probe_surfaces_child_error(monkeypatch):
    monkeypatch.setattr(
        install.subprocess, "run",
        lambda cmd, **kwargs: _completed(0, json.dumps({"status": 401, "error": "HTTP 401: bad key"})),
    )

    ids, error, status = install._model_ids_for_key(
        install.DEFAULT_BASEURL, "sk-probe-key", "/venv/bin/python"
    )

    assert ids is None
    assert status == 401
    assert "HTTP 401" in error


def test_probe_refuses_non_https_baseurl_before_spawning_child(monkeypatch):
    def _boom(*_args, **_kwargs):
        raise AssertionError("拒绝 baseurl 之后不该再起子进程")

    monkeypatch.setattr(install.subprocess, "run", _boom)

    ids, error, status = install._model_ids_for_key(
        "http://evil.example", "sk-probe-key", "/venv/bin/python"
    )

    assert ids is None
    assert status is None
    assert "https" in error
