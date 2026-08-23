from __future__ import annotations

import json
import os
import sys
from pathlib import Path

import pytest

from tests.contract.capture_python_fixtures import collect
from tests.contract.contract_cases import REPO_ROOT, case_names, run_case
from tests.contract.differential import assert_equal, normalize, normalize_stdio


BEFORE = REPO_ROOT / "tests" / "fixtures"
RUST_BINARY = REPO_ROOT / "target" / "debug" / (
    "micu-image-mcp.exe" if sys.platform == "win32" else "micu-image-mcp"
)


def _before(name: str) -> object:
    return json.loads((BEFORE / name).read_text(encoding="utf-8"))


@pytest.mark.skipif(not RUST_BINARY.is_file(), reason="cargo build is required")
def test_path_refactor_preserves_initialize_tools_schema_validation_and_public_server_info() -> None:
    current = collect([str(RUST_BINARY)])
    for current_name, baseline_name in (
        ("initialize-2024-11-05.json", "initialize-before-path-refactor.json"),
        ("tools-list.json", "tools-list-before-path-refactor.json"),
        ("validation-calls.json", "validation-calls-before-path-refactor.json"),
    ):
        assert_equal(_before(baseline_name), current[current_name], current_name)

    # Only the two explicitly requested runtime path descriptions may change. Keys, field types,
    # and all other server_info values remain exact.
    expected_info = normalize_stdio(
        "server-info.json", _before("server-info-before-path-refactor.json")
    )
    current_info = normalize_stdio("server-info.json", current["server-info.json"])
    assert_equal(expected_info, current_info, "server_info path refactor")


@pytest.mark.skipif(
    os.environ.get("MICU_RUN_CONTRACT_TESTS") != "1",
    reason="set MICU_RUN_CONTRACT_TESTS=1 to compare all 36 before/after mock cases",
)
def test_path_refactor_preserves_all_mock_http_multipart_retry_and_output_cases() -> None:
    assert RUST_BINARY.is_file(), f"build Rust first: {RUST_BINARY}"
    baseline = _before("mock-cases-before-path-refactor.json")
    assert isinstance(baseline, dict)
    assert set(baseline).issubset(case_names())
    for name in baseline:
        assert_equal(
            normalize(baseline[name]),
            normalize(run_case([str(RUST_BINARY)], name)),
            f"path refactor before/after case {name}",
        )
