"""Offline Micu Images API and forward-proxy fixture.

Scenarios are selected with ``[scenario:<name>]`` in the prompt.  The server
captures a redacted semantic representation of requests, so Python/Rust tests
can compare JSON and multipart behavior without comparing random boundaries.
"""
from __future__ import annotations

import base64
import email.parser
import email.policy
import functools
import hashlib
import json
import re
import socket
import struct
import threading
import time
import zlib
from dataclasses import dataclass, field
from email.message import Message
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit


MAX_RESPONSE_BYTES = 25 * 1024 * 1024
_SCENARIO_RE = re.compile(r"\[scenario:([a-z0-9_-]+)\]")


def _chunk(kind: bytes, payload: bytes) -> bytes:
    crc = zlib.crc32(payload, zlib.crc32(kind))
    return struct.pack(">I", len(payload)) + kind + payload + struct.pack(">I", crc)


def png_bytes(width: int = 32, height: int = 24) -> bytes:
    """Create a small, fully decodable RGB PNG without Pillow."""
    signature = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    scanline = b"\x00" + (b"\x20\x70\xc0" * width)
    pixels = scanline * height
    return signature + _chunk(b"IHDR", ihdr) + _chunk(b"IDAT", zlib.compress(pixels, 9)) + _chunk(b"IEND", b"")


def rgba_png_bytes(width: int = 32, height: int = 24) -> bytes:
    """Create a fully decodable RGBA PNG suitable for mask fixtures."""
    signature = b"\x89PNG\r\n\x1a\n"
    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    scanline = b"\x00" + (b"\x20\x70\xc0\x00" * width)
    pixels = scanline * height
    return signature + _chunk(b"IHDR", ihdr) + _chunk(b"IDAT", zlib.compress(pixels, 9)) + _chunk(b"IEND", b"")


def truncated_png_bytes() -> bytes:
    return png_bytes()[:40]


@functools.lru_cache(maxsize=4)
def large_png_bytes(target_size: int) -> bytes:
    base = png_bytes()
    payload_size = max(0, target_size - len(base) - 12)
    ancillary = _chunk(b"miCu", b"\x5a" * payload_size)
    return base[:-12] + ancillary + base[-12:]


def _json_bytes(value: Any) -> bytes:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")


def _scenario(prompt: str) -> str:
    match = _SCENARIO_RE.search(prompt)
    return match.group(1) if match else "b64"


def _parse_multipart(content_type: str, body: bytes) -> tuple[dict[str, str], list[dict[str, Any]]]:
    raw = (
        f"Content-Type: {content_type}\r\nMIME-Version: 1.0\r\n\r\n".encode("ascii")
        + body
    )
    message = email.parser.BytesParser(policy=email.policy.default).parsebytes(raw)
    fields: dict[str, str] = {}
    files: list[dict[str, Any]] = []
    if not message.is_multipart():
        return fields, files
    for part in message.iter_parts():
        name = part.get_param("name", header="content-disposition")
        filename = part.get_filename()
        payload = part.get_payload(decode=True) or b""
        if not isinstance(name, str):
            continue
        if filename is None:
            fields[name] = payload.decode("utf-8", errors="replace")
        else:
            files.append(
                {
                    "name": name,
                    "filename": filename,
                    "mime": part.get_content_type(),
                    "size": len(payload),
                    "sha256": hashlib.sha256(payload).hexdigest(),
                }
            )
    return fields, files


@dataclass
class CaptureState:
    expected_key: str = "contract-secret-key"
    proxy_port: int = 0
    requests: list[dict[str, Any]] = field(default_factory=list)
    scenario_attempts: dict[str, int] = field(default_factory=dict)
    scenario_start_times: dict[str, list[float]] = field(default_factory=dict)
    active_api_requests: int = 0
    max_active_api_requests: int = 0
    lock: threading.Lock = field(default_factory=threading.Lock)

    def add_request(self, request: dict[str, Any], *, api_request: bool = False) -> int:
        scenario = str(request.get("scenario", "unknown"))
        with self.lock:
            attempt = self.scenario_attempts.get(scenario, 0) + 1
            self.scenario_attempts[scenario] = attempt
            request["attempt"] = attempt
            self.requests.append(request)
            if api_request:
                self.active_api_requests += 1
                self.max_active_api_requests = max(
                    self.max_active_api_requests,
                    self.active_api_requests,
                )
                self.scenario_start_times.setdefault(scenario, []).append(time.monotonic())
            return attempt

    def finish_api_request(self) -> None:
        with self.lock:
            self.active_api_requests = max(0, self.active_api_requests - 1)

    def snapshot(self) -> list[dict[str, Any]]:
        with self.lock:
            return json.loads(json.dumps(self.requests, ensure_ascii=False))

    def metrics(self) -> dict[str, Any]:
        with self.lock:
            gaps: dict[str, list[float]] = {}
            for scenario, starts in self.scenario_start_times.items():
                gaps[scenario] = [round(starts[index] - starts[index - 1], 3) for index in range(1, len(starts))]
            return {
                "max_active_api_requests": self.max_active_api_requests,
                "request_start_gaps_seconds": gaps,
            }


class _FixtureServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address: tuple[str, int], handler, state: CaptureState):  # noqa: ANN001
        super().__init__(address, handler)
        self.state = state

    def handle_error(self, _request, _client_address) -> None:  # noqa: ANN001
        # Disconnect/stream-cap scenarios intentionally close sockets mid-response.
        return


class _ApiHandler(BaseHTTPRequestHandler):
    server: _FixtureServer
    protocol_version = "HTTP/1.1"

    def log_message(self, _format: str, *_args: Any) -> None:
        return

    def do_POST(self) -> None:  # noqa: N802
        length_text = self.headers.get("Content-Length", "0")
        try:
            length = int(length_text)
        except ValueError:
            length = 0
        body = self.rfile.read(max(length, 0))
        content_type = self.headers.get("Content-Type", "")
        json_body: dict[str, Any] | None = None
        fields: dict[str, str] = {}
        files: list[dict[str, Any]] = []
        if content_type.lower().startswith("application/json"):
            try:
                parsed = json.loads(body.decode("utf-8"))
                json_body = parsed if isinstance(parsed, dict) else None
            except (UnicodeDecodeError, json.JSONDecodeError):
                json_body = None
        elif content_type.lower().startswith("multipart/form-data"):
            fields, files = _parse_multipart(content_type, body)

        prompt = ""
        if json_body is not None and isinstance(json_body.get("prompt"), str):
            prompt = json_body["prompt"]
        elif isinstance(fields.get("prompt"), str):
            prompt = fields["prompt"]
        scenario = _scenario(prompt)
        authorization = self.headers.get("Authorization", "")
        capture = {
            "method": "POST",
            "path": urlsplit(self.path).path,
            "headers": {
                "accept": self.headers.get("Accept"),
                "authorization_scheme": authorization.split(" ", 1)[0] if authorization else None,
                "authorization_valid": authorization == f"Bearer {self.server.state.expected_key}",
                "content_type": content_type.split(";", 1)[0].lower(),
            },
            "json": json_body,
            "multipart_fields": fields,
            "multipart_files": files,
            "scenario": scenario,
        }
        attempt = self.server.state.add_request(capture, api_request=True)
        try:
            if not capture["headers"]["authorization_valid"]:
                self._respond_json(HTTPStatus.UNAUTHORIZED, {"error": {"message": "invalid key"}})
                return
            self._respond_scenario(scenario, attempt, json_body or fields)
        finally:
            self.server.state.finish_api_request()

    def _respond_scenario(self, scenario: str, attempt: int, request: dict[str, Any]) -> None:
        if scenario == "disconnect_once" and attempt == 1:
            try:
                self.connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            self.connection.close()
            return
        if scenario == "disconnect_body_once" and attempt == 1:
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", "1000")
            self.end_headers()
            try:
                self.wfile.write(b"{")
                self.wfile.flush()
                self.connection.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            self.connection.close()
            return
        if scenario == "api_timeout":
            time.sleep(0.25)
        if scenario == "concurrency_probe":
            time.sleep(0.2)
        retry_status = {
            "retry_400_too_many": 400,
            "retry_408": 408,
            "retry_429": 429,
            "retry_500": 500,
            "retry_after_seconds": 429,
            "retry_after_http_date": 429,
        }.get(scenario)
        if retry_status is not None and attempt == 1:
            headers: dict[str, str] = {}
            if scenario in {"retry_400_too_many", "retry_408", "retry_429", "retry_500", "retry_after_seconds"}:
                headers["Retry-After"] = "0"
            elif scenario == "retry_after_http_date":
                headers["Retry-After"] = "Thu, 01 Jan 1970 00:00:00 GMT"
            message = "Too Many Requests" if retry_status == 400 else f"fixture HTTP {retry_status}"
            self._respond_json(retry_status, {"error": {"message": message}}, headers=headers)
            return
        if scenario == "http_524":
            self._respond_json(524, {"error": {"message": "fixture timeout"}})
            return
        if scenario == "secret_error":
            self._respond_json(
                400,
                {
                    "error": {
                        "message": (
                            f"Authorization: Bearer {self.server.state.expected_key}; "
                            f"image=iVBORw0KGgo{'A' * 200}"
                        )
                    }
                },
            )
            return
        if scenario == "content_length_too_large":
            payload = b"{}"
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(MAX_RESPONSE_BYTES + 1))
            self.end_headers()
            self.wfile.write(payload)
            return
        if scenario == "stream_too_large":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Connection", "close")
            self.end_headers()
            block = b"x" * (64 * 1024)
            try:
                for _ in range((MAX_RESPONSE_BYTES // len(block)) + 2):
                    self.wfile.write(block)
                    self.wfile.flush()
            except (BrokenPipeError, ConnectionResetError, OSError):
                pass
            self.close_connection = True
            return
        if scenario == "near_json_cap":
            prefix = b'{"data":[],"padding":"'
            suffix = b'"}'
            padding_size = MAX_RESPONSE_BYTES - len(prefix) - len(suffix) - 4096
            self._respond_bytes(200, prefix + (b"A" * padding_size) + suffix, "application/json")
            return
        if scenario == "invalid_json":
            self._respond_bytes(200, b"{this is not json", "application/json")
            return
        if scenario == "no_image":
            self._respond_json(200, {"data": []})
            return

        response_format = str(request.get("response_format", "url"))
        requested_size = str(request.get("size", "1024x1024"))
        width, height = (32, 24)
        if scenario == "exact_b64":
            match = re.fullmatch(r"(\d+)x(\d+)", requested_size)
            if match:
                width, height = int(match.group(1)), int(match.group(2))
        image = png_bytes(width, height)
        if scenario == "large_b64":
            image = large_png_bytes(17 * 1024 * 1024)
        encoded = base64.b64encode(image).decode("ascii")
        if scenario == "data_url":
            self._respond_json(200, {"data": [{"url": f"data:image/png;base64,{encoded}"}]})
            return
        if scenario == "url_loopback":
            self._respond_json(200, {"data": [{"url": f"http://127.0.0.1:{self.server.server_address[1]}/private.png"}]})
            return
        if scenario == "url_mapped_loopback":
            self._respond_json(200, {"data": [{"url": "http://[::ffff:127.0.0.1]/private.png"}]})
            return
        if scenario in {"url_success", "url_no_content_length", "large_url"}:
            if scenario == "url_no_content_length":
                suffix = "asset-no-length.png"
            elif scenario == "large_url":
                suffix = "large.png"
            else:
                suffix = "asset.png"
            self._respond_json(200, {"data": [{"url": self._public_url(suffix)}]})
            return
        if scenario == "url_truncated_then_b64" and response_format == "url":
            self._respond_json(200, {"data": [{"url": self._public_url("truncated.png")}]})
            return
        if scenario == "url_redirect_private_then_b64" and response_format == "url":
            self._respond_json(200, {"data": [{"url": self._public_url("redirect-private.png")}]})
            return
        self._respond_json(200, {"data": [{"b64_json": encoded}]})

    def _public_url(self, suffix: str) -> str:
        return f"http://1.1.1.1:{self.server.state.proxy_port}/{suffix}"

    def _respond_json(self, status: int, value: Any, headers: dict[str, str] | None = None) -> None:
        self._respond_bytes(status, _json_bytes(value), "application/json", headers=headers)

    def _respond_bytes(
        self,
        status: int,
        payload: bytes,
        content_type: str,
        headers: dict[str, str] | None = None,
    ) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(payload)))
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        try:
            self.wfile.write(payload)
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass


class _ProxyHandler(BaseHTTPRequestHandler):
    server: _FixtureServer
    protocol_version = "HTTP/1.1"

    def log_message(self, _format: str, *_args: Any) -> None:
        return

    def do_GET(self) -> None:  # noqa: N802
        parsed = urlsplit(self.path)
        path = parsed.path or self.path
        self.server.state.add_request(
            {
                "method": "GET",
                "path": path,
                "headers": {
                    "accept": self.headers.get("Accept"),
                    "authorization_scheme": None,
                    "authorization_valid": None,
                    "content_type": None,
                },
                "json": None,
                "multipart_fields": {},
                "multipart_files": [],
                "scenario": f"download:{path}",
            }
        )
        if path == "/redirect-private.png":
            self.send_response(302)
            self.send_header("Location", "http://127.0.0.1/private.png")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        if path == "/truncated.png":
            payload = truncated_png_bytes()
        elif path == "/large.png":
            payload = large_png_bytes(24 * 1024 * 1024)
        else:
            payload = png_bytes()
        self.send_response(200)
        self.send_header("Content-Type", "image/png")
        if path != "/asset-no-length.png":
            self.send_header("Content-Length", str(len(payload)))
        else:
            self.send_header("Connection", "close")
            self.close_connection = True
        self.end_headers()
        self.wfile.write(payload)


@dataclass
class MockMicuApi:
    expected_key: str = "contract-secret-key"
    state: CaptureState = field(init=False)
    api_server: _FixtureServer | None = field(init=False, default=None)
    proxy_server: _FixtureServer | None = field(init=False, default=None)
    _threads: list[threading.Thread] = field(init=False, default_factory=list)

    def __enter__(self) -> "MockMicuApi":
        self.state = CaptureState(expected_key=self.expected_key)
        self.proxy_server = _FixtureServer(("127.0.0.1", 0), _ProxyHandler, self.state)
        self.state.proxy_port = int(self.proxy_server.server_address[1])
        self.api_server = _FixtureServer(("127.0.0.1", 0), _ApiHandler, self.state)
        for server in (self.proxy_server, self.api_server):
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            self._threads.append(thread)
        return self

    def __exit__(self, exc_type, exc, tb) -> None:  # noqa: ANN001
        for server in (self.api_server, self.proxy_server):
            if server is not None:
                server.shutdown()
                server.server_close()
        for thread in self._threads:
            thread.join(timeout=2)

    @property
    def base_url(self) -> str:
        if self.api_server is None:
            raise RuntimeError("mock server is not running")
        return f"http://127.0.0.1:{self.api_server.server_address[1]}"

    @property
    def proxy_url(self) -> str:
        if self.proxy_server is None:
            raise RuntimeError("mock proxy is not running")
        return f"http://127.0.0.1:{self.proxy_server.server_address[1]}"

    def env(self) -> dict[str, str]:
        return {
            "MICU_BASEURL": self.base_url,
            "MICU_API_KEY": self.expected_key,
            "MICU_USE_SHELL_PROXY": "1",
            "HTTP_PROXY": self.proxy_url,
            "HTTPS_PROXY": self.proxy_url,
            "ALL_PROXY": self.proxy_url,
            "NO_PROXY": "127.0.0.1,localhost",
            "http_proxy": self.proxy_url,
            "https_proxy": self.proxy_url,
            "all_proxy": self.proxy_url,
            "no_proxy": "127.0.0.1,localhost",
        }


__all__ = [
    "MAX_RESPONSE_BYTES",
    "MockMicuApi",
    "png_bytes",
    "large_png_bytes",
    "rgba_png_bytes",
    "truncated_png_bytes",
]
