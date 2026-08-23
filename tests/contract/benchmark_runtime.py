"""Same-machine Python/Rust startup and peak-RSS benchmark using only local fixtures."""
from __future__ import annotations

import argparse
import contextlib
import json
import os
import platform
import shlex
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.mock_micu_api import MockMicuApi, png_bytes
from tests.contract.stdio_driver import StdioSession, isolated_server_env, text_content_json


def rss_kib(pid: int) -> int:
    result = subprocess.run(
        ["ps", "-o", "rss=", "-p", str(pid)],
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0 or not result.stdout.strip():
        raise RuntimeError(f"unable to read RSS for pid={pid}: {result.stderr.strip()}")
    return int(result.stdout.strip().split()[0])


def protocol_metrics(command: list[str], repeats: int) -> dict[str, Any]:
    samples = []
    for _ in range(repeats):
        with tempfile.TemporaryDirectory(prefix="micu-benchmark-protocol-") as temp_dir:
            root = Path(temp_dir).resolve()
            started = time.perf_counter()
            with StdioSession(command, isolated_server_env(root), REPO_ROOT, timeout=30) as session:
                session.initialize()
                initialize_ms = (time.perf_counter() - started) * 1000
                tools_started = time.perf_counter()
                session.request(2, "tools/list", {})
                tools_list_ms = (time.perf_counter() - tools_started) * 1000
                time.sleep(0.05)
                assert session.process is not None
                idle_rss = rss_kib(session.process.pid)
                info_started = time.perf_counter()
                session.request(3, "tools/call", {"name": "server_info", "arguments": {}})
                server_info_ms = (time.perf_counter() - info_started) * 1000
                server_info_rss = rss_kib(session.process.pid)
            samples.append(
                {
                    "initialize_ms": round(initialize_ms, 3),
                    "tools_list_ms": round(tools_list_ms, 3),
                    "idle_rss_kib": idle_rss,
                    "server_info_ms": round(server_info_ms, 3),
                    "server_info_rss_kib": server_info_rss,
                }
            )
    return {
        "samples": samples,
        "median": {
            key: round(statistics.median(sample[key] for sample in samples), 3)
            for key in samples[0]
        },
    }


def call_with_peak_rss(
    session: StdioSession,
    request_id: int,
    params: dict[str, Any],
) -> tuple[dict[str, Any], int, float]:
    assert session.process is not None
    holder: dict[str, Any] = {}

    def invoke() -> None:
        try:
            holder["response"] = session.request(request_id, "tools/call", params)
        except BaseException as error:  # surfaced in the benchmark thread below
            holder["error"] = error

    started = time.perf_counter()
    peak = rss_kib(session.process.pid)
    thread = threading.Thread(target=invoke)
    thread.start()
    while thread.is_alive():
        peak = max(peak, rss_kib(session.process.pid))
        thread.join(timeout=0.005)
    wall_ms = (time.perf_counter() - started) * 1000
    if "error" in holder:
        raise holder["error"]
    response = holder.get("response")
    if not isinstance(response, dict):
        raise RuntimeError("tool call produced no response")
    peak = max(peak, rss_kib(session.process.pid))
    return response, peak, wall_ms


def heavy_metric(command: list[str], scenario: str) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix=f"micu-benchmark-{scenario}-") as temp_dir:
        case_root = Path(temp_dir).resolve()
        output_root = case_root / "out"
        input_root = case_root / "input"
        home_root = case_root / "home"
        output_root.mkdir()
        input_root.mkdir()
        home_root.mkdir()
        with MockMicuApi() as mock:
            overrides = mock.env()
            overrides.update(
                {
                    "HOME": str(home_root),
                    "USERPROFILE": str(home_root),
                    "MICU_INPUT_ROOT": str(input_root),
                }
            )
            if scenario == "large_url":
                overrides["MICU_RESPONSE_FORMAT"] = "url"
                arguments = {
                    "prompt": "benchmark [scenario:large_url]",
                    "size": "1024x1024",
                    "basename": "large_url",
                }
                tool = "image_generate"
            elif scenario == "large_b64":
                overrides["MICU_RESPONSE_FORMAT"] = "b64_json"
                arguments = {
                    "prompt": "benchmark [scenario:large_b64]",
                    "size": "1024x1024",
                    "basename": "large_b64",
                }
                tool = "image_generate"
            elif scenario == "near_json_cap":
                overrides["MICU_RESPONSE_FORMAT"] = "url"
                arguments = {
                    "prompt": "benchmark [scenario:near_json_cap]",
                    "size": "1024x1024",
                    "basename": "near_cap",
                }
                tool = "image_generate"
            elif scenario == "multi_8mib":
                overrides["MICU_RESPONSE_FORMAT"] = "b64_json"
                base = png_bytes()
                each_size = 4 * 1024 * 1024 - 1024
                padded = base + (b"\x00" * (each_size - len(base)))
                paths = []
                for index in range(2):
                    path = input_root / f"large-ref-{index}.png"
                    path.write_bytes(padded)
                    paths.append(str(path))
                arguments = {
                    "prompt": "benchmark [scenario:b64]",
                    "image_paths": paths,
                    "size": "1024x1024",
                    "basename": "multi_8mib",
                }
                tool = "image_multi_reference"
            else:
                raise ValueError(scenario)

            env = isolated_server_env(output_root, overrides)
            with StdioSession(command, env, REPO_ROOT, timeout=180) as session:
                session.initialize()
                assert session.process is not None
                idle = rss_kib(session.process.pid)
                response, peak, wall_ms = call_with_peak_rss(
                    session,
                    10,
                    {"name": tool, "arguments": arguments},
                )
                final_rss = rss_kib(session.process.pid)
            value = text_content_json(response)
            ok = value.get("ok") if isinstance(value, dict) else None
            return {
                "idle_rss_kib": idle,
                "peak_rss_kib": peak,
                "peak_delta_kib": peak - idle,
                "final_rss_kib": final_rss,
                "wall_ms": round(wall_ms, 3),
                "tool_ok": ok,
                "mock_request_count": len(mock.state.snapshot()),
            }


def multiprocess_idle(command: list[str], count: int) -> dict[str, Any]:
    with contextlib.ExitStack() as stack:
        sessions = []
        roots = []
        for index in range(count):
            temp_dir = stack.enter_context(
                tempfile.TemporaryDirectory(prefix=f"micu-benchmark-multiprocess-{index}-")
            )
            root = Path(temp_dir).resolve()
            roots.append(root)
            session = stack.enter_context(
                StdioSession(command, isolated_server_env(root), REPO_ROOT, timeout=30)
            )
            session.initialize()
            sessions.append(session)
        time.sleep(0.05)
        rss_values = []
        for session in sessions:
            assert session.process is not None
            rss_values.append(rss_kib(session.process.pid))
        return {
            "count": count,
            "rss_kib_each": rss_values,
            "total_rss_kib": sum(rss_values),
        }


def command_version(command: list[str]) -> str:
    result = subprocess.run(command, check=False, capture_output=True, text=True)
    return (result.stdout or result.stderr).strip().splitlines()[0]


def benchmark(command: list[str], repeats: int, process_count: int) -> dict[str, Any]:
    return {
        "command": command,
        "protocol": protocol_metrics(command, repeats),
        "large_url": heavy_metric(command, "large_url"),
        "large_b64": heavy_metric(command, "large_b64"),
        "multi_8mib": heavy_metric(command, "multi_8mib"),
        "near_json_cap": heavy_metric(command, "near_json_cap"),
        "multiprocess_idle": multiprocess_idle(command, process_count),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--python-command",
        default=f"{shlex.quote(sys.executable)} {shlex.quote(str(REPO_ROOT / 'server.py'))}",
    )
    parser.add_argument(
        "--rust-command",
        default=str(REPO_ROOT / "target" / "release" / "micu-image-mcp"),
    )
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--process-count", type=int, default=4)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if os.environ.get("MICU_RUN_LIVE_TESTS") == "1":
        raise SystemExit("benchmark refuses MICU_RUN_LIVE_TESTS=1; it is strictly offline")
    python_command = shlex.split(args.python_command)
    rust_command = shlex.split(args.rust_command)
    rust_binary = Path(rust_command[0])
    if not rust_binary.is_file():
        raise SystemExit(f"release binary missing: {rust_binary}")
    payload = {
        "meta": {
            "timestamp": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "os": platform.platform(),
            "architecture": platform.machine(),
            "python": platform.python_version(),
            "rustc": command_version(["rustc", "--version"]),
            "cargo": command_version(["cargo", "--version"]),
            "rust_profile": "release (thin LTO, codegen-units=1, stripped)",
            "protocol_repeats": args.repeat,
            "multiprocess_count": args.process_count,
            "release_binary_bytes": rust_binary.stat().st_size,
        },
        "python": benchmark(python_command, args.repeat, args.process_count),
        "rust": benchmark(rust_command, args.repeat, args.process_count),
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(args.output)


if __name__ == "__main__":
    main()
