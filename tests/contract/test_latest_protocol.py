from __future__ import annotations

import sys
import tempfile
from pathlib import Path

import pytest

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.stdio_driver import StdioSession, isolated_server_env, text_content_json


RUST_BINARY = REPO_ROOT / "target" / "debug" / (
    "micu-image-mcp.exe" if sys.platform == "win32" else "micu-image-mcp"
)


def latest_meta() -> dict:
    return {
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "micu-latest-contract", "version": "1"},
        "io.modelcontextprotocol/clientCapabilities": {},
    }


@pytest.mark.skipif(not RUST_BINARY.is_file(), reason="cargo build is required")
def test_rust_supports_the_stateless_2026_07_28_lifecycle() -> None:
    with tempfile.TemporaryDirectory(prefix="micu-latest-") as temp_dir:
        save_root = Path(temp_dir).resolve()
        with StdioSession(
            [str(RUST_BINARY)],
            env=isolated_server_env(save_root),
            cwd=REPO_ROOT,
        ) as session:
            discover = session.request(
                1,
                "server/discover",
                {"_meta": latest_meta()},
            )
            assert "2026-07-28" in discover["result"]["supportedVersions"]
            tools = session.request(2, "tools/list", {"_meta": latest_meta()})
            assert tools["result"]["resultType"] == "complete"
            assert len(tools["result"]["tools"]) == 5
            info = session.request(
                3,
                "tools/call",
                {"_meta": latest_meta(), "name": "server_info", "arguments": {}},
            )
            assert info["result"]["resultType"] == "complete"
            assert text_content_json(info)["available_models"] == [
                "gpt-image-2",
                "gpt-image-2-openai",
            ]
        assert all(line.lstrip().startswith(b"{") for line in session.stdout_lines)

