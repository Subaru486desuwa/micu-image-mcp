"""Guard the HEAD-era Python reference fixtures against accidental drift."""
from __future__ import annotations

import json
import sys
from pathlib import Path

from tests.contract.capture_python_fixtures import FIXTURE_ROOT, REPO_ROOT, collect


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
        assert actual_value == expected, f"Python STDIO contract drifted: {name}"
