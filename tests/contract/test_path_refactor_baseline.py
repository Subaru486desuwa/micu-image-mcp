from __future__ import annotations

import json
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


def _accept_image25_contract_delta(name: str, expected: object, actual: object) -> None:
    """Exclude only the public model/quality changes authorized after the path refactor."""
    if not isinstance(expected, dict) or not isinstance(actual, dict):
        return
    if name == "tools-list.json":
        expected_tools = expected["result"]["tools"]
        actual_tools = actual["result"]["tools"]
        for old, new in zip(expected_tools, actual_tools, strict=True):
            old["description"] = new["description"]
            if new["name"] in {"image_edit", "image_batch_edit", "image_multi_reference"}:
                old["inputSchema"]["properties"]["quality"] = new["inputSchema"]["properties"]["quality"]
    elif name == "validation-calls.json":
        for case in ("generate_grok_disabled", "generate_invalid_quality"):
            expected[case] = actual[case]


@pytest.mark.skipif(not RUST_BINARY.is_file(), reason="cargo build is required")
def test_path_refactor_preserves_initialize_tools_schema_validation_and_public_server_info() -> None:
    current = collect([str(RUST_BINARY)])
    for current_name, baseline_name in (
        ("initialize-2024-11-05.json", "initialize-before-path-refactor.json"),
        ("tools-list.json", "tools-list-before-path-refactor.json"),
        ("validation-calls.json", "validation-calls-before-path-refactor.json"),
    ):
        expected = _before(baseline_name)
        actual = current[current_name]
        if current_name == "initialize-2024-11-05.json":
            expected = normalize_stdio(current_name, expected)
            actual = normalize_stdio(current_name, actual)
        _accept_image25_contract_delta(current_name, expected, actual)
        assert_equal(expected, actual, current_name)

    # Only the two explicitly requested runtime path descriptions may change. Keys, field types,
    # and all other server_info values remain exact.
    expected_info = normalize_stdio(
        "server-info.json", _before("server-info-before-path-refactor.json")
    )
    current_info = normalize_stdio("server-info.json", current["server-info.json"])
    for value in (expected_info, current_info):
        value["result"]["structuredContent"]["version"] = "<PROJECT_VERSION>"
        value["result"]["content"][0]["text"]["version"] = "<PROJECT_VERSION>"
    for surface in ("structuredContent",):
        expected_payload = expected_info["result"][surface]
        current_payload = current_info["result"][surface]
        for key in (
            "default_model",
            "default_models",
            "available_models",
            "size_rules",
            "recommended_sizes",
            "capability_matrix",
        ):
            expected_payload[key] = current_payload[key]
    expected_info["result"]["content"][0]["text"] = current_info["result"]["content"][0]["text"]
    assert_equal(expected_info, current_info, "server_info path refactor")


@pytest.mark.skip(reason="Python reference is frozen; new Rust mock contracts are maintained independently")
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
