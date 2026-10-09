"""Offline issue #11 probe using the real Claude Code client and a local model fixture.

The negative control removes only the modern directory cache fields. No paid
model/image endpoint is used. Can also run the CLI bundled with Claude Desktop.
"""
from __future__ import annotations

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time

TOOL_NAMES = {f"mcp__micu__{name}" for name in
              ("image_generate", "image_edit", "image_batch_edit", "image_multi_reference", "server_info")}
CATALOG_METHODS = {"tools/list", "prompts/list", "resources/list", "resources/templates/list"}


def relay(command: list[str], audit: Path, omit_cache: bool) -> None:
    process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL)
    assert process.stdin and process.stdout
    pending = {}
    lock = threading.Lock()
    def feed():
        try:
            for line in sys.stdin.buffer:
                frame = json.loads(line)
                if "id" in frame and "method" in frame:
                    with lock:
                        pending[str(frame["id"])] = frame["method"]
                process.stdin.write(line)
                process.stdin.flush()
        except (BrokenPipeError, OSError):
            pass
        finally:
            process.stdin.close()
    threading.Thread(target=feed, daemon=True).start()
    try:
        with audit.open("w", encoding="utf-8") as log:
            for line in process.stdout:
                frame = json.loads(line)
                with lock:
                    method = pending.pop(str(frame.get("id")), None)
                result = frame.get("result", {})
                if omit_cache and method in CATALOG_METHODS:
                    result.pop("ttlMs", None)
                    result.pop("cacheScope", None)
                if method:
                    event = {"method": method, "result_keys": list(result)}
                    if method == "tools/list":
                        event["tool_count"] = len(result.get("tools", []))
                        event["ttlMs"] = result.get("ttlMs")
                        event["cacheScope"] = result.get("cacheScope")
                    log.write(json.dumps(event) + "\n")
                    log.flush()
                sys.stdout.write(json.dumps(frame) + "\n")
                sys.stdout.flush()
    finally:
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()


class ModelFixture(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        tools = [tool["name"] for tool in request.get("tools", [])]
        results = [block for message in request.get("messages", [])
                   if isinstance(message.get("content"), list)
                   for block in message["content"] if block.get("type") == "tool_result"]
        self.server.records.append({"tool_names": tools, "tool_results": results})
        call = "mcp__micu__server_info" in tools and not results
        content = ({"type": "tool_use", "id": "toolu_local_server_info",
                    "name": "mcp__micu__server_info", "input": {}} if call else
                   {"type": "text", "text": "LOCAL_FIXTURE_DONE"})
        message = {"id": "msg_local_fixture", "type": "message", "role": "assistant",
                   "model": request.get("model"), "content": [content],
                   "stop_reason": "tool_use" if call else "end_turn", "stop_sequence": None,
                   "usage": {"input_tokens": 1, "output_tokens": 1}}
        if request.get("stream"):
            start = {**content}
            start["input" if call else "text"] = {} if call else ""
            delta = ({"type": "input_json_delta", "partial_json": "{}"} if call else
                     {"type": "text_delta", "text": content["text"]})
            events = [("message_start", {"type": "message_start", "message": {
                **message, "content": [], "stop_reason": None}}),
                ("content_block_start", {"type": "content_block_start", "index": 0, "content_block": start}),
                ("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": delta}),
                ("content_block_stop", {"type": "content_block_stop", "index": 0}),
                ("message_delta", {"type": "message_delta", "delta": {"stop_reason": message["stop_reason"],
                 "stop_sequence": None}, "usage": {"output_tokens": 1}}),
                ("message_stop", {"type": "message_stop"})]
            raw = "".join(f"event: {name}\ndata: {json.dumps(data)}\n\n" for name, data in events).encode()
            mime = "text/event-stream"
        else:
            raw = json.dumps(message).encode()
            mime = "application/json"
        self.send_response(200)
        self.send_header("Content-Type", mime)
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)


def probe(cli: Path, command: list[str], output: Path) -> dict:
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    server = ThreadingHTTPServer(("127.0.0.1", 0), ModelFixture)
    server.records = []
    threading.Thread(target=server.serve_forever, daemon=True).start()
    summaries = []
    try:
        for omit in (True, False):
            case = output / ("missing-cache-fields" if omit else "fixed")
            case.mkdir()
            audit = case / "rpc.jsonl"
            arguments = [str(Path(__file__).resolve()), "--relay", "--server-command-json", json.dumps(command),
                         "--audit", str(audit)] + (["--omit-cache"] if omit else [])
            env = {key: value for key, value in os.environ.items()
                   if not key.startswith(("ANTHROPIC_", "CLAUDE_", "MICU_", "AWS_", "GOOGLE_"))}
            env.update(HOME=str(case), USERPROFILE=str(case), CLAUDE_CONFIG_DIR=str(case / "config"),
                       ANTHROPIC_API_KEY="sk-local-fixture-only", ANTHROPIC_BASE_URL=f"http://127.0.0.1:{server.server_port}",
                       CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1", DISABLE_AUTOUPDATER="1", ENABLE_TOOL_SEARCH="false")
            mcp_env = {"MICU_API_KEY": "sk-local-fixture-only", "MICU_BASEURL": "http://127.0.0.1:9",
                       "MICU_SAVE_DIR": str(case), "MICU_SAVE_DIR_ROOT": str(case), "MICU_USE_SHELL_PROXY": "0"}
            config = case / "mcp.json"
            config.write_text(json.dumps({"mcpServers": {"micu": {
                "command": sys.executable, "args": arguments, "env": mcp_env}}}))
            config.chmod(0o600)
            start = len(server.records)
            began = time.monotonic()
            process = subprocess.run([str(cli), "-p", "Use server_info once then reply OK.",
                "--bare", "--model", "claude-sonnet-4-6", "--output-format", "json",
                "--strict-mcp-config", "--mcp-config", str(config), "--setting-sources", "",
                "--tools", ",".join(sorted(TOOL_NAMES)), "--allowedTools", ",".join(sorted(TOOL_NAMES)),
                "--permission-mode", "dontAsk", "--debug-file", str(case / "client.log")],
                cwd=case, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=45)
            records = server.records[start:]
            names = {name for record in records for name in record["tool_names"]} & TOOL_NAMES
            tool_results = [result for record in records for result in record["tool_results"]]
            rpc = [json.loads(line) for line in audit.read_text().splitlines()] if audit.exists() else []
            debug = (case / "client.log").read_text() if (case / "client.log").exists() else ""
            error_signals = {token: token in debug for token in ("ttlMs", "cacheScope", "Invalid result for tools/list")}
            info_ok = any(not result.get("is_error") and "available_models" in json.dumps(result)
                          for result in tool_results)
            summary = {"case": case.name, "exit_code": process.returncode,
                       "elapsed_seconds": round(time.monotonic()-began, 3), "registered_tools": sorted(names),
                       "server_info_success": info_ok, "catalog": [event for event in rpc if event["method"] == "tools/list"],
                       "rpc_methods": [event["method"] for event in rpc], "error_signals": error_signals}
            summaries.append(summary)
            print(json.dumps(summary, ensure_ascii=False), flush=True)
            if omit and "server/discover" in summary["rpc_methods"]:
                assert not names and not info_ok, "negative control did not reproduce 0 tools"
                assert all(error_signals.values()), "negative control did not match issue #11 validation errors"
            else:
                assert process.returncode == 0 and names == TOOL_NAMES and info_ok, "server was not callable"
                assert summary["rpc_methods"].count("tools/call") == 1
                if not omit:
                    assert len(summary["catalog"]) == 1, "fixed client unexpectedly retried tools/list"
        result = {"client_version": subprocess.check_output([str(cli), "--version"], text=True).strip(),
                  "paid_requests": 0, "cases": summaries}
        (output / "report.json").write_text(json.dumps(result, ensure_ascii=False, indent=2))
        return result
    finally:
        server.shutdown()
        server.server_close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--cli", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--server-command-json")
    parser.add_argument("--relay", action="store_true")
    parser.add_argument("--audit", type=Path)
    parser.add_argument("--omit-cache", action="store_true")
    args = parser.parse_args()
    command = json.loads(args.server_command_json) if args.server_command_json else [str(
        Path(__file__).resolve().parents[2] / "target/debug/micu-image-mcp")]
    if args.relay:
        relay(command, args.audit, args.omit_cache)
    else:
        if not args.cli or not args.output:
            parser.error("--cli and --output are required")
        probe(args.cli, command, args.output)


if __name__ == "__main__":
    main()
