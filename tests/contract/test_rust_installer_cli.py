from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
from pathlib import Path

import pytest

from tests.contract.contract_cases import REPO_ROOT


RUST_BINARY = REPO_ROOT / "target" / "debug" / (
    "micu-image-mcp.exe" if sys.platform == "win32" else "micu-image-mcp"
)


@pytest.mark.skipif(not RUST_BINARY.is_file(), reason="cargo build is required")
def test_rust_installer_and_reset_preserve_unrelated_configuration(tmp_path: Path) -> None:
    home = tmp_path / "home"
    save_dir = tmp_path / "images"
    codex_dir = home / ".codex"
    home.mkdir()
    codex_dir.mkdir()
    claude_path = home / ".claude.json"
    codex_path = codex_dir / "config.toml"
    claude_path.write_text(
        json.dumps({"theme": "dark", "mcpServers": {"other": {"command": "other"}}}),
        encoding="utf-8",
    )
    codex_path.write_text(
        "model = 'gpt-test'\n\n[mcp_servers.other]\ncommand = 'other'\n",
        encoding="utf-8",
    )
    secret = "contract-installer-secret-key"
    env = {
        **os.environ,
        "HOME": str(home),
        "USERPROFILE": str(home),
        "MICU_API_KEY": secret,
        "MICU_SAVE_DIR": str(save_dir),
        "MICU_SAVE_DIR_ROOT": str(save_dir),
        "MICU_RUN_LIVE_TESTS": "0",
    }
    installed = subprocess.run(
        [str(RUST_BINARY), "install", "--yes"],
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    assert installed.returncode == 0, installed.stderr
    assert secret not in installed.stdout + installed.stderr
    claude = json.loads(claude_path.read_text(encoding="utf-8"))
    assert claude["theme"] == "dark"
    assert claude["mcpServers"]["other"]["command"] == "other"
    assert claude["mcpServers"]["micu-image"]["command"] == str(RUST_BINARY.resolve())
    assert claude["mcpServers"]["micu-image"]["args"] == []
    codex = codex_path.read_text(encoding="utf-8")
    assert "model = 'gpt-test'" in codex
    assert "[mcp_servers.other]" in codex
    assert "[mcp_servers.micu-image]" in codex
    assert "[mcp_servers.micu-image.env]" in codex
    assert secret in codex
    if os.name != "nt":
        assert stat.S_IMODE(claude_path.stat().st_mode) == 0o600
        assert stat.S_IMODE(codex_path.stat().st_mode) == 0o600

    doctor = subprocess.run(
        [str(RUST_BINARY), "doctor"],
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    assert doctor.returncode == 0, doctor.stderr
    assert "doctor: OK" in doctor.stderr
    assert secret not in doctor.stdout + doctor.stderr

    reset = subprocess.run(
        [str(RUST_BINARY), "reset", "--yes"],
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    assert reset.returncode == 0, reset.stderr
    claude = json.loads(claude_path.read_text(encoding="utf-8"))
    assert "micu-image" not in claude["mcpServers"]
    assert claude["mcpServers"]["other"]["command"] == "other"
    codex = codex_path.read_text(encoding="utf-8")
    assert "[mcp_servers.other]" in codex
    assert "mcp_servers.micu-image" not in codex
    assert list(home.glob(".claude.json.bak.*"))
    assert list(codex_dir.glob("config.toml.bak.*"))

