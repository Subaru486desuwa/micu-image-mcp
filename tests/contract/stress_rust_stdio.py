from __future__ import annotations

import argparse
import json
import shlex
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.mock_micu_api import MockMicuApi
from tests.contract.stdio_driver import StdioSession, isolated_server_env, text_content_json


@dataclass(frozen=True)
class PhaseResult:
    name: str
    processes: int
    images_per_call: int
    rounds: int
    calls: int
    expected_api_requests: int
    captured_api_requests: int
    saved_images: int
    max_active_api_requests: int
    wall_seconds: float
    stdout_frames: int
    stderr_secret_leak: bool
    failures: list[str]


def _run_phase(
    command: list[str],
    root: Path,
    *,
    name: str,
    processes: int,
    images_per_call: int,
    rounds: int,
    size: str,
    model: str,
) -> PhaseResult:
    shared_home = root / name / "home"
    shared_home.mkdir(parents=True)
    failures: list[str] = []
    saved_images = 0
    stdout_frames = 0
    stderr_secret_leak = False
    captured_requests = 0
    phase_max_active = 0
    started = time.perf_counter()

    for round_index in range(rounds):
        with MockMicuApi() as mock:
            barrier = threading.Barrier(processes)

            def worker(process_index: int) -> dict[str, Any]:
                output_root = root / name / f"round-{round_index}" / f"process-{process_index}"
                output_root.mkdir(parents=True)
                overrides = mock.env()
                overrides.update(
                    {
                        "HOME": str(shared_home),
                        "USERPROFILE": str(shared_home),
                    }
                )
                env = isolated_server_env(output_root, overrides)
                with StdioSession(command, env, REPO_ROOT, timeout=90) as session:
                    session.initialize()
                    barrier.wait(timeout=30)
                    response = session.request(
                        10,
                        "tools/call",
                        {
                            "name": "image_generate",
                            "arguments": {
                                "prompt": f"Rust stress {name} [scenario:concurrency_probe]",
                                "size": size,
                                "n": images_per_call,
                                "model": model,
                                "basename": f"stress_{round_index}_{process_index}",
                            },
                        },
                    )
                result = text_content_json(response)
                return {
                    "ok": result.get("ok"),
                    "saved": len(result.get("saved", [])),
                    "errors": result.get("errors", []),
                    "stdout_frames": len(session.stdout_lines),
                    "stderr_secret": mock.expected_key.encode() in bytes(session.stderr_bytes),
                }

            with ThreadPoolExecutor(max_workers=processes) as executor:
                results = list(executor.map(worker, range(processes)))
            for process_index, result in enumerate(results):
                if not result["ok"] or result["errors"]:
                    failures.append(
                        f"round={round_index} process={process_index}: {result['errors']}"
                    )
                saved_images += int(result["saved"])
                stdout_frames += int(result["stdout_frames"])
                stderr_secret_leak = stderr_secret_leak or bool(result["stderr_secret"])
            requests = [
                request
                for request in mock.state.snapshot()
                if str(request.get("path", "")).startswith("/v1/images/")
            ]
            captured_requests += len(requests)
            phase_max_active = max(
                phase_max_active,
                int(mock.state.metrics()["max_active_api_requests"]),
            )

    calls = processes * rounds
    expected_requests = calls * images_per_call
    return PhaseResult(
        name=name,
        processes=processes,
        images_per_call=images_per_call,
        rounds=rounds,
        calls=calls,
        expected_api_requests=expected_requests,
        captured_api_requests=captured_requests,
        saved_images=saved_images,
        max_active_api_requests=phase_max_active,
        wall_seconds=round(time.perf_counter() - started, 3),
        stdout_frames=stdout_frames,
        stderr_secret_leak=stderr_secret_leak,
        failures=failures,
    )


def run(command: list[str], output: Path, processes: int, rounds: int) -> dict[str, Any]:
    with tempfile.TemporaryDirectory(prefix="micu-rust-stress-") as temp_dir:
        root = Path(temp_dir).resolve()
        standard = _run_phase(
            command,
            root,
            name="standard-1k",
            processes=processes,
            images_per_call=5,
            rounds=rounds,
            size="1024x1024",
            model="gpt-image-2",
        )
        quality = _run_phase(
            command,
            root,
            name="quality-cross-process-lock",
            processes=min(processes, 5),
            images_per_call=1,
            rounds=max(1, min(rounds, 2)),
            size="2048x2048",
            model="gpt-image-2-openai",
        )

    assertions = {
        "standard_all_calls_succeeded": not standard.failures,
        "standard_all_requests_captured": (
            standard.captured_api_requests == standard.expected_api_requests
        ),
        "standard_all_images_saved": standard.saved_images == standard.expected_api_requests,
        "standard_observed_concurrency": standard.max_active_api_requests >= 5,
        "quality_all_calls_succeeded": not quality.failures,
        "quality_all_requests_captured": quality.captured_api_requests == quality.expected_api_requests,
        "quality_all_images_saved": quality.saved_images == quality.expected_api_requests,
        "quality_cross_process_serial": quality.max_active_api_requests == 1,
        "stdout_json_rpc_only": (
            standard.stdout_frames == standard.calls * 2
            and quality.stdout_frames == quality.calls * 2
        ),
        "no_secret_on_stderr": not standard.stderr_secret_leak and not quality.stderr_secret_leak,
    }
    payload = {
        "command": command,
        "standard": asdict(standard),
        "quality": asdict(quality),
        "assertions": assertions,
        "status": "PASS" if all(assertions.values()) else "FAIL",
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    if payload["status"] != "PASS":
        raise AssertionError(json.dumps(payload, ensure_ascii=False, indent=2))
    return payload


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--rust-command",
        default=str(REPO_ROOT / "target" / "release" / "micu-image-mcp"),
    )
    parser.add_argument("--processes", type=int, default=8)
    parser.add_argument("--rounds", type=int, default=3)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not 2 <= args.processes <= 16:
        raise SystemExit("--processes must be in [2, 16]")
    if not 1 <= args.rounds <= 10:
        raise SystemExit("--rounds must be in [1, 10]")
    command = shlex.split(args.rust_command)
    if not command:
        raise SystemExit("--rust-command must not be empty")
    payload = run(command, args.output, args.processes, args.rounds)
    print(
        "Rust stress: PASS; "
        f"standard={payload['standard']['captured_api_requests']} requests, "
        f"quality max_active={payload['quality']['max_active_api_requests']}"
    )


if __name__ == "__main__":
    main()
