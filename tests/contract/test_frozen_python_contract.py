"""Guard the HEAD-era Python reference fixtures against accidental drift."""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

from tests.contract.capture_python_fixtures import FIXTURE_ROOT, REPO_ROOT, collect


def normalize_diagnostic_urls(value: object) -> object:
    # Dependency documentation versions are incidental; keep diagnostics and error codes exact.
    if isinstance(value, dict):
        return {key: normalize_diagnostic_urls(item) for key, item in value.items()}
    if isinstance(value, list):
        return [normalize_diagnostic_urls(item) for item in value]
    if isinstance(value, str):
        return re.sub(r"(https://errors\.pydantic\.dev/)\d+\.\d+(/v/)",
                      r"\1<DEPENDENCY_VERSION>\2", value)
    return value


def test_python_reference_matches_frozen_stdio_contract() -> None:
    actual = collect([sys.executable, str(REPO_ROOT / "server.py")])
    for name, actual_value in actual.items():
        expected = json.loads((FIXTURE_ROOT / name).read_text(encoding="utf-8"))
        if name == "initialize-2024-11-05.json":
            # The SDK implementation version may move from 1.28.x to 1.29.x without changing the
            # lifecycle contract. Protocol version, capabilities, server name, schemas, defaults,
            # descriptions, validations, and server_info remain exact below.
            actual_value["result"]["serverInfo"]["version"] = "<IMPLEMENTATION_VERSION>"
            expected["result"]["serverInfo"]["version"] = "<IMPLEMENTATION_VERSION>"
        assert normalize_diagnostic_urls(actual_value) == normalize_diagnostic_urls(expected), f"Python STDIO contract drifted: {name}"


def test_frozen_contract_ignores_only_diagnostic_url_version() -> None:
    before = {"result": {"content": [{"type": "text", "text":
        "Input should be a valid string [type=string_type, input_value=1024, input_type=int]\n"
        "https://errors.pydantic.dev/2.13/v/string_type"}], "isError": True}}
    after = json.loads(json.dumps(before).replace("/2.13/v/", "/2.14/v/"))
    assert normalize_diagnostic_urls(before) == normalize_diagnostic_urls(after)
    for changed in (
        json.loads(json.dumps(after).replace("string_type", "list_type")),
        json.loads(json.dumps(after).replace("input_value=1024", "input_value=2048")),
        {"result": {**after["result"], "isError": False}},
    ):
        assert normalize_diagnostic_urls(before) != normalize_diagnostic_urls(changed)
