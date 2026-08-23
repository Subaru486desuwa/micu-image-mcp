"""Capture the current Python reference server's MCP behavior.

The files are canonical JSON snapshots: object key order is normalized, while
tool descriptions, required fields, defaults, schema keywords, and values are
kept byte-for-byte as JSON strings/scalars.
"""
from __future__ import annotations

import argparse
import json
import shlex
import sys
import tempfile
from pathlib import Path
from typing import Any

try:
    from tests.contract.stdio_driver import StdioSession, canonicalize, isolated_server_env
except ModuleNotFoundError:  # direct script execution keeps only this directory on sys.path
    from stdio_driver import StdioSession, canonicalize, isolated_server_env


REPO_ROOT = Path(__file__).resolve().parents[2]
FIXTURE_ROOT = Path(__file__).resolve().parent / "fixtures" / "python"


VALIDATION_CASES: list[tuple[str, str, dict[str, Any]]] = [
    ("generate_empty_prompt", "image_generate", {"prompt": ""}),
    ("generate_n_over_limit", "image_generate", {"prompt": "x", "n": 11}),
    ("generate_n_bool", "image_generate", {"prompt": "x", "n": True}),
    ("generate_invalid_size", "image_generate", {"prompt": "x", "size": "1920x1080"}),
    ("generate_non_string_size", "image_generate", {"prompt": "x", "size": 1024}),
    ("generate_invalid_quality", "image_generate", {"prompt": "x", "quality": "ultra"}),
    ("generate_non_string_quality", "image_generate", {"prompt": "x", "quality": 1}),
    ("generate_grok_disabled", "image_generate", {"prompt": "x", "model": "grok-imagine-image"}),
    ("generate_bad_basename", "image_generate", {"prompt": "x", "basename": "../escape"}),
    ("generate_missing_key", "image_generate", {"prompt": "x", "size": "1024x1024"}),
    ("edit_missing_image", "image_edit", {"prompt": "x", "image_path": "/definitely/missing.png"}),
    ("batch_empty_images", "image_batch_edit", {"prompt": "x", "image_paths": []}),
    ("batch_non_list_images", "image_batch_edit", {"prompt": "x", "image_paths": "one.png"}),
    ("multi_reference_too_few", "image_multi_reference", {"prompt": "x", "image_paths": ["one.png"]}),
    (
        "multi_reference_too_many",
        "image_multi_reference",
        {"prompt": "x", "image_paths": [f"{index}.png" for index in range(11)]},
    ),
]


def _write(root: Path, name: str, value: Any) -> None:
    root = root.resolve()
    root.mkdir(parents=True, exist_ok=True)
    path = root / name
    path.write_text(
        json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(path.relative_to(REPO_ROOT))


def collect(command: list[str]) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="micu-contract-") as temp_dir:
        save_root = Path(temp_dir).resolve()
        replacements = [
            (str(save_root), "<SAVE_ROOT>"),
            (str(REPO_ROOT), "<STARTUP_CWD>"),
        ]
        env = isolated_server_env(save_root)
        with StdioSession(command=command, env=env, cwd=REPO_ROOT) as session:
            initialize = session.initialize("2024-11-05")
            tools = session.request(2, "tools/list", {})
            server_info = session.request(
                3,
                "tools/call",
                {"name": "server_info", "arguments": {}},
            )
            validations: dict[str, Any] = {}
            next_id = 10
            for case_name, tool_name, arguments in VALIDATION_CASES:
                validations[case_name] = session.request(
                    next_id,
                    "tools/call",
                    {"name": tool_name, "arguments": arguments},
                )
                next_id += 1

        return {
            "initialize-2024-11-05.json": canonicalize(initialize, replacements),
            "tools-list.json": canonicalize(tools, replacements),
            "server-info.json": canonicalize(server_info, replacements),
            "validation-calls.json": canonicalize(validations, replacements),
        }


def capture(command: list[str], output_root: Path = FIXTURE_ROOT) -> None:
    for name, value in collect(command).items():
        _write(output_root, name, value)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--server-command",
        default=f"{shlex.quote(sys.executable)} {shlex.quote(str(REPO_ROOT / 'server.py'))}",
    )
    parser.add_argument("--output-dir", type=Path, default=FIXTURE_ROOT)
    args = parser.parse_args()
    command = shlex.split(args.server_command)
    if not command:
        raise SystemExit("--server-command must not be empty")
    capture(command, args.output_dir)


if __name__ == "__main__":
    main()
