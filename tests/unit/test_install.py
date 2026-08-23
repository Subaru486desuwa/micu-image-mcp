from __future__ import annotations

import argparse
import json
import os

import install


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
