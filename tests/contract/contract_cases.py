"""Protocol-level cases that run unchanged against Python and Rust."""
from __future__ import annotations

import hashlib
import json
import re
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from tests.contract.mock_micu_api import MockMicuApi, png_bytes, rgba_png_bytes
from tests.contract.stdio_driver import StdioSession, canonicalize, isolated_server_env


REPO_ROOT = Path(__file__).resolve().parents[2]


@dataclass(frozen=True)
class CaseSpec:
    name: str
    tool: str
    arguments: dict[str, Any]
    setup: str = "none"
    calls: int = 1
    env: tuple[tuple[str, str], ...] = ()


CASES: tuple[CaseSpec, ...] = (
    CaseSpec(
        "generate_exact_b64",
        "image_generate",
        {
            "prompt": "精确尺寸 [scenario:exact_b64]",
            "size": "1024x1024",
            "basename": "exact",
        },
    ),
    CaseSpec(
        "generate_data_url",
        "image_generate",
        {"prompt": "data URL [scenario:data_url]", "size": "1024x1024", "basename": "data_url"},
    ),
    CaseSpec(
        "generate_url",
        "image_generate",
        {"prompt": "URL [scenario:url_success]", "size": "1024x1024", "basename": "url"},
    ),
    CaseSpec(
        "generate_url_without_content_length",
        "image_generate",
        {
            "prompt": "URL stream [scenario:url_no_content_length]",
            "size": "1024x1024",
            "basename": "url_stream",
        },
    ),
    CaseSpec(
        "generate_truncated_url_fallback",
        "image_generate",
        {
            "prompt": "truncated [scenario:url_truncated_then_b64]",
            "size": "1024x1024",
            "basename": "truncated",
        },
    ),
    CaseSpec(
        "generate_private_redirect_fallback",
        "image_generate",
        {
            "prompt": "redirect [scenario:url_redirect_private_then_b64]",
            "size": "1024x1024",
            "basename": "redirect",
        },
    ),
    CaseSpec(
        "generate_ssrf_loopback_rejected",
        "image_generate",
        {
            "prompt": "ssrf [scenario:url_loopback]",
            "size": "1024x1024",
            "basename": "ssrf_loopback",
        },
        env=(("MICU_RESPONSE_FORMAT", "url"),),
    ),
    CaseSpec(
        "generate_ssrf_mapped_ipv6_rejected",
        "image_generate",
        {
            "prompt": "ssrf [scenario:url_mapped_loopback]",
            "size": "1024x1024",
            "basename": "ssrf_mapped",
        },
        env=(("MICU_RESPONSE_FORMAT", "url"),),
    ),
    CaseSpec(
        "generate_400_too_many_retry",
        "image_generate",
        {
            "prompt": "retry [scenario:retry_400_too_many]",
            "size": "1024x1024",
            "model": "gpt-image-2-openai",
            "basename": "retry400",
        },
    ),
    CaseSpec(
        "generate_408_retry",
        "image_generate",
        {
            "prompt": "retry [scenario:retry_408]",
            "size": "1024x1024",
            "model": "gpt-image-2-openai",
            "basename": "retry408",
        },
    ),
    CaseSpec(
        "generate_429_retry_after_seconds",
        "image_generate",
        {
            "prompt": "retry [scenario:retry_after_seconds]",
            "size": "1024x1024",
            "model": "gpt-image-2-openai",
            "basename": "retry429",
        },
    ),
    CaseSpec(
        "generate_retry_after_http_date",
        "image_generate",
        {
            "prompt": "retry [scenario:retry_after_http_date]",
            "size": "1024x1024",
            "model": "gpt-image-2-openai",
            "basename": "retrydate",
        },
    ),
    CaseSpec(
        "generate_500_retry",
        "image_generate",
        {
            "prompt": "retry [scenario:retry_500]",
            "size": "1024x1024",
            "model": "gpt-image-2-openai",
            "basename": "retry500",
        },
    ),
    CaseSpec(
        "generate_524_large_fail_fast",
        "image_generate",
        {"prompt": "timeout [scenario:http_524]", "size": "2048x2048", "basename": "failfast"},
    ),
    CaseSpec(
        "generate_error_redacts_key_and_base64",
        "image_generate",
        {
            "prompt": "secret [scenario:secret_error]",
            "size": "1024x1024",
            "basename": "secret_error",
        },
        env=(("RUST_LOG", "trace"),),
    ),
    CaseSpec(
        "generate_network_disconnect_free_retry",
        "image_generate",
        {
            "prompt": "disconnect [scenario:disconnect_once]",
            "size": "1024x1024",
            "basename": "disconnect",
        },
    ),
    CaseSpec(
        "generate_mid_body_disconnect_free_retry",
        "image_generate",
        {
            "prompt": "disconnect [scenario:disconnect_body_once]",
            "size": "1024x1024",
            "basename": "disconnect_body",
        },
    ),
    CaseSpec(
        "generate_api_timeout_free_retry",
        "image_generate",
        {
            "prompt": "timeout [scenario:api_timeout]",
            "size": "1024x1024",
            "basename": "api_timeout",
        },
        env=(("MICU_CONTRACT_TESTING", "1"), ("MICU_TEST_API_TIMEOUT_MS", "50")),
    ),
    CaseSpec(
        "generate_content_length_cap",
        "image_generate",
        {
            "prompt": "oversize [scenario:content_length_too_large]",
            "size": "1024x1024",
            "basename": "content_length",
        },
    ),
    CaseSpec(
        "generate_stream_cap",
        "image_generate",
        {"prompt": "oversize [scenario:stream_too_large]", "size": "1024x1024", "basename": "stream_cap"},
    ),
    CaseSpec(
        "generate_invalid_json",
        "image_generate",
        {"prompt": "invalid [scenario:invalid_json]", "size": "1024x1024", "basename": "invalid_json"},
    ),
    CaseSpec(
        "generate_missing_payload",
        "image_generate",
        {"prompt": "empty [scenario:no_image]", "size": "1024x1024", "basename": "no_image"},
    ),
    CaseSpec(
        "edit_multipart_with_mask",
        "image_edit",
        {
            "prompt": "编辑中文 [scenario:b64]",
            "image_path": "<INPUT>/source.png",
            "mask_path": "<INPUT>/mask.png",
            "size": "1024x1024",
            "basename": "edit_contract",
        },
        setup="edit",
    ),
    CaseSpec(
        "edit_input_over_4mib",
        "image_edit",
        {
            "prompt": "x",
            "image_path": "<INPUT>/over-4m.png",
            "size": "1024x1024",
        },
        setup="over4",
    ),
    CaseSpec(
        "edit_truncated_input",
        "image_edit",
        {
            "prompt": "x",
            "image_path": "<INPUT>/truncated.png",
            "size": "1024x1024",
        },
        setup="truncated",
    ),
    CaseSpec(
        "edit_malformed_jpeg",
        "image_edit",
        {
            "prompt": "x",
            "image_path": "<INPUT>/malformed.jpg",
            "size": "1024x1024",
        },
        setup="malformed_jpeg",
    ),
    CaseSpec(
        "edit_malformed_webp",
        "image_edit",
        {
            "prompt": "x",
            "image_path": "<INPUT>/malformed.webp",
            "size": "1024x1024",
        },
        setup="malformed_webp",
    ),
    CaseSpec(
        "edit_decompression_bomb",
        "image_edit",
        {
            "prompt": "x",
            "image_path": "<INPUT>/bomb.png",
            "size": "1024x1024",
        },
        setup="bomb",
    ),
    CaseSpec(
        "edit_mask_wrong_size",
        "image_edit",
        {
            "prompt": "x",
            "image_path": "<INPUT>/source.png",
            "mask_path": "<INPUT>/wrong-mask.png",
            "size": "1024x1024",
        },
        setup="mask_wrong_size",
    ),
    CaseSpec(
        "edit_mask_without_alpha",
        "image_edit",
        {
            "prompt": "x",
            "image_path": "<INPUT>/source.png",
            "mask_path": "<INPUT>/rgb-mask.png",
            "size": "1024x1024",
        },
        setup="mask_no_alpha",
    ),
    CaseSpec(
        "edit_input_root_symlink_escape",
        "image_edit",
        {
            "prompt": "x",
            "image_path": "<SYMLINK_INPUT>",
            "size": "1024x1024",
        },
        setup="input_symlink",
    ),
    CaseSpec(
        "multi_reference_image_array",
        "image_multi_reference",
        {
            "prompt": "融合中文 [scenario:b64]",
            "image_paths": ["<INPUT>/ref-a.png", "<INPUT>/ref-b.webp"],
            "size": "1024x1024",
            "basename": "multi_contract",
        },
        setup="multi",
    ),
    CaseSpec(
        "multi_reference_total_over_8mib",
        "image_multi_reference",
        {
            "prompt": "x",
            "image_paths": [f"<INPUT>/large-{index}.png" for index in range(3)],
            "size": "1024x1024",
        },
        setup="multi_over8",
    ),
    CaseSpec(
        "generate_standard_concurrency_six",
        "image_generate",
        {
            "prompt": "并发 [scenario:concurrency_probe]",
            "size": "1024x1024",
            "n": 6,
            "basename": "generate_concurrent",
        },
    ),
    CaseSpec(
        "batch_standard_concurrency_six",
        "image_batch_edit",
        {
            "prompt": "批量并发 [scenario:concurrency_probe]",
            "image_paths": [f"<INPUT>/batch-{index}.png" for index in range(6)],
            "size": "1024x1024",
        },
        setup="batch",
    ),
    CaseSpec(
        "batch_quality_serial_gap",
        "image_batch_edit",
        {
            "prompt": "批量串行 [scenario:b64]",
            "image_paths": ["<INPUT>/batch-0.png", "<INPUT>/batch-1.png"],
            "size": "1024x1024",
            "model": "gpt-image-2-openai",
        },
        setup="batch",
    ),
    CaseSpec(
        "atomic_filename_collision",
        "image_generate",
        {"prompt": "collision [scenario:b64]", "size": "1024x1024", "basename": "same"},
        calls=2,
    ),
    CaseSpec(
        "generate_save_dir_symlink_escape",
        "image_generate",
        {
            "prompt": "x",
            "size": "1024x1024",
            "save_dir": "<SYMLINK_OUTPUT>",
        },
        setup="output_symlink",
    ),
)


def case_names() -> list[str]:
    return [case.name for case in CASES]


def _spec(name: str) -> CaseSpec:
    for case in CASES:
        if case.name == name:
            return case
    raise KeyError(name)


def _prepare_inputs(setup: str, input_root: Path, case_root: Path, output_root: Path) -> dict[str, str]:
    input_root.mkdir(parents=True, exist_ok=True)
    replacements = {
        "<INPUT>": str(input_root),
        "<CASE>": str(case_root),
        "<OUTPUT>": str(output_root),
    }
    if setup == "edit":
        (input_root / "source.png").write_bytes(png_bytes())
        (input_root / "mask.png").write_bytes(rgba_png_bytes())
    elif setup == "multi":
        # The second extension is intentionally false; MIME must follow magic bytes.
        (input_root / "ref-a.png").write_bytes(png_bytes(40, 30))
        (input_root / "ref-b.webp").write_bytes(png_bytes(48, 32))
    elif setup == "batch":
        for index in range(6):
            (input_root / f"batch-{index}.png").write_bytes(png_bytes(32 + index, 24 + index))
    elif setup == "over4":
        with (input_root / "over-4m.png").open("wb") as output:
            output.truncate(4 * 1024 * 1024 + 1)
    elif setup == "truncated":
        (input_root / "truncated.png").write_bytes(png_bytes()[:40])
    elif setup == "malformed_jpeg":
        (input_root / "malformed.jpg").write_bytes(b"\xff\xd8\xff\xe0" + (b"\x00" * 36))
    elif setup == "malformed_webp":
        (input_root / "malformed.webp").write_bytes(
            b"RIFF" + (32).to_bytes(4, "little") + b"WEBPVP8 " + (b"\x00" * 24)
        )
    elif setup == "bomb":
        raw = bytearray(png_bytes())
        raw[16:20] = (100_000).to_bytes(4, "big")
        raw[20:24] = (100_000).to_bytes(4, "big")
        (input_root / "bomb.png").write_bytes(raw)
    elif setup == "mask_wrong_size":
        (input_root / "source.png").write_bytes(png_bytes())
        (input_root / "wrong-mask.png").write_bytes(rgba_png_bytes(64, 48))
    elif setup == "mask_no_alpha":
        (input_root / "source.png").write_bytes(png_bytes())
        (input_root / "rgb-mask.png").write_bytes(png_bytes())
    elif setup == "input_symlink":
        outside = case_root / "outside-input.png"
        outside.write_bytes(png_bytes())
        link = input_root / "escape.png"
        try:
            link.symlink_to(outside)
            selected = link
        except OSError:
            selected = outside
        replacements["<SYMLINK_INPUT>"] = str(selected)
    elif setup == "multi_over8":
        base = png_bytes()
        target_size = 3 * 1024 * 1024
        padded = base + (b"\x00" * (target_size - len(base)))
        for index in range(3):
            (input_root / f"large-{index}.png").write_bytes(padded)
    elif setup == "output_symlink":
        outside = case_root / "outside-output"
        outside.mkdir()
        link = output_root / "escape"
        try:
            link.symlink_to(outside, target_is_directory=True)
            selected = link
        except OSError:
            selected = outside
        replacements["<SYMLINK_OUTPUT>"] = str(selected)
    return replacements


def _replace_placeholders(value: Any, replacements: dict[str, str]) -> Any:
    if isinstance(value, dict):
        return {key: _replace_placeholders(item, replacements) for key, item in value.items()}
    if isinstance(value, list):
        return [_replace_placeholders(item, replacements) for item in value]
    if isinstance(value, str):
        result = value
        for placeholder, replacement in replacements.items():
            result = result.replace(placeholder, replacement)
        return result
    return value


_TIMESTAMP_RE = re.compile(r"\b(gen|edit|batch|multiref)_(\d{12,})")


def _normalize_dynamic_names(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: _normalize_dynamic_names(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_normalize_dynamic_names(item) for item in value]
    if isinstance(value, str):
        return _TIMESTAMP_RE.sub(r"\1_<TIMESTAMP>", value)
    return value


def _saved_files(output_root: Path) -> list[dict[str, Any]]:
    files: list[dict[str, Any]] = []
    if not output_root.exists():
        return files
    for path in sorted(candidate for candidate in output_root.rglob("*") if candidate.is_file()):
        raw = path.read_bytes()
        files.append(
            {
                "path": _normalize_dynamic_names(str(path.relative_to(output_root))),
                "size": len(raw),
                "sha256": hashlib.sha256(raw).hexdigest(),
            }
        )
    return files


_UNORDERED_CASES = {
    "generate_standard_concurrency_six",
    "batch_standard_concurrency_six",
}


def _normalize_requests(name: str, requests: list[dict[str, Any]]) -> list[dict[str, Any]]:
    normalized = json.loads(json.dumps(requests, ensure_ascii=False))
    if name in _UNORDERED_CASES:
        for request in normalized:
            request.pop("attempt", None)
        normalized.sort(key=lambda item: json.dumps(item, ensure_ascii=False, sort_keys=True))
    return normalized


def run_case(command: list[str], name: str) -> dict[str, Any]:
    spec = _spec(name)
    with tempfile.TemporaryDirectory(prefix=f"micu-contract-{name}-") as temp_dir:
        case_root = Path(temp_dir).resolve()
        output_root = case_root / "out"
        input_root = case_root / "input"
        home_root = case_root / "home"
        output_root.mkdir(parents=True)
        home_root.mkdir(parents=True)
        placeholders = _prepare_inputs(spec.setup, input_root, case_root, output_root)
        arguments = _replace_placeholders(spec.arguments, placeholders)
        with MockMicuApi() as mock:
            overrides = mock.env()
            overrides.update(
                {
                    "HOME": str(home_root),
                    "USERPROFILE": str(home_root),
                    "MICU_INPUT_ROOT": str(input_root),
                }
            )
            overrides.update(dict(spec.env))
            env = isolated_server_env(output_root, overrides)
            with StdioSession(command, env=env, cwd=REPO_ROOT, timeout=30.0) as session:
                initialize = session.initialize()
                responses = []
                for index in range(spec.calls):
                    responses.append(
                        session.request(
                            10 + index,
                            "tools/call",
                            {"name": spec.tool, "arguments": arguments},
                        )
                    )
            stderr = bytes(session.stderr_bytes)
            requests = mock.state.snapshot()
            metrics = mock.state.metrics()
            replacements = [
                (str(case_root), "<CASE_ROOT>"),
                (mock.base_url, "<MOCK_API>"),
                (mock.proxy_url, "<MOCK_PROXY>"),
                (f"1.1.1.1:{mock.state.proxy_port}", "<PUBLIC_DOWNLOAD>"),
            ]

        gaps = metrics["request_start_gaps_seconds"].get("b64", [])
        normalized_metrics = {
            "max_active_api_requests": metrics["max_active_api_requests"],
            "serial_gap_at_least_1_4_seconds": bool(gaps) and min(gaps) >= 1.4,
        }
        result = {
            "initialize_protocol": initialize.get("result", {}).get("protocolVersion"),
            "responses": canonicalize(responses, replacements),
            "requests": canonicalize(_normalize_requests(name, requests), replacements),
            "saved_files": _saved_files(output_root),
            "metrics": normalized_metrics,
            "stdout_frames": len(session.stdout_lines),
            "stderr_contains_secret": b"contract-secret-key" in stderr,
            "stderr_contains_image_base64": base64_marker_in(stderr),
        }
        return _normalize_dynamic_names(result)


def base64_marker_in(raw: bytes) -> bool:
    marker = hashlib.sha256(png_bytes()).hexdigest().encode("ascii")
    # The SHA marker should not occur either; the long literal prefix catches raw b64 logging.
    encoded_prefix = json.dumps(png_bytes()[:24].hex()).encode("ascii")
    return marker in raw or encoded_prefix in raw or b"iVBORw0KGgo" in raw


__all__ = ["CASES", "CaseSpec", "case_names", "run_case"]
