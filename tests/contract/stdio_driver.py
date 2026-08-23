"""Small synchronous MCP STDIO driver used by contract tests and benchmarks.

The driver deliberately treats every stdout line as JSON-RPC.  A server log or
banner written to stdout therefore fails the contract instead of being silently
ignored.
"""
from __future__ import annotations

import json
import os
import queue
import subprocess
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable


class ProtocolError(RuntimeError):
    """The child violated the line-delimited MCP STDIO contract."""


@dataclass
class StdioSession:
    command: list[str]
    env: dict[str, str]
    cwd: Path
    timeout: float = 10.0
    process: subprocess.Popen[bytes] | None = field(init=False, default=None)
    stdout_lines: list[bytes] = field(init=False, default_factory=list)
    stderr_bytes: bytearray = field(init=False, default_factory=bytearray)
    _stdout_queue: queue.Queue[bytes | None] = field(init=False, default_factory=queue.Queue)
    _stderr_thread: threading.Thread | None = field(init=False, default=None)
    _stdout_thread: threading.Thread | None = field(init=False, default=None)

    def __enter__(self) -> "StdioSession":
        self.process = subprocess.Popen(
            self.command,
            cwd=self.cwd,
            env=self.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if self.process.stdout is None or self.process.stderr is None:
            raise ProtocolError("failed to open child stdout/stderr")

        def read_stdout() -> None:
            assert self.process is not None and self.process.stdout is not None
            for line in iter(self.process.stdout.readline, b""):
                self._stdout_queue.put(line)
            self._stdout_queue.put(None)

        def read_stderr() -> None:
            assert self.process is not None and self.process.stderr is not None
            while True:
                chunk = self.process.stderr.read(8192)
                if not chunk:
                    break
                self.stderr_bytes.extend(chunk)

        self._stdout_thread = threading.Thread(target=read_stdout, daemon=True)
        self._stderr_thread = threading.Thread(target=read_stderr, daemon=True)
        self._stdout_thread.start()
        self._stderr_thread.start()
        return self

    def __exit__(self, exc_type, exc, tb) -> None:  # noqa: ANN001
        if self.process is None:
            return
        if self.process.stdin is not None:
            try:
                self.process.stdin.close()
            except OSError:
                pass
        try:
            self.process.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.process.terminate()
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=2)
        if self._stdout_thread is not None:
            self._stdout_thread.join(timeout=1)
        if self._stderr_thread is not None:
            self._stderr_thread.join(timeout=1)

    def send(self, message: dict[str, Any]) -> None:
        if self.process is None or self.process.stdin is None:
            raise ProtocolError("child stdin is unavailable")
        payload = json.dumps(message, ensure_ascii=False, separators=(",", ":")) + "\n"
        try:
            self.process.stdin.write(payload.encode("utf-8"))
            self.process.stdin.flush()
        except (BrokenPipeError, OSError) as error:
            raise ProtocolError(self._child_failure("server stdin closed", error)) from error

    def request(self, request_id: int, method: str, params: dict[str, Any]) -> dict[str, Any]:
        self.send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.monotonic() + self.timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise ProtocolError(self._child_failure(f"timeout waiting for id={request_id}"))
            try:
                line = self._stdout_queue.get(timeout=remaining)
            except queue.Empty as error:
                raise ProtocolError(self._child_failure(f"timeout waiting for id={request_id}")) from error
            if line is None:
                raise ProtocolError(self._child_failure(f"server exited before id={request_id}"))
            self.stdout_lines.append(line)
            try:
                message = json.loads(line.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError) as error:
                preview = line[:240].decode("utf-8", errors="replace")
                raise ProtocolError(f"stdout pollution (not JSON-RPC): {preview!r}") from error
            if not isinstance(message, dict):
                raise ProtocolError(f"stdout JSON-RPC frame is not an object: {message!r}")
            if message.get("id") == request_id:
                return message
            raise ProtocolError(
                f"unexpected stdout JSON-RPC frame while waiting for id={request_id}: {message!r}"
            )

    def notify(self, method: str, params: dict[str, Any] | None = None) -> None:
        message: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        self.send(message)

    def initialize(self, protocol_version: str = "2024-11-05") -> dict[str, Any]:
        response = self.request(
            1,
            "initialize",
            {
                "protocolVersion": protocol_version,
                "capabilities": {},
                "clientInfo": {"name": "micu-contract", "version": "1"},
            },
        )
        self.notify("notifications/initialized")
        return response

    def _child_failure(self, message: str, error: Exception | None = None) -> str:
        suffix = f": {error}" if error is not None else ""
        stderr_tail = bytes(self.stderr_bytes[-1000:]).decode("utf-8", errors="replace")
        if stderr_tail:
            return f"{message}{suffix}; stderr tail: {stderr_tail}"
        return f"{message}{suffix}"


def isolated_server_env(save_root: Path, overrides: dict[str, str] | None = None) -> dict[str, str]:
    """Return a deterministic no-live-key environment for a contract child."""
    env = os.environ.copy()
    for name in (
        "MICU_API_KEY",
        "MICU_GROK_API_KEY",
        "XAI_API_KEY",
        "GROK_API_KEY",
        "MICU_INPUT_ROOT",
        "MICU_BASEURL",
        "MICU_MODEL",
        "MICU_RESPONSE_FORMAT",
        "MICU_TRUSTED_DOWNLOAD_HOSTS",
        "MICU_ALLOW_FAKE_IP_DOWNLOAD",
        "MICU_CONTRACT_TESTING",
        "MICU_TEST_API_TIMEOUT_MS",
        "RUST_LOG",
    ):
        env.pop(name, None)
    env.update(
        {
            "HOME": str(save_root),
            "USERPROFILE": str(save_root),
            "MICU_SAVE_DIR": str(save_root),
            "MICU_SAVE_DIR_ROOT": str(save_root),
            "MICU_USE_SHELL_PROXY": "0",
            "MICU_RUN_LIVE_TESTS": "0",
            "PYTHONUNBUFFERED": "1",
        }
    )
    if overrides:
        env.update(overrides)
    return env


def canonicalize(value: Any, replacements: Iterable[tuple[str, str]]) -> Any:
    """Replace deterministic runtime paths while preserving all schema content."""
    if isinstance(value, dict):
        return {key: canonicalize(item, replacements) for key, item in value.items()}
    if isinstance(value, list):
        return [canonicalize(item, replacements) for item in value]
    if isinstance(value, str):
        result = value
        for source, replacement in replacements:
            result = result.replace(source, replacement)
        return result
    return value


def text_content_json(response: dict[str, Any]) -> Any:
    """Decode FastMCP/RMCP JSON text content without weakening wire checks."""
    result = response.get("result")
    if not isinstance(result, dict):
        return response
    content = result.get("content")
    if not isinstance(content, list) or not content:
        return response
    first = content[0]
    if not isinstance(first, dict) or first.get("type") != "text":
        return response
    text = first.get("text")
    if not isinstance(text, str):
        return response
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return text
