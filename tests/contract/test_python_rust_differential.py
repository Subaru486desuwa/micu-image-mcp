from __future__ import annotations

import sys
from pathlib import Path

import pytest

from tests.contract.contract_cases import REPO_ROOT, case_names
from tests.contract.differential import compare_live


RUST_BINARY = REPO_ROOT / "target" / "debug" / (
    "micu-image-mcp.exe" if sys.platform == "win32" else "micu-image-mcp"
)


@pytest.mark.skip(reason="Python reference is frozen; new Rust contracts are maintained independently")
def test_python_and_rust_match_the_live_stdio_and_mock_api_contract() -> None:
    assert RUST_BINARY.is_file(), f"build Rust first: {RUST_BINARY}"
    compare_live(
        [sys.executable, str(REPO_ROOT / "server.py")],
        [str(RUST_BINARY)],
        case_names(),
    )
