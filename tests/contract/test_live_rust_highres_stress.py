from __future__ import annotations

import json
import os
import re
import sys
import tempfile
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.stdio_driver import StdioSession, text_content_json


RUST_BINARY = REPO_ROOT / "target" / "release" / (
    "micu-image-mcp.exe" if sys.platform == "win32" else "micu-image-mcp"
)


@dataclass(frozen=True)
class HighResolutionScenario:
    name: str
    size: str
    prompt: str


SCENARIOS = (
    HighResolutionScenario(
        "2k-landscape",
        "2048x1152",
        "A single blue circle centered on a white background, flat vector stress test",
    ),
    HighResolutionScenario(
        "2k-portrait",
        "1152x2048",
        "A single green triangle centered on a white background, flat vector stress test",
    ),
    HighResolutionScenario(
        "2k-square",
        "2048x2048",
        "A single red square centered on a white background, flat vector stress test",
    ),
    HighResolutionScenario(
        "4k-landscape",
        "3840x2160",
        "A single purple hexagon centered on a white background, flat vector stress test",
    ),
    HighResolutionScenario(
        "4k-portrait",
        "2160x3840",
        "A single orange star centered on a white background, flat vector stress test",
    ),
)


def _redact(text: str) -> str:
    text = re.sub(
        r"(?i)authorization\s*:\s*bearer\s+\S+",
        "Authorization: [REDACTED]",
        text,
    )
    text = re.sub(r"(?i)bearer\s+\S+", "Bearer [REDACTED]", text)
    return re.sub(r"sk-[A-Za-z0-9_-]+", "[REDACTED]", text)


def _redact_value(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: _redact_value(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_redact_value(item) for item in value]
    if isinstance(value, str):
        return _redact(value)
    return value


def _stderr_is_safe(stderr: bytes) -> bool:
    lowered = stderr.lower()
    return (
        b"authorization:" not in lowered
        and b"bearer " not in lowered
        and b"sk-" not in lowered
    )


def _lock_description(info: Any) -> str:
    if not isinstance(info, dict):
        return ""
    retry = info.get("retry_policy")
    if not isinstance(retry, dict):
        return ""
    value = retry.get("concurrency_2k_4k")
    return value if isinstance(value, str) else ""


def test_highres_live_report_redacts_sensitive_failure_text() -> None:
    value = _redact_value(
        {
            "errors": ["Authorization: Bearer sk-test-secret"],
            "nested": {"detail": "bearer another-token"},
        }
    )
    serialized = json.dumps(value)
    assert "sk-test-secret" not in serialized
    assert "another-token" not in serialized
    assert serialized.count("[REDACTED]") == 2


@pytest.mark.skipif(
    not (
        os.environ.get("MICU_RUN_LIVE_TESTS") == "1"
        and os.environ.get("MICU_RUN_LIVE_STRESS") == "1"
        and os.environ.get("MICU_RUN_LIVE_HIGHRES_STRESS") == "1"
    ),
    reason=(
        "paid 2K/4K stress requires MICU_RUN_LIVE_TESTS=1, "
        "MICU_RUN_LIVE_STRESS=1 and MICU_RUN_LIVE_HIGHRES_STRESS=1"
    ),
)
def test_real_rust_2k_4k_cross_process_pressure_matrix() -> None:
    """Run all recommended 2K/4K shapes at once through independent MCP processes."""

    assert RUST_BINARY.is_file(), f"release binary missing: {RUST_BINARY}"
    with tempfile.TemporaryDirectory(prefix="micu-rust-live-highres-") as temp_dir:
        root = Path(temp_dir).resolve()
        barrier = threading.Barrier(len(SCENARIOS))
        suite_started = time.perf_counter()

        def worker(index: int) -> dict[str, Any]:
            scenario = SCENARIOS[index]
            output_root = root / "outputs" / scenario.name
            output_root.mkdir(parents=True)
            env = os.environ.copy()
            env.update(
                {
                    "MICU_BASEURL": "https://www.micuapi.ai",
                    "MICU_SAVE_DIR": str(output_root),
                    "MICU_SAVE_DIR_ROOT": str(output_root),
                    "MICU_MODEL": "gpt-image-2",
                    "MICU_USE_SHELL_PROXY": "0",
                    "MICU_RUN_LIVE_TESTS": "1",
                    "MICU_RUN_LIVE_STRESS": "1",
                    "MICU_RUN_LIVE_HIGHRES_STRESS": "1",
                }
            )
            started = time.perf_counter()
            try:
                with StdioSession([str(RUST_BINARY)], env, REPO_ROOT, timeout=3600) as session:
                    session.initialize("2024-11-05")
                    info_response = session.request(
                        2,
                        "tools/call",
                        {"name": "server_info", "arguments": {}},
                    )
                    info = text_content_json(info_response)
                    barrier.wait(timeout=120)
                    response = session.request(
                        3,
                        "tools/call",
                        {
                            "name": "image_generate",
                            "arguments": {
                                "prompt": scenario.prompt,
                                "size": scenario.size,
                                "n": 3,
                                "quality": "high",
                                "model": "gpt-image-2",
                                "basename": f"live_highres_{scenario.name}",
                            },
                        },
                    )
                result = text_content_json(response)
                saved = result.get("saved", []) if isinstance(result, dict) else []
                saved_item = saved[0] if len(saved) == 1 and isinstance(saved[0], dict) else {}
                result_errors = _redact_value(
                    result.get("errors", []) if isinstance(result, dict) else []
                )
                tool_error = None
                if not isinstance(result, dict):
                    tool_error = _redact(str(result))
                elif "error" in response:
                    tool_error = _redact(json.dumps(response["error"], ensure_ascii=False))
                saved_path_raw = saved_item.get("path")
                saved_path = Path(saved_path_raw) if isinstance(saved_path_raw, str) else None
                notes = _redact_value(result.get("notes", []) if isinstance(result, dict) else [])
                lock_description = _lock_description(info)
                checks = {
                    "ok": isinstance(result, dict) and result.get("ok") is True,
                    "auto_routed_quality_model": (
                        isinstance(result, dict) and result.get("model") == "gpt-image-2-openai"
                    ),
                    "n_forced_to_one": (
                        isinstance(result, dict) and result.get("requested_n") == 1
                    ),
                    "no_errors": isinstance(result, dict) and result_errors == [],
                    "one_image_saved": len(saved) == 1,
                    "size_honored": (
                        isinstance(result, dict)
                        and result.get("size_honored") is True
                        and saved_item.get("actual_size") == scenario.size
                    ),
                    "file_is_valid_output": (
                        saved_path is not None
                        and saved_path.is_file()
                        and saved_path.is_relative_to(output_root)
                        and int(saved_item.get("size_bytes", 0)) > 0
                    ),
                    "forced_n_note": any(
                        isinstance(note, str) and "强制 N=1" in note for note in notes
                    ),
                    "api_key_configured": (
                        isinstance(info, dict) and info.get("api_key_configured") is True
                    ),
                    "shared_lock_reported": "跨进程 fs4" in lock_description,
                    "stdout_json_rpc_only": len(session.stdout_lines) == 3,
                    "stderr_secret_free": _stderr_is_safe(bytes(session.stderr_bytes)),
                }
                return {
                    "name": scenario.name,
                    "binary_version": info.get("version") if isinstance(info, dict) else None,
                    "requested_size": scenario.size,
                    "effective_model": result.get("model") if isinstance(result, dict) else None,
                    "requested_n": result.get("requested_n") if isinstance(result, dict) else None,
                    "size_honored": result.get("size_honored") if isinstance(result, dict) else None,
                    "actual_size": saved_item.get("actual_size"),
                    "actual_megapixels": saved_item.get("actual_megapixels"),
                    "size_bytes": saved_item.get("size_bytes"),
                    "errors": result_errors,
                    "tool_error": tool_error,
                    "notes": notes,
                    "lock_description": lock_description,
                    "wall_seconds": round(time.perf_counter() - started, 3),
                    "completed_offset_seconds": round(time.perf_counter() - suite_started, 3),
                    "checks": checks,
                    "error": None,
                }
            except Exception as error:  # noqa: BLE001 - preserve all live worker failures
                return {
                    "name": scenario.name,
                    "requested_size": scenario.size,
                    "wall_seconds": round(time.perf_counter() - started, 3),
                    "completed_offset_seconds": round(time.perf_counter() - suite_started, 3),
                    "checks": {},
                    "error": _redact(str(error)),
                }

        with ThreadPoolExecutor(max_workers=len(SCENARIOS)) as executor:
            results = list(executor.map(worker, range(len(SCENARIOS))))

        lock_descriptions = {
            item.get("lock_description") for item in results if item.get("lock_description")
        }
        binary_versions = {
            item.get("binary_version") for item in results if item.get("binary_version")
        }
        queue_waiters = sum(
            any(
                isinstance(note, str) and "等待跨进程 ≥2K 锁" in note
                for note in item.get("notes", [])
            )
            for item in results
        )
        all_checks_passed = all(
            item.get("error") is None
            and item.get("checks")
            and all(item["checks"].values())
            for item in results
        )
        aggregate_checks = {
            "all_five_calls_passed": all_checks_passed,
            "one_binary_version": len(binary_versions) == 1,
            "all_processes_share_one_lock": len(lock_descriptions) == 1,
            "cross_process_queue_observed": queue_waiters >= len(SCENARIOS) - 1,
            "all_2k_4k_sizes_exact": all(
                item.get("actual_size") == item.get("requested_size") for item in results
            ),
        }
        report = {
            "status": "PASS" if all(aggregate_checks.values()) else "FAIL",
            "binary_version": next(iter(binary_versions), None),
            "provider_base_url": "https://www.micuapi.ai",
            "processes": len(SCENARIOS),
            "requested_images": len(SCENARIOS),
            "successful_images": sum(
                bool(item.get("checks", {}).get("one_image_saved")) for item in results
            ),
            "queue_waiters": queue_waiters,
            "wall_seconds": round(time.perf_counter() - suite_started, 3),
            "aggregate_checks": aggregate_checks,
            "scenarios": results,
        }
        report_path = os.environ.get("MICU_LIVE_HIGHRES_STRESS_REPORT")
        if report_path:
            home_text = str(Path.home())
            serialized = json.dumps(report, ensure_ascii=False, indent=2)
            serialized = serialized.replace(home_text, "<HOME>")
            Path(report_path).write_text(serialized + "\n", encoding="utf-8")

        assert all(aggregate_checks.values()), json.dumps(
            report, ensure_ascii=False, indent=2
        )
