"""Transparent STDIO audit adapter for native harness compatibility tests."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import threading
import time


def scrub(value):
    if isinstance(value, dict):
        return {key: ("<REDACTED>" if any(part in key.lower() for part in ("api_key", "auth_token", "password")) else scrub(item))
                for key, item in value.items()}
    if isinstance(value, list):
        return [scrub(item) for item in value]
    return value


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--audit", type=Path, required=True)
    parser.add_argument("--catalog", type=Path)
    args = parser.parse_args()
    catalog = None
    if args.catalog:
        catalog = json.loads(args.catalog.read_text())["result"]["tools"]
    process = subprocess.Popen([args.binary], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL)
    assert process.stdin and process.stdout
    args.audit.parent.mkdir(parents=True, exist_ok=True)
    pending = {}
    lock = threading.Lock()
    fd = os.open(args.audit, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    log = os.fdopen(fd, "a")

    def record(event):
        with lock:
            log.write(json.dumps(event, ensure_ascii=False) + "\n")
            log.flush()

    def forward_input():
        try:
            for line in sys.stdin.buffer:
                message = json.loads(line)
                if isinstance(message, dict) and "id" in message and "method" in message:
                    request_id = json.dumps(message["id"])
                    now = time.perf_counter_ns()
                    with lock:
                        pending[request_id] = (now, message)
                    event = {"kind": "request", "at_ns": now, "id": message["id"], "method": message["method"]}
                    event["params"] = scrub(message.get("params", {}))
                    record(event)
                process.stdin.write(line)
                process.stdin.flush()
        except (BrokenPipeError, OSError):
            pass
        finally:
            try:
                process.stdin.close()
            except OSError:
                pass

    sender = threading.Thread(target=forward_input, daemon=True)
    sender.start()
    try:
        for line in process.stdout:
            message = json.loads(line)
            request_id = json.dumps(message.get("id"))
            with lock:
                item = pending.pop(request_id, None)
            if item:
                started, request = item
                now = time.perf_counter_ns()
                event = {"kind": "response", "at_ns": now, "id": message["id"], "method": request["method"],
                         "elapsed_ms": round((now - started) / 1e6, 3)}
                if "error" in message:
                    event["error"] = scrub(message["error"])
                event["result_keys"] = list(message.get("result", {}))
                if request["method"] == "server/discover":
                    event["result"] = scrub(message.get("result"))
                if request["method"] == "tools/list":
                    if catalog is not None:
                        actual = message.get("result", {}).get("tools", [])
                        if {tool["name"] for tool in actual} != {tool["name"] for tool in catalog}:
                            raise RuntimeError("catalog override changed tool identities")
                        for actual_tool in actual:
                            override = next(tool for tool in catalog if tool["name"] == actual_tool["name"])
                            if override.get("inputSchema") != actual_tool.get("inputSchema"):
                                raise RuntimeError("catalog override changed the argument schema")
                        message["result"]["tools"] = catalog
                        line = json.dumps(message, ensure_ascii=False).encode() + b"\n"
                    event["tool_count"] = len(message.get("result", {}).get("tools", []))
                    event["catalog_bytes"] = len(line)
                if request["method"] == "tools/call":
                    event["tool"] = request.get("params", {}).get("name")
                    value = message.get("result", {}).get("structuredContent")
                    if value is None:
                        content = message.get("result", {}).get("content", [])
                        for entry in content:
                            if entry.get("type") == "text":
                                try:
                                    value = json.loads(entry["text"])
                                except ValueError:
                                    pass
                                break
                    event["result"] = scrub(value)
                    if "error" in message:
                        event["error"] = scrub(message["error"])
                record(event)
            sys.stdout.buffer.write(line)
            sys.stdout.buffer.flush()
    finally:
        if process.poll() is None:
            process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
        log.close()


if __name__ == "__main__":
    main()
