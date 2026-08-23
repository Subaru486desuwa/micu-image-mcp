from __future__ import annotations

import sys
import tempfile
from pathlib import Path

import pytest

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.differential import assert_equal, normalize, normalize_stdio
from tests.contract.stdio_driver import StdioSession, isolated_server_env


RUST_BINARY = REPO_ROOT / "target" / "debug" / (
    "micu-image-mcp.exe" if sys.platform == "win32" else "micu-image-mcp"
)


@pytest.mark.skipif(not RUST_BINARY.is_file(), reason="cargo build is required")
def test_python_and_rust_both_ignore_unknown_tool_arguments() -> None:
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
        with (
            StdioSession(
                [sys.executable, str(REPO_ROOT / "server.py")], env, REPO_ROOT
            ) as python,
            StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as rust,
        ):
            python.initialize()
            rust.initialize()
            for request_id, (tool, arguments) in enumerate(cases, start=10):
                params = {"name": tool, "arguments": arguments}
                expected = python.request(request_id, "tools/call", params)
                actual = rust.request(request_id, "tools/call", params)
                normalize_response = (
                    (lambda value: normalize_stdio("server-info.json", value))
                    if tool == "server_info"
                    else normalize
                )
                assert_equal(
                    normalize_response(expected),
                    normalize_response(actual),
                    f"unknown arguments for {tool}",
                )
