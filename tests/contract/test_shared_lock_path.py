from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.stdio_driver import StdioSession, isolated_server_env, text_content_json


RUST_BINARY = REPO_ROOT / "target" / "debug" / (
    "micu-image-mcp.exe" if sys.platform == "win32" else "micu-image-mcp"
)


@pytest.mark.skipif(not RUST_BINARY.is_file(), reason="cargo build is required")
def test_python_and_rust_share_the_same_default_large_size_tier_lock(tmp_path: Path) -> None:
    home = tmp_path.resolve()
    env = isolated_server_env(home)
    python = subprocess.run(
        [
            sys.executable,
            "-c",
            (
                "import json; "
                "from micu_image_mcp.locks import _BIG_SIZE_FILE_LOCK_PATH; "
                "print(json.dumps(str(_BIG_SIZE_FILE_LOCK_PATH)))"
            ),
        ],
        cwd=REPO_ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )
    assert python.returncode == 0, python.stderr
    python_lock = Path(json.loads(python.stdout)).resolve()

    with StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as session:
        session.initialize()
        response = session.request(
            2,
            "tools/call",
            {"name": "server_info", "arguments": {}},
        )
    info = text_content_json(response)
    rust_description = info["retry_policy"]["concurrency_2k_4k"]
    expected = (home / ".cache" / "micu-image" / "bigsize.lock").resolve()

    assert python_lock == expected
    assert str(expected) in rust_description
    assert "Python/Rust" in rust_description
