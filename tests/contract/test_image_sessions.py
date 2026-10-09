"""Image conversation memory through the public Rust MCP and a local API fixture."""
from __future__ import annotations

import hashlib
import json
import sys
from pathlib import Path

import pytest

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.mock_micu_api import MockMicuApi
from tests.contract.stdio_driver import StdioSession, isolated_server_env, text_content_json

RUST_BINARY = REPO_ROOT / "target" / "debug" / (
    "micu-image-mcp.exe" if sys.platform == "win32" else "micu-image-mcp"
)
pytestmark = pytest.mark.skipif(not RUST_BINARY.is_file(), reason="cargo build is required")


def call(server: StdioSession, tool: str, arguments: dict) -> dict:
    response = server.request(10, "tools/call", {"name": tool, "arguments": arguments})
    assert "error" not in response, response
    return text_content_json(response)


def requests(api: MockMicuApi) -> list[dict]:
    return [request for request in api.state.snapshot() if request["method"] == "POST"]


def prompt(request: dict) -> str:
    return (request["json"] or request["multipart_fields"])["prompt"]


def assert_input(request: dict, output: str) -> None:
    files = request["multipart_files"]
    assert len(files) == 1
    assert files[0]["name"] == "image"
    assert files[0]["sha256"] == hashlib.sha256(Path(output).read_bytes()).hexdigest()


def test_generation_memory_survives_restart_and_is_shared_with_edits_and_branches(tmp_path: Path) -> None:
    with MockMicuApi() as api:
        env = isolated_server_env(tmp_path, api.env())
        with StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as server:
            server.initialize()
            first = call(server, "image_generate", {
                "prompt": f"ROOT_INTENT: portrait with red clothing, {api.expected_key}",
                "session_id": "new", "size": "1536x1024", "quality": "low",
                "context": {"identity": "same face", "clothing": "red", "secret": api.expected_key},
            })
            assert first["ok"] is True, first
            assert first["session"]["revision"] == 1
            assert first["session"]["parent_revision"] is None
            session_id = first["session"]["id"]
            private = call(server, "image_generate", {
                "prompt": "PRIVATE_OTHER_SESSION", "session_id": "new",
                "context": {"identity": "another face"},
            })
            assert private["ok"] is True
            assert private["session"]["id"] != session_id
        captured = requests(api)
        assert captured[0]["path"] == "/v1/images/generations"
        assert "EFFECTIVE CONSTRAINTS" in prompt(captured[0])
        assert '"clothing":"red"' in prompt(captured[0])
        assert api.expected_key not in prompt(captured[0])

        # Start a fresh MCP process: the remembered image, description, settings and constraints
        # must come from disk rather than Python test state or a process-local conversation.
        with StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as server:
            server.initialize()
            second = call(server, "image_edit", {
                "prompt": "BEACH_INTENT: change background to seaside", "session_id": session_id,
                "context": {"background": "seaside"},
            })
            assert second["ok"] is True, second
            assert second["session"]["revision"] == 2
            assert second["session"]["parent_revision"] == 1
            assert second["session"]["history_length"] == 2
            assert second["session"]["context"]["clothing"] == "red"
            assert second["model"] == first["model"]
            assert second["size"] == "1536x1024"
        request = requests(api)[-1]
        assert request["path"] == "/v1/images/edits"
        assert request["multipart_fields"]["quality"] == "low"
        assert "ROOT_INTENT" in prompt(request)
        assert "PRIVATE_OTHER_SESSION" not in prompt(request)
        assert_input(request, first["saved"][0]["path"])

        with StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as server:
            server.initialize()
            third = call(server, "image_generate", {
                "prompt": "BLUE_INTENT: change clothing to blue", "session_id": session_id,
                "context": {"clothing": "blue", "secret": None},
            })
            assert third["ok"] is True, third
            assert isinstance(third["saved"], list)
            assert third["saved"][0]["index"] == 1
            assert third["requested_n"] == 1
            assert third["errors"] == []
            assert third["size_honored"] is False  # fixture deliberately returns 32x24
            assert third["session"]["revision"] == 3
            assert third["session"]["parent_revision"] == 2
            assert third["session"]["context"]["clothing"] == "blue"
            assert third["session"]["context"]["background"] == "seaside"
            assert "secret" not in third["session"]["context"]
            third_request = requests(api)[-1]
            assert_input(third_request, second["saved"]["path"])
            assert "BEACH_INTENT" in prompt(third_request)
            assert '"clothing":"blue"' in prompt(third_request)

            branch = call(server, "image_generate", {
                "prompt": "BRANCH_INTENT: add a hat", "session_id": session_id, "parent_revision": 1,
            })
            assert branch["ok"] is True, branch
            assert branch["session"]["revision"] == 4
            assert branch["session"]["parent_revision"] == 1
            assert branch["session"]["history_length"] == 2
            assert branch["session"]["context"]["clothing"] == "red"
            assert "background" not in branch["session"]["context"]
            branch_request = requests(api)[-1]
            assert_input(branch_request, first["saved"][0]["path"])
            assert "ROOT_INTENT" in prompt(branch_request)
            assert "BEACH_INTENT" not in prompt(branch_request)
            assert "BLUE_INTENT" not in prompt(branch_request)
            assert "PRIVATE_OTHER_SESSION" not in prompt(branch_request)

        for path in (tmp_path / ".micu-sessions").glob("*/revision-*.json"):
            assert api.expected_key not in path.read_text()
            assert "api_key" not in path.read_text()
        assert api.expected_key.encode() not in b"".join(server.stdout_lines)
        assert api.expected_key.encode() not in server.stderr_bytes


def test_failed_generation_or_edit_does_not_become_conversation_memory(tmp_path: Path) -> None:
    with MockMicuApi() as api:
        env = isolated_server_env(tmp_path, api.env())
        with StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as server:
            server.initialize()
            failed_first = call(server, "image_generate", {
                "prompt": "[scenario:secret_error] FAILED_INITIAL", "session_id": "new",
                "context": {"clothing": "green"},
            })
            assert failed_first["ok"] is False
            assert not list((tmp_path / ".micu-sessions").glob("*/revision-*.json"))
            first = call(server, "image_generate", {
                "prompt": "INITIAL_SUCCESS", "session_id": "new", "context": {"clothing": "red"},
            })
            assert first["ok"] is True
            session_id = first["session"]["id"]
            for scenario in ("secret_error", "no_image"):
                failed = call(server, "image_generate", {
                    "prompt": f"[scenario:{scenario}] FAILED_CHANGE", "session_id": session_id,
                    "context": {"clothing": "green"},
                })
                assert failed["ok"] is False, failed
                assert failed["session"]["revision"] == 1
                assert failed["session"]["context"]["clothing"] == "red"
                assert not (tmp_path / ".micu-sessions" / session_id / "revision-2.json").exists()
                assert api.expected_key not in json.dumps(failed)
            success = call(server, "image_generate", {
                "prompt": "SUCCESS_AFTER_FAILURE", "session_id": session_id,
            })
            assert success["ok"] is True, success
            assert success["session"]["revision"] == 2
            assert success["session"]["context"]["clothing"] == "red"
            assert "FAILED_CHANGE" not in prompt(requests(api)[-1])
            assert "green" not in prompt(requests(api)[-1])


@pytest.mark.parametrize("arguments", [
    {"session_id": "new", "n": 2},
    {"context": {"identity": "same"}},
    {"parent_revision": 1},
    {"session_id": "new", "parent_revision": 1},
    {"session_id": "../another-session"},
    {"session_id": "edit_" + "a" * 32},
    {"session_id": "new", "context": {"identity": ""}},
    {"session_id": "new", "context": {"identity": "x" * 2049}},
    {"session_id": "new", "prompt": "x" * 32001},
    {"session_id": "new", "prompt": "描述" * 15750, "context": {"identity": "人物" * 1000}},
])
def test_invalid_session_requests_do_not_reach_provider(tmp_path: Path, arguments: dict) -> None:
    with MockMicuApi() as api:
        env = isolated_server_env(tmp_path, api.env())
        with StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as server:
            server.initialize()
            result = call(server, "image_generate", {"prompt": "portrait", **arguments})
            assert result["ok"] is False, result
            assert requests(api) == []
            assert not list((tmp_path / ".micu-sessions").glob("*/revision-*.json"))


def test_conversation_memory_cannot_bypass_input_path_permissions(tmp_path: Path) -> None:
    input_root = tmp_path / "allowed-input"
    input_root.mkdir()
    with MockMicuApi() as api:
        env = isolated_server_env(tmp_path, {**api.env(), "MICU_INPUT_ROOT": str(input_root)})
        with StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as server:
            server.initialize()
            first = call(server, "image_generate", {"prompt": "portrait", "session_id": "new"})
            assert first["ok"] is True
            denied = call(server, "image_generate", {
                "prompt": "change background", "session_id": first["session"]["id"],
            })
            assert denied["ok"] is False
            assert "MICU_INPUT_ROOT" in denied["error"]
            assert len(requests(api)) == 1
            assert not (tmp_path / ".micu-sessions" / first["session"]["id"] / "revision-2.json").exists()


def test_session_memory_works_with_stateless_mcp_requests(tmp_path: Path) -> None:
    from tests.contract.test_latest_protocol import latest_meta

    with MockMicuApi() as api:
        env = isolated_server_env(tmp_path, api.env())
        with StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as server:
            discovered = server.request(1, "server/discover", {"_meta": latest_meta()})
            assert "2026-07-28" in discovered["result"]["supportedVersions"]
            first = text_content_json(server.request(2, "tools/call", {
                "_meta": latest_meta(), "name": "image_generate",
                "arguments": {"prompt": "STATELESS_ORIGINAL", "session_id": "new"},
            }))
            assert first["ok"] is True
        with StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as server:
            second = text_content_json(server.request(3, "tools/call", {
                "_meta": latest_meta(), "name": "image_generate",
                "arguments": {"prompt": "STATELESS_NEXT", "session_id": first["session"]["id"]},
            }))
            assert second["ok"] is True, second
            assert second["session"]["revision"] == 2
            assert "STATELESS_ORIGINAL" in prompt(requests(api)[-1])
            assert_input(requests(api)[-1], first["saved"][0]["path"])


def test_missing_remembered_image_fails_without_advancing_or_calling_provider(tmp_path: Path) -> None:
    with MockMicuApi() as api:
        env = isolated_server_env(tmp_path, api.env())
        with StdioSession([str(RUST_BINARY)], env, REPO_ROOT) as server:
            server.initialize()
            first = call(server, "image_generate", {"prompt": "portrait", "session_id": "new"})
            assert first["ok"] is True
            Path(first["saved"][0]["path"]).unlink()
            result = call(server, "image_generate", {
                "prompt": "continue", "session_id": first["session"]["id"],
            })
            assert result["ok"] is False
            assert len(requests(api)) == 1
            assert not (tmp_path / ".micu-sessions" / first["session"]["id"] / "revision-2.json").exists()
