from __future__ import annotations

import sys
import tempfile
from pathlib import Path

from tests.contract.mock_micu_api import MockMicuApi
from tests.contract.stdio_driver import StdioSession, isolated_server_env, text_content_json


REPO_ROOT = Path(__file__).resolve().parents[2]


def test_python_reference_uses_mock_generation_json_without_secret_capture() -> None:
    with tempfile.TemporaryDirectory(prefix="micu-mock-contract-") as temp_dir:
        save_root = Path(temp_dir).resolve()
        with MockMicuApi() as mock:
            env = isolated_server_env(save_root, mock.env())
            with StdioSession(
                [sys.executable, str(REPO_ROOT / "server.py")],
                env=env,
                cwd=REPO_ROOT,
            ) as session:
                session.initialize()
                response = session.request(
                    2,
                    "tools/call",
                    {
                        "name": "image_generate",
                        "arguments": {
                            "prompt": "contract [scenario:exact_b64] 中文",
                            "size": "1024x1024",
                            "basename": "mock_generation",
                        },
                    },
                )
            result = text_content_json(response)
            assert result["ok"] is True, result
            assert result["size_honored"] is True
            assert Path(result["saved"][0]["path"]).is_file()
            captured = mock.state.snapshot()

    posts = [item for item in captured if item["method"] == "POST"]
    assert len(posts) == 1
    assert posts[0]["path"] == "/v1/images/generations"
    assert posts[0]["json"]["prompt"] == "contract [scenario:exact_b64] 中文"
    assert posts[0]["headers"]["authorization_valid"] is True
    assert "contract-secret-key" not in repr(captured)
