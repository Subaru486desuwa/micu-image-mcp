"""Real native Claude Code/OpenCode reasoning with an offline Rust MCP image backend."""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import threading
import time
import uuid

import httpx

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.mock_micu_api import MockMicuApi
from tests.contract.stdio_driver import isolated_server_env

TOOLS_ROOT = Path.home() / ".local/share/micu-harness-tools/node_modules/.bin"
PRINT_LOCK = threading.Lock()
CAP = 8 * 1024 * 1024
PROMPTS = [
    "请生成一张小猫跳伞的图片。",
    "把它的降落伞换成滑翔伞，保留这只小猫和构图。",
    "现在再生成另一张独立图片：一位成年女性在沙滩海景中晒太阳。",
    "回到之前那张小猫图，把背景改成日落，保留滑翔伞。",
]
GUIDANCE = "使用可用的图片 MCP 工具实际完成用户请求。每张图只需一张，尺寸1024x1024，quality=low。工具执行后用一句中文回复，不把内部会话ID展示给用户。不要使用其他工具。"
CASES = {
    "claude": {"cli": "claude", "provider": "claude", "model": "claude-sonnet-4-6"},
    "deepseek": {"cli": "opencode", "provider": "deepseek", "model": "deepseek-flash"},
    "claude-deepseek": {"cli": "claude", "provider": "deepseek", "model": "deepseek-v4-pro"},
}


def emit(value):
    with PRINT_LOCK:
        print(json.dumps(value, ensure_ascii=False), flush=True)


def sanitize(value, secret):
    if isinstance(value, str):
        return value.replace(secret, "<REDACTED>") if secret else value
    if isinstance(value, list):
        return [sanitize(item, secret) for item in value]
    if isinstance(value, dict):
        return {key: sanitize(item, secret) for key, item in value.items()}
    return value


def response_usage(raw):
    usage = {}
    response_model = None
    for line in raw.decode("utf-8", "replace").splitlines():
        if not line.startswith("data:"):
            continue
        try:
            obj = json.loads(line[5:].strip())
        except ValueError:
            continue
        message = obj.get("message", {})
        response_model = obj.get("model") or message.get("model") or response_model
        usage.update(message.get("usage") or obj.get("usage") or {})
    if not usage:
        try:
            obj = json.loads(raw)
            usage = obj.get("usage") or {}
            response_model = obj.get("model")
        except ValueError:
            pass
    return usage, response_model


class ModelRelay(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def do_POST(self):
        allowed = ("/v1/messages", "/v1/messages/count_tokens", "/v1/chat/completions", "/chat/completions")
        if self.path.split("?", 1)[0] not in allowed:
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", "0"))
        if length < 0 or length > CAP:
            self.send_error(413)
            return
        body = self.rfile.read(length)
        try:
            obj = json.loads(body)
        except ValueError:
            self.send_error(400)
            return
        started = time.perf_counter()
        with self.server.lock:
            if len(self.server.calls) >= 32:
                self.send_error(429, "bounded harness call limit")
                return
            event = {"request": len(self.server.calls) + 1, "path": self.path,
                     "model": obj.get("model"), "request_bytes": len(body),
                     "tools": len(obj.get("tools", [])),
                     "tool_definition_bytes": len(json.dumps(obj.get("tools", []), ensure_ascii=False).encode()),
                     "thinking": obj.get("thinking"), "reasoning_effort": obj.get("reasoning_effort"),
                     "native_user_agent": self.headers.get("user-agent"),
                     "native_x_app": self.headers.get("x-app"),
                     "status": None}
            self.server.calls.append(event)
        emit({"event": "model_request", "case": self.server.case_name, **event})
        headers = {
            "Authorization": "Bearer " + self.server.secret,
            "x-api-key": self.server.secret,
            "Content-Type": "application/json", "Accept-Encoding": "identity",
        }
        # Preserve the native harness identity headers; replace only transport/auth headers.
        excluded = {"host", "content-length", "connection", "transfer-encoding", "accept-encoding", "authorization", "x-api-key"}
        for name, value in self.headers.items():
            if name.lower() not in excluded:
                headers[name] = value
        path = self.path
        upstream_base = self.server.upstream
        if self.server.provider == "deepseek" and "messages" in path:
            upstream_base += "/anthropic"
        if self.server.provider == "deepseek" and path == "/chat/completions":
            path = "/v1/chat/completions"
        try:
            with self.server.client.stream("POST", upstream_base + path, headers=headers, content=body) as response:
                event["status"] = response.status_code
                event["headers_ms"] = round((time.perf_counter() - started) * 1000, 3)
                self.send_response(response.status_code)
                self.send_header("Content-Type", response.headers.get("Content-Type", "application/json"))
                self.send_header("Connection", "close")
                if response.headers.get("Retry-After"):
                    self.send_header("Retry-After", response.headers["Retry-After"])
                self.end_headers()
                captured = bytearray()
                pending = b""
                secret = self.server.secret.encode()
                keep = max(len(secret) - 1, 0)
                for chunk in response.iter_bytes():
                    if len(captured) + len(chunk) > CAP:
                        raise ValueError("model response cap")
                    if "first_byte_ms" not in event:
                        event["first_byte_ms"] = round((time.perf_counter() - started) * 1000, 3)
                    captured.extend(chunk)
                    pending = (pending + chunk).replace(secret, b"<REDACTED>")
                    if len(pending) > keep:
                        self.wfile.write(pending[:-keep] if keep else pending)
                        self.wfile.flush()
                        pending = pending[-keep:] if keep else b""
                if pending:
                    self.wfile.write(pending.replace(secret, b"<REDACTED>"))
                    self.wfile.flush()
                event["response_bytes"] = len(captured)
                event["usage"], event["response_model"] = response_usage(bytes(captured))
                if response.status_code >= 400:
                    try:
                        error = json.loads(captured).get("error", {})
                        event["error"] = sanitize(error, self.server.secret)
                    except ValueError:
                        event["error"] = "non-JSON upstream error"
        except Exception as error:
            event["transport_error_type"] = type(error).__name__
        finally:
            event["wall_ms"] = round((time.perf_counter() - started) * 1000, 3)
            self.close_connection = True
            emit({"event": "model_response", "case": self.server.case_name,
                  "request": event["request"], "status": event.get("status"),
                  "wall_ms": event["wall_ms"], "usage": event.get("usage")})


class LiveImageApi:
    """User-authorized live image transport; the credential is never written into config files."""
    def __enter__(self):
        if os.environ.get("MICU_RUN_LIVE_TESTS") != "1":
            raise RuntimeError("live images require MICU_RUN_LIVE_TESTS=1")
        try:
            import tomllib
        except ModuleNotFoundError:
            import tomli as tomllib
        from tests.contract.live_quality_matrix import RelayHandler
        config = tomllib.loads((Path.home() / ".codex/config.toml").read_text())["mcp_servers"]["micu-image"]["env"]
        if config.get("MICU_BASEURL") != "https://www.micuapi.ai":
            raise RuntimeError("unexpected live image destination")
        self.expected_key = config["MICU_API_KEY"]
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), RelayHandler)
        self.server.daemon_threads = True
        self.server.key, self.server.base_url = self.expected_key, "https://www.micuapi.ai"
        self.server.requests, self.server.lock = [], threading.Lock()
        self.server.client = httpx.Client(trust_env=False, follow_redirects=False, timeout=180)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def env(self):
        return {"MICU_API_KEY": self.expected_key, "MICU_BASEURL": f"http://127.0.0.1:{self.server.server_port}",
                "MICU_USE_SHELL_PROXY": "0"}

    def __exit__(self, *_args):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=2)
        self.server.client.close()


def invoke(command, env, cwd, timeout, secret):
    process = subprocess.Popen(command, env=env, cwd=cwd, stdin=subprocess.DEVNULL,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                               start_new_session=True)
    started = time.perf_counter()
    timed_out = False
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        timed_out = True
        os.killpg(process.pid, signal.SIGTERM)
        try:
            stdout, stderr = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            stdout, stderr = process.communicate()
    events = []
    for line in stdout.splitlines():
        try:
            events.append(json.loads(line))
        except ValueError:
            pass
    return {"exit_code": 124 if timed_out else process.returncode, "timed_out": timed_out, "wall_ms": round((time.perf_counter() - started) * 1000, 3),
            "events": sanitize(events, secret), "stderr": sanitize(stderr[-4000:], secret)}


def audit_calls(path):
    if not path.is_file():
        return []
    events = [json.loads(line) for line in path.read_text().splitlines()]
    return [event for event in events if event.get("kind") == "response" and event.get("method") == "tools/call"]


def validate_calls(calls, allow_failed_attempts=False):
    all_images = [call for call in calls if call.get("tool") in ("image_generate", "image_edit")]
    failures = [call for call in all_images if (call.get("result") or {}).get("ok") is not True]
    if failures and not allow_failed_attempts:
        return {"ok": False, "reason": "image calls failed", "failed_attempts": len(failures)}
    images = [call for call in all_images if (call.get("result") or {}).get("ok") is True]
    if len(images) != 4:
        return {"ok": False, "reason": f"expected 4 image calls, got {len(images)}"}
    values = [call.get("result") or {} for call in images]
    if not all(value.get("ok") is True for value in values):
        return {"ok": False, "reason": "an image call failed"}
    ids = [value.get("session", {}).get("id") for value in values]
    versions = [value.get("session", {}).get("revision") for value in values]
    expected = bool(ids[0] and ids[0] == ids[1] == ids[3] and ids[2] and ids[2] != ids[0])
    if not expected:
        return {"ok": False, "reason": "session routing was not A,A,B,A", "session_ids": ids, "revisions": versions}
    if versions != [1, 2, 1, 3]:
        return {"ok": False, "reason": "unexpected revision chain", "revisions": versions}
    parents = [value.get("session", {}).get("parent_revision") for value in values]
    histories = [value.get("session", {}).get("history_length") for value in values]
    if parents != [None, 1, None, 2] or histories != [1, 2, 1, 3]:
        return {"ok": False, "reason": "parent/history selected the wrong revision", "parents": parents, "history_lengths": histories}
    if images[0]["tool"] != "image_generate" or images[2]["tool"] != "image_generate":
        return {"ok": False, "reason": "independent images were not generated from text"}
    def output_path(value):
        saved = value.get("saved") or {}
        return (saved[0] if isinstance(saved, list) else saved).get("path")
    if values[1]["session"].get("input_path") != output_path(values[0]) or values[3]["session"].get("input_path") != output_path(values[1]):
        return {"ok": False, "reason": "continued edits used the wrong image"}
    new_context = json.dumps(values[2].get("session", {}).get("context", {}), ensure_ascii=False)
    if re.search(r"小猫|小狗|滑翔伞|降落伞|摩托|\b(?:cat|dog|motorbike|motorcycle|parachute|paraglider)\b", new_context, re.IGNORECASE):
        return {"ok": False, "reason": "new image task inherited previous-task constraints"}
    return {"ok": True, "routing": ["A", "A", "B", "A"], "revisions": versions,
            "new_task_context_isolated": True, "successful_image_calls": 4,
            "failed_attempts_recovered": len(failures)}


def run_case(name, provider, root, variant, timeout, persistent=False, live_images=False):
    case = CASES[name]
    work = root / (name + "-" + variant)
    work.mkdir(parents=True, exist_ok=True)
    audit = work / "mcp-audit.jsonl"
    relay = ThreadingHTTPServer(("127.0.0.1", 0), ModelRelay)
    relay.daemon_threads = True
    relay.secret, relay.upstream, relay.provider = provider["key"], provider["base_url"], case["provider"]
    relay.calls, relay.lock, relay.case_name = [], threading.Lock(), name + "-" + variant
    relay.client = httpx.Client(trust_env=False, follow_redirects=False, timeout=150)
    thread = threading.Thread(target=relay.serve_forever, daemon=True)
    thread.start()
    turns = []
    error = None
    attached_server = None
    server_start_ms = 0
    image_posts = []
    try:
        with (LiveImageApi() if live_images else MockMicuApi()) as images:
            image_env = isolated_server_env(work / "images", images.env())
            (work / "images").mkdir()
            image_env["MICU_INPUT_ROOT"] = str(work / "images")
            image_env["MICU_RESPONSE_FORMAT"] = "b64_json"
            image_env["MICU_RUN_LIVE_TESTS"] = "1" if live_images else "0"
            proxy = [str(REPO_ROOT / ".venv/bin/python"), "-m", "tests.contract.mcp_audit_proxy",
                     "--binary", str(REPO_ROOT / "target/release/micu-image-mcp"), "--audit", str(audit)]
            if variant != "full":
                proxy += ["--catalog", str(root / "compact-catalog.json")]
            env = os.environ.copy()
            env.update({"ANTHROPIC_API_KEY": "sk-local-harness-dummy-not-an-upstream-secret",
                        "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{relay.server_port}",
                        "DEEPSEEK_API_KEY": "sk-local-harness-dummy-not-an-upstream-secret",
                        "CLAUDE_CONFIG_DIR": str(work / "claude-config"),
                        "XDG_CONFIG_HOME": str(work / "config"), "XDG_DATA_HOME": str(work / "data"),
                        "XDG_CACHE_HOME": str(root / "cache"),
                        "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1", "ENABLE_TOOL_SEARCH": "false",
                        "DISABLE_AUTOUPDATER": "1", "OPENCODE_DISABLE_AUTOUPDATE": "1"})
            env.pop("ANTHROPIC_AUTH_TOKEN", None)
            mcp_env = {key: value for key, value in image_env.items() if key.startswith("MICU_") or key in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY")}
            mcp_env["PYTHONPATH"] = str(REPO_ROOT)
            if live_images:
                # The CLI passes this through to its MCP child, never via a file or argv.
                env["MICU_API_KEY"] = mcp_env.pop("MICU_API_KEY")
            (work / "claude-config").mkdir(exist_ok=True)
            mcp_path = work / "mcp.json"
            mcp_path.write_text(json.dumps({"mcpServers": {"micu": {"type": "stdio", "command": proxy[0], "args": proxy[1:], "env": mcp_env}}}))
            model = case["model"]
            session_id = str(uuid.uuid4())
            if case["cli"] == "opencode":
                config = {"$schema": "https://opencode.ai/config.json", "enabled_providers": ["deepseek"],
                    "provider": {"deepseek": {"options": {"baseURL": f"http://127.0.0.1:{relay.server_port}/v1", "apiKey": "{env:DEEPSEEK_API_KEY}"},
                        "models": {model: {"name": model, "tool_call": True, "reasoning": True,
                                           "limit": {"context": 1000000, "output": 8192}}}}},
                    "tools": {"*": False, "micu_*": True}, "permission": {"*": "deny", "micu_*": "allow"},
                    "mcp": {"micu": {"type": "local", "command": proxy, "environment": mcp_env, "enabled": True}},
                    "agent": {"image": {"mode": "primary", "prompt": GUIDANCE, "steps": 8}}}
                env["OPENCODE_CONFIG_CONTENT"] = json.dumps(config)
            attached_url = None
            if persistent and case["cli"] == "opencode":
                with socket.socket() as probe:
                    probe.bind(("127.0.0.1", 0))
                    port = probe.getsockname()[1]
                attached_url = f"http://127.0.0.1:{port}"
                env["OPENCODE_SERVER_PASSWORD"] = uuid.uuid4().hex
                started = time.perf_counter()
                attached_server = subprocess.Popen(
                    [str(TOOLS_ROOT / "opencode"), "serve", "--hostname", "127.0.0.1", "--port", str(port)],
                    env=env, cwd=work, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                    stderr=subprocess.DEVNULL, start_new_session=True,
                )
                with httpx.Client(trust_env=False, timeout=1,
                                  auth=("opencode", env["OPENCODE_SERVER_PASSWORD"])) as ready:
                    while time.perf_counter() - started < 30:
                        if attached_server.poll() is not None:
                            raise RuntimeError("persistent native server exited")
                        try:
                            if ready.get(attached_url + "/global/health").is_success:
                                break
                        except httpx.HTTPError:
                            pass
                        time.sleep(0.1)
                    else:
                        raise TimeoutError("persistent native server readiness timeout")
                server_start_ms = round((time.perf_counter() - started) * 1000, 3)
            for index, prompt in enumerate(PROMPTS):
                emit({"event": "turn_start", "case": name + "-" + variant, "turn": index + 1})
                if case["cli"] == "claude":
                    command = [str(TOOLS_ROOT / "claude"), "--bare", "-p", prompt,
                               "--output-format", "stream-json", "--verbose", "--include-partial-messages",
                               "--model", model, "--effort", "low", "--tools", "mcp__micu__image_generate,mcp__micu__image_edit,mcp__micu__image_batch_edit,mcp__micu__image_multi_reference,mcp__micu__server_info",
                               "--allowedTools", "mcp__micu__*", "--permission-mode", "dontAsk",
                               "--permission-prompts", "none", "--strict-mcp-config", "--mcp-config", str(mcp_path),
                               "--append-system-prompt", GUIDANCE]
                    command += ["--session-id", session_id] if index == 0 else ["--resume", session_id]
                else:
                    command = [str(TOOLS_ROOT / "opencode"), "run", "--pure", "--format", "json",
                               "--model", "deepseek/" + model, "--agent", "image", prompt]
                    if attached_url:
                        command += ["--attach", attached_url]
                    if index:
                        command += ["--session", session_id]
                before = len(relay.calls)
                outcome = invoke(command, env, work, timeout, provider["key"])
                turns.append({"turn": index + 1, **outcome})
                if case["cli"] == "opencode":
                    session_id = next((event.get("sessionID") or event.get("part", {}).get("sessionID")
                                       for event in outcome["events"] if event.get("sessionID") or event.get("part", {}).get("sessionID")), session_id)
                emit({"event": "turn_complete", "case": name + "-" + variant, "turn": index + 1,
                      "exit_code": outcome["exit_code"], "wall_ms": outcome["wall_ms"],
                      "model_requests": len(relay.calls) - before,
                      "image_calls_so_far": len([call for call in audit_calls(audit) if call.get("tool") in ("image_generate", "image_edit")])})
                if outcome["exit_code"] != 0:
                    break
            if live_images:
                image_posts = [dict(item) for item in images.server.requests]
    except Exception as exc:
        error = type(exc).__name__
    finally:
        if attached_server is not None and attached_server.poll() is None:
            os.killpg(attached_server.pid, signal.SIGTERM)
            try:
                attached_server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(attached_server.pid, signal.SIGKILL)
                attached_server.wait()
        relay.shutdown()
        relay.server_close()
        thread.join(timeout=2)
        relay.client.close()
    calls = audit_calls(audit)
    validation = validate_calls(calls, allow_failed_attempts=live_images)
    report = {"case": name, "variant": variant, "cli": case["cli"], "model": case["model"],
              "provider_base_url": provider["base_url"], "validation": validation, "error_type": error,
              "harness_process_mode": "persistent_server" if persistent and case["cli"] == "opencode" else "per_turn_cli",
              "persistent_server_start_ms": server_start_ms,
              "turns": turns, "model_requests": relay.calls, "mcp_calls": calls,
              "paid_image_requests": len(image_posts), "upstream_image_posts": image_posts,
              "validation_scope": "real model/native harness and live image API" if live_images else "real model/native harness; local image API fixture"}
    (work / "report.json").write_text(json.dumps(sanitize(report, provider["key"]), ensure_ascii=False, indent=2) + "\n")
    emit({"event": "case_complete", "case": name, "variant": variant, "validation": validation, "error_type": error})
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cases", default="claude,deepseek")
    parser.add_argument("--variant", choices=("full", "compact"), default="full")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--persistent", action="store_true")
    parser.add_argument("--scenario", choices=("cat", "dog"), default="cat")
    parser.add_argument("--live-images", action="store_true")
    args = parser.parse_args()
    if args.scenario == "dog":
        PROMPTS[:] = [
            "生成一只戴头盔的小狗骑摩托车的图片。",
            "把头盔改成黄色，保持小狗和摩托车。",
            "另外生成一张全新的森林旅行者肖像，人物为成年人。",
            "回到小狗骑摩托车那张，把背景变成雨夜，保持黄色头盔。",
        ]
    if os.environ.get("MICU_RUN_HARNESS_TESTS") != "1":
        raise SystemExit("Real model harness testing requires MICU_RUN_HARNESS_TESTS=1")
    # Honor requested pixels so an agent is not induced to retry a deliberately tiny fixture.
    from tests.contract import mock_micu_api
    original_scenario = mock_micu_api._scenario
    mock_micu_api._scenario = lambda prompt: "exact_b64" if original_scenario(prompt) == "b64" else original_scenario(prompt)
    providers = json.loads(sys.stdin.readline())
    cases = args.cases.split(",")
    if not cases or any(name not in CASES for name in cases):
        raise SystemExit("Unknown native harness case")
    allowed = {"claude": {"https://sub.micuapi.ai", "https://api.anthropic.com"},
               "deepseek": {"https://api.deepseek.com"}}
    for name in {CASES[case]["provider"] for case in cases}:
        provider = providers.get(name, {})
        if provider.get("base_url") not in allowed[name] or not provider.get("key"):
            raise SystemExit("Invalid configured provider; credentials omitted")
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=True)
    if (root / "summary.json").exists():
        raise SystemExit("Use a fresh report directory")
    results = []
    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(run_case, name, providers[CASES[name]["provider"]], root, args.variant, args.timeout, args.persistent, args.live_images) for name in cases]
        for future in as_completed(futures):
            results.append(future.result())
            summary = {"host": "osaka", "results": [{key: result[key] for key in ("case", "variant", "model", "validation", "error_type")} for result in results]}
            (root / "summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")


if __name__ == "__main__":
    main()
