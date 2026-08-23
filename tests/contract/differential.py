"""Black-box Python/Rust differential runner.

Normalization is deliberately narrow: JSON object order, implementation version,
SDK-specific parameter diagnostics, and the two documented Rust security/runtime
status strings.  Tool schemas, HTTP captures, saved bytes, public fields, status
codes, retry counts, Chinese notes, and filenames remain compared.
"""
from __future__ import annotations

import argparse
import json
import re
import shlex
import sys
from pathlib import Path
from typing import Any

from tests.contract.capture_python_fixtures import REPO_ROOT, collect
from tests.contract.contract_cases import case_names, run_case


PYTHON_FIXTURES = Path(__file__).resolve().parent / "fixtures" / "python"
RUST_FIXTURES = Path(__file__).resolve().parent / "fixtures" / "rust"


def normalize(value: Any) -> Any:
    if isinstance(value, list):
        return [normalize(item) for item in value]
    if not isinstance(value, dict):
        return normalize_text(value) if isinstance(value, str) else value
    if value.get("type") == "text" and isinstance(value.get("text"), str):
        normalized = {key: normalize(item) for key, item in value.items() if key != "text"}
        raw_text = value["text"]
        try:
            normalized["text"] = normalize(json.loads(raw_text))
        except json.JSONDecodeError:
            normalized["text"] = normalize_text(raw_text)
        return normalized
    normalized = {key: normalize(item) for key, item in value.items()}
    return normalized


def normalize_text(text: str) -> str:
    if "validation error for" in text or "参数校验失败 (" in text:
        return "<PARAMETER_VALIDATION_ERROR>"
    text = re.sub(
        r"(HTTP 0 可重试，等待 [^；]+；原因：).*",
        r"\1<NETWORK_ERROR>",
        text,
        flags=re.DOTALL,
    )
    text = re.sub(r"(#\d+ HTTP 0: ).*", r"\1<NETWORK_ERROR>", text, flags=re.DOTALL)
    text = re.sub(
        r"(?:ConnectError|RequestError|NetworkError|RemoteProtocolError|ReadTimeout|TimeoutError): [^；\n]+",
        "<NETWORK_ERROR>",
        text,
    )
    text = re.sub(
        r"Redirect response .*?(?=）→|$)",
        "<REDIRECT_REJECTED>",
        text,
        flags=re.DOTALL,
    )
    return text


def normalize_stdio(name: str, value: Any) -> Any:
    value = normalize(value)
    if name == "initialize-2024-11-05.json":
        value["result"]["serverInfo"]["version"] = "<IMPLEMENTATION_VERSION>"
    if name == "server-info.json":
        structured = value["result"]["structuredContent"]
        structured["retry_policy"]["concurrency_2k_4k"] = "<RUNTIME_LOCK_DESCRIPTION>"
        structured["safety_constraints"]["input_image_validation"] = "<IMAGE_VALIDATION_DESCRIPTION>"
        content = value["result"].get("content") or []
        if content and isinstance(content[0], dict):
            content[0]["text"] = structured
    return value


def fixture_payload(root: Path, name: str) -> Any:
    return json.loads((root / name).read_text(encoding="utf-8"))


def assert_equal(left: Any, right: Any, label: str) -> None:
    differences: list[str] = []
    _collect_differences(left, right, "$", differences, limit=30)
    if differences:
        rendered = "\n".join(differences)
        raise AssertionError(f"{label} differs:\n{rendered}")


def _collect_differences(left: Any, right: Any, path: str, output: list[str], limit: int) -> None:
    if len(output) >= limit:
        return
    if type(left) is not type(right):
        output.append(f"{path}: type {type(left).__name__} != {type(right).__name__}")
        return
    if isinstance(left, dict):
        left_keys = set(left)
        right_keys = set(right)
        for key in sorted(left_keys - right_keys):
            output.append(f"{path}.{key}: missing on right")
        for key in sorted(right_keys - left_keys):
            output.append(f"{path}.{key}: missing on left")
        for key in sorted(left_keys & right_keys):
            _collect_differences(left[key], right[key], f"{path}.{key}", output, limit)
        return
    if isinstance(left, list):
        if len(left) != len(right):
            output.append(f"{path}: len {len(left)} != {len(right)}")
        for index, (left_item, right_item) in enumerate(zip(left, right)):
            _collect_differences(left_item, right_item, f"{path}[{index}]", output, limit)
        return
    if left != right:
        output.append(f"{path}: {left!r} != {right!r}")


def compare_fixture_files() -> None:
    for name in (
        "initialize-2024-11-05.json",
        "tools-list.json",
        "server-info.json",
        "validation-calls.json",
    ):
        left = normalize_stdio(name, fixture_payload(PYTHON_FIXTURES, name))
        right = normalize_stdio(name, fixture_payload(RUST_FIXTURES, name))
        assert_equal(left, right, f"stdio fixture {name}")
    left_cases = fixture_payload(PYTHON_FIXTURES, "mock-cases.json")
    right_cases = fixture_payload(RUST_FIXTURES, "mock-cases.json")
    assert_equal(normalize(left_cases), normalize(right_cases), "mock API fixtures")


def compare_live(python_command: list[str], rust_command: list[str], selected: list[str]) -> None:
    python_stdio = collect(python_command)
    rust_stdio = collect(rust_command)
    for name in python_stdio:
        assert_equal(
            normalize_stdio(name, python_stdio[name]),
            normalize_stdio(name, rust_stdio[name]),
            f"live stdio {name}",
        )
    for name in selected:
        assert_equal(
            normalize(run_case(python_command, name)),
            normalize(run_case(rust_command, name)),
            f"live mock case {name}",
        )


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--fixtures-only", action="store_true")
    parser.add_argument(
        "--python-command",
        default=f"{shlex.quote(sys.executable)} {shlex.quote(str(REPO_ROOT / 'server.py'))}",
    )
    parser.add_argument(
        "--rust-command",
        default=str(REPO_ROOT / "target" / "debug" / "micu-image-mcp"),
    )
    parser.add_argument("--case", choices=case_names(), action="append")
    args = parser.parse_args()
    if args.fixtures_only:
        compare_fixture_files()
    else:
        compare_live(
            shlex.split(args.python_command),
            shlex.split(args.rust_command),
            args.case or case_names(),
        )
    print("Python/Rust differential contract: PASS")


if __name__ == "__main__":
    main()
