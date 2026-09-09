"""Real PEP 668 installer smoke; run with a distro-managed Python on Linux.

Dependencies come from the configured pip index. Only /v1/models is a loopback
fixture; the venv, pip, installed dependencies and MCP stdio server are real.
No production key or paid image API is used. All client config is temporary.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import sysconfig
import tempfile
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread

KEY = "sk-ci-pep668-fixture"


class ModelsHandler(BaseHTTPRequestHandler):
    calls = 0

    def do_GET(self) -> None:
        if self.path != "/v1/models":
            self.send_error(404)
            return
        if self.headers.get("Authorization") != f"Bearer {KEY}":
            self.send_error(401)
            return
        type(self).calls += 1
        body = json.dumps({"data": [{"id": "gpt-image-2"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt: str, *args: object) -> None:
        pass


def run(command: list[str], *, cwd: Path, env: dict[str, str]) -> str:
    result = subprocess.run(command, cwd=cwd, env=env, capture_output=True,
                            text=True, encoding="utf-8", timeout=300, check=False)
    print(result.stdout, end="", flush=True)
    if result.returncode:
        print(result.stderr, file=sys.stderr, flush=True)
        raise AssertionError(f"Command failed ({result.returncode}): {command}")
    return result.stdout


def main() -> None:
    marker = Path(sysconfig.get_path("stdlib")) / "EXTERNALLY-MANAGED"
    assert sys.prefix == sys.base_prefix, "Run with system Python, not a venv"
    assert marker.is_file(), "This smoke requires a real PEP 668 marker"
    marker_before = marker.read_bytes()
    source = Path(__file__).resolve().parents[1]
    clean_env = {key: value for key, value in os.environ.items()
                 if not key.startswith(("MICU_", "PIP_", "PYTHON"))
                 and key not in {"VIRTUAL_ENV", "CONDA_PREFIX"}}
    clean_env.update(PYTHONUTF8="1", PIP_DISABLE_PIP_VERSION_CHECK="1")
    # A nonexistent package and --no-index ensure this cannot install anything,
    # even if an unexpected pip configuration disables the PEP 668 protection.
    blocked = subprocess.run(
        [sys.executable, "-m", "pip", "install", "--no-index", "--no-deps",
         "micu-pep668-ci-nonexistent"], env=clean_env,
        capture_output=True, text=True, timeout=30, check=False,
    )
    assert blocked.returncode != 0
    assert "externally-managed-environment" in blocked.stdout + blocked.stderr
    print("Confirmed: system pip refuses installation under PEP 668", flush=True)
    probe = "import importlib.util;print(importlib.util.find_spec('mcp'))"
    before = subprocess.check_output([sys.executable, "-I", "-c", probe], env=clean_env)

    with tempfile.TemporaryDirectory(prefix="micu pep668 ") as temporary:
        root = Path(temporary)
        repo = root / "repo with spaces"
        shutil.copytree(source, repo, ignore=shutil.ignore_patterns(
            ".git", ".venv", "__pycache__", ".pytest_cache", "target", "output",
            "*.egg-info", "build", "dist",
        ))
        home = root / "home"
        (home / ".codex").mkdir(parents=True)
        claude = home / ".claude.json"
        codex = home / ".codex" / "config.toml"
        claude.write_text(json.dumps({"theme": "dark", "mcpServers": {
            "other": {"command": "keep"}}}), encoding="utf-8")
        codex.write_text('[mcp_servers.other]\ncommand = "keep"\n', encoding="utf-8")
        env = {**clean_env, "HOME": str(home), "USERPROFILE": str(home),
               "MICU_API_KEY": KEY, "MICU_SAVE_DIR": str(root / "images"),
               "VIRTUAL_ENV": str(root / "stale-IDE-environment"),
               "CONDA_PREFIX": str(root / "stale-conda-environment")}
        httpd = ThreadingHTTPServer(("127.0.0.1", 0), ModelsHandler)
        thread = Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        try:
            baseurl = f"http://127.0.0.1:{httpd.server_port}"
            command = [sys.executable, str(repo / "install.py"), "--yes",
                       "--baseurl", baseurl]
            python = repo / ".venv" / "bin" / "python"
            original_cfg = None
            for _ in range(2):
                output = run(command, cwd=repo, env=env)
                assert "tools/list OK" in output, "Real MCP stdio smoke did not pass"
                assert "httpx 不可用" not in output, "Model validation was skipped"
                cfg = (repo / ".venv" / "pyvenv.cfg").read_bytes()
                if original_cfg is not None:
                    assert cfg == original_cfg, "Existing venv was unexpectedly recreated"
                original_cfg = cfg
                parsed = json.loads(claude.read_text(encoding="utf-8"))
                assert parsed["theme"] == "dark"
                assert parsed["mcpServers"]["other"]["command"] == "keep"
                assert parsed["mcpServers"]["micu-image"]["command"] == str(python)
                toml = codex.read_text(encoding="utf-8")
                assert 'command = "keep"' in toml
                assert f"command = {json.dumps(str(python))}" in toml
                assert toml.count("[mcp_servers.micu-image]") == 1
            assert ModelsHandler.calls >= 2, "Both real httpx model probes must run"
            metadata = json.loads(run([str(python), "-I", "-c",
                "import json,sys,mcp,httpx,pip,PIL;print(json.dumps({"
                "'prefix':sys.prefix,'base':sys.base_prefix,'files':"
                "[mcp.__file__,httpx.__file__,pip.__file__,PIL.__file__]}))"],
                cwd=repo, env=env))
            venv = (repo / ".venv").resolve()
            assert Path(metadata["prefix"]).resolve() == venv
            assert metadata["prefix"] != metadata["base"]
            assert all(Path(path).resolve().is_relative_to(venv)
                       for path in metadata["files"])
            run([sys.executable, str(repo / "install.py"), "--reset"], cwd=repo, env=env)
            assert json.loads(claude.read_text(encoding="utf-8"))["mcpServers"] == {
                "other": {"command": "keep"}}
            assert codex.read_text(encoding="utf-8").strip() == (
                '[mcp_servers.other]\ncommand = "keep"')
            assert python.is_file(), "Reset must preserve the installed environment"
        finally:
            httpd.shutdown()
            httpd.server_close()
            thread.join(timeout=5)
    assert marker.read_bytes() == marker_before
    after = subprocess.check_output([sys.executable, "-I", "-c", probe], env=clean_env)
    assert before == after, "System Python package visibility changed"
    print("PEP 668 smoke passed: real install/reuse/httpx/MCP/config/reset; system unchanged")


if __name__ == "__main__":
    main()
