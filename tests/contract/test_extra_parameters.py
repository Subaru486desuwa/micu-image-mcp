from __future__ import annotations

import tempfile
import sys
from pathlib import Path

import pytest

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.stdio_driver import StdioSession, isolated_server_env, text_content_json


RUST_BINARY = REPO_ROOT / "target" / "debug" / (
    "micu-image-mcp.exe" if sys.platform == "win32" else "micu-image-mcp"
)


@pytest.mark.skipif(not RUST_BINARY.is_file(), reason="cargo build is required")
def test_rust_ignores_unknown_tool_arguments() -> None:
    cases = [
        ("image_generate", {"prompt": "", "future_field": "ignored"}),
        (
            "image_edit",
            {
                "prompt": "",
                "image_path": "/definitely/missing.png",
                "future_field": "ignored",
            },
        ),
        (
            "image_batch_edit",
            {"prompt": "", "image_paths": [], "future_field": "ignored"},
        ),
        (
            "image_multi_reference",
            {"prompt": "", "image_paths": [], "future_field": "ignored"},
        ),
        ("server_info", {"future_field": "ignored"}),
    ]
    with tempfile.TemporaryDirectory(prefix="micu-extra-args-") as temp_dir:
        root = Path(temp_dir).resolve()
        env = isolated_server_env(root)
        with StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as rust:
            rust.initialize()
            for request_id, (tool, arguments) in enumerate(cases, start=10):
                params = {"name": tool, "arguments": arguments}
                actual = rust.request(request_id, "tools/call", params)
                assert "error" not in actual, actual
                payload = text_content_json(actual)
                assert isinstance(payload, dict), actual
                if tool == "server_info":
                    assert payload["default_models"]["image_generate"] == "gpt-image-2.5-flare"
                else:
                    assert payload["ok"] is False
