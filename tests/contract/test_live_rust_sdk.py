from __future__ import annotations

import json
import os
import sys
import tempfile
from pathlib import Path

import pytest

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.stdio_driver import StdioSession, text_content_json


RUST_BINARY = REPO_ROOT / "target" / "release" / (
    "micu-image-mcp.exe" if sys.platform == "win32" else "micu-image-mcp"
)


@pytest.mark.skipif(
    os.environ.get("MICU_RUN_LIVE_TESTS") != "1",
    reason="paid live SDK test requires MICU_RUN_LIVE_TESTS=1",
)
def test_one_real_rust_sdk_generation_without_persisting_or_logging_key() -> None:
    assert RUST_BINARY.is_file(), f"release binary missing: {RUST_BINARY}"
    with tempfile.TemporaryDirectory(prefix="micu-rust-live-sdk-") as temp_dir:
        save_root = Path(temp_dir).resolve()
        env = os.environ.copy()
        env.update(
            {
                "MICU_BASEURL": "https://www.micuapi.ai",
                "MICU_SAVE_DIR": str(save_root),
                "MICU_SAVE_DIR_ROOT": str(save_root),
                "MICU_MODEL": "gpt-image-2",
                "MICU_RUN_LIVE_TESTS": "1",
            }
        )
        with StdioSession([str(RUST_BINARY)], env, REPO_ROOT, timeout=700) as session:
            session.initialize("2024-11-05")
            info_response = session.request(
                2,
                "tools/call",
                {"name": "server_info", "arguments": {}},
            )
            info = text_content_json(info_response)
            assert info["api_key_configured"] is True
            response = session.request(
                3,
                "tools/call",
                {
                    "name": "image_generate",
                    "arguments": {
                        "prompt": "A simple blue circle centered on a clean white background",
                        "size": "1024x1024",
                        "n": 1,
                        "model": "gpt-image-2",
                        "basename": "live_sdk_path_refactor",
                    },
                },
            )
        result = text_content_json(response)
        assert result["ok"] is True, result
        assert result["model"] == "gpt-image-2"
        assert result["requested_n"] == 1
        assert len(result["saved"]) == 1
        saved = result["saved"][0]
        saved_path = Path(saved["path"])
        assert saved_path.is_file()
        assert saved_path.is_relative_to(save_root)
        assert saved["size_bytes"] > 0
        assert "x" in saved["actual_size"]

        report_path = os.environ.get("MICU_LIVE_REPORT")
        if report_path:
            sanitized = {
                "ok": result["ok"],
                "model": result["model"],
                "size": result["size"],
                "requested_n": result["requested_n"],
                "saved": {
                    "size_bytes": saved["size_bytes"],
                    "actual_size": saved["actual_size"],
                    "actual_megapixels": saved["actual_megapixels"],
                },
                "notes": result.get("notes", []),
            }
            Path(report_path).write_text(
                json.dumps(sanitized, ensure_ascii=False, indent=2) + "\n",
                encoding="utf-8",
            )
