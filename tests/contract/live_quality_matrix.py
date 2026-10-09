"""Explicitly gated paid quality validation through the Rust MCP. No key is logged/saved."""
from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import threading
import time

import httpx
from PIL import Image, ImageDraw

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.stdio_driver import StdioSession, text_content_json

MODELS = ["gpt-image-2.5-flare", "gpt-image-2.5-sunburst", "gpt-image-2", "gpt-image-2-openai"]
QUALITIES = [None, "auto", "low", "medium", "high", "xhigh", "max"]
BASE_URLS = ["https://www.micuapi.ai", "https://sub.micuapi.ai"]
CAP = 25 * 1024 * 1024


def matrix(models: list[str]) -> list[dict]:
    return [
        {"index": index, "model": model, "tool": tool, "quality": quality}
        for index, (model, tool, quality) in enumerate(
            ((model, tool, quality) for model in models
             for tool in ("image_generate", "image_edit")
             for quality in QUALITIES
             if "2.5" in model or quality not in ("xhigh", "max")), start=1
        )
    ]


def scrub(value, key: str):
    if isinstance(value, str):
        return value.replace(key, "[REDACTED]") if key else value
    if isinstance(value, list):
        return [scrub(item, key) for item in value]
    if isinstance(value, dict):
        return {name: scrub(item, key) for name, item in value.items()}
    return value


class RelayHandler(BaseHTTPRequestHandler):
    """Count upstream image POSTs, including MCP retries; destination is a fixed Micu host."""
    def log_message(self, *_args) -> None:
        pass

    def do_POST(self) -> None:
        if self.path not in ("/v1/images/generations", "/v1/images/edits"):
            self.send_error(404)
            return
        length = int(self.headers.get("Content-Length", "0"))
        if length < 0 or length > 8 * 1024 * 1024:
            self.send_error(413)
            return
        if self.headers.get("Authorization") != "Bearer " + self.server.key:
            self.send_error(401)
            return
        body = self.rfile.read(length)
        started = time.monotonic()
        with self.server.lock:
            sequence = len(self.server.requests) + 1
            event = {"sequence": sequence, "path": self.path, "status": None}
            self.server.requests.append(event)
        try:
            headers = {
                "Authorization": "Bearer " + self.server.key,
                "Content-Type": self.headers.get("Content-Type", "application/json"),
                "User-Agent": "micu-image-mcp-rust/0.3.3",
            }
            with self.server.client.stream("POST", self.server.base_url + self.path,
                                           headers=headers, content=body) as upstream:
                chunks, total = [], 0
                for chunk in upstream.iter_bytes():
                    total += len(chunk)
                    if total > CAP:
                        raise ValueError("upstream response cap")
                    chunks.append(chunk)
                payload = b"".join(chunks)
                status = upstream.status_code
                content_type = upstream.headers.get("Content-Type", "application/json")
            with self.server.lock:
                event.update(status=status, elapsed_seconds=round(time.monotonic() - started, 3))
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        except Exception as error:
            with self.server.lock:
                event.update(status=0, error_type=type(error).__name__)
            try:
                self.send_error(502, "Upstream request failed; details omitted")
            except OSError:
                pass


def run_case(case: dict, env: dict, binary: Path, source: Path, key: str) -> dict:
    started = time.monotonic()
    quality_label = case["quality"] or "omitted"
    print(json.dumps({"event": "started", **case}), flush=True)
    arguments = {
        "prompt": ("A clean studio product photograph of a cobalt blue ceramic cup on a red table, "
                   "plain white background, soft natural light, no text, no logo."
                   if case["tool"] == "image_generate" else
                   "Change the blue cup to yellow; preserve the red table, white background and composition."),
        "size": "1024x1024", "model": case["model"], "session_id": "new",
        "basename": f"quality_{case['index']:02d}_{quality_label}",
        "context": {"composition": "single centered cup on a red table, white background"},
    }
    if case["tool"] == "image_edit":
        arguments["image_path"] = str(source)
    if case["quality"] is not None:
        arguments["quality"] = case["quality"]
    try:
        with StdioSession([str(binary)], env, REPO_ROOT, timeout=700) as server:
            server.initialize()
            response = server.request(2, "tools/call", {"name": case["tool"], "arguments": arguments})
            result = text_content_json(response)
            stdout = b"".join(server.stdout_lines)
        assert key.encode() not in stdout
        assert key.encode() not in server.stderr_bytes
        entries = result.get("saved", [])
        if isinstance(entries, dict):
            entries = [entries]
        for entry in entries:
            path = Path(entry["path"])
            assert path.is_file()
            assert path.stat().st_size == entry["size_bytes"]
            with Image.open(path) as image:
                assert f"{image.width}x{image.height}" == entry["actual_size"]
                image.verify()
        return {**case, "elapsed_seconds": round(time.monotonic() - started, 3),
                "ok": bool(result.get("ok")), "result": scrub(result, key)}
    except Exception as error:
        return {**case, "elapsed_seconds": round(time.monotonic() - started, 3),
                "ok": False, "error_type": type(error).__name__}


def key_from_config() -> str:
    try:
        import tomllib
    except ModuleNotFoundError:
        import tomli as tomllib
    data = tomllib.loads((Path.home() / ".codex/config.toml").read_text())
    return data.get("mcp_servers", {}).get("micu-image", {}).get("env", {}).get("MICU_API_KEY", "")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", action="store_true", help="List cases without credentials or network")
    parser.add_argument("--base-url", choices=BASE_URLS, default=BASE_URLS[0])
    parser.add_argument("--key-stdin", action="store_true", help="Read JSON credential payload from stdin; never write it")
    parser.add_argument("--workers", type=int, choices=(1, 2), default=2)
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    if args.plan:
        cases = matrix(MODELS)
        print(json.dumps({"cases": cases, "tool_calls": len(cases), "paid_requests": 0}, ensure_ascii=False, indent=2))
        return
    if os.environ.get("MICU_RUN_LIVE_TESTS") != "1":
        raise SystemExit("Paid validation requires MICU_RUN_LIVE_TESTS=1")
    if args.key_stdin:
        import sys
        key = json.load(sys.stdin).get("key", "")
    else:
        key = os.environ.get("MICU_API_KEY") or key_from_config()
    if not key:
        raise SystemExit("No Image2 credential; raw configuration omitted")
    binary = REPO_ROOT / "target/debug/micu-image-mcp"
    if not binary.is_file():
        raise SystemExit("Build the Rust MCP binary first")
    with httpx.Client(trust_env=False, follow_redirects=False, timeout=30) as client:
        response = client.get(args.base_url + "/v1/models", headers={
            "Authorization": "Bearer " + key, "User-Agent": "micu-image-mcp-rust/0.3.3"})
        if not response.is_success:
            print(json.dumps({"status": "blocked_auth", "http_status": response.status_code,
                              "image_tool_calls": 0, "image_posts": 0}), flush=True)
            return
        available = {item.get("id") for item in response.json().get("data", [])}
    models = [model for model in MODELS if model in available]
    if not models:
        raise SystemExit("Credential exposes no supported image models")
    cases = matrix(models)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    root = (args.output or REPO_ROOT / "tests/reports" / f"live-quality-{stamp}").resolve()
    root.mkdir(parents=True, exist_ok=True)
    report_path = root / "report.json"
    if report_path.exists():
        raise SystemExit("Report already exists; use a new output directory to avoid overwriting evidence")
    source = root / "reference.png"
    image = Image.new("RGB", (1024, 1024), "white")
    draw = ImageDraw.Draw(image)
    draw.rectangle((0, 650, 1023, 1023), fill="#c52b30")
    draw.rounded_rectangle((350, 350, 650, 700), radius=55, fill="#2456cc")
    draw.ellipse((610, 415, 765, 615), outline="#2456cc", width=45)
    image.save(source)
    relay = ThreadingHTTPServer(("127.0.0.1", 0), RelayHandler)
    relay.daemon_threads = True
    relay.key, relay.base_url, relay.requests, relay.lock = key, args.base_url, [], threading.Lock()
    relay.client = httpx.Client(trust_env=False, follow_redirects=False, timeout=180)
    thread = threading.Thread(target=relay.serve_forever, daemon=True)
    thread.start()
    env = os.environ.copy()
    env.update({"MICU_API_KEY": key, "MICU_BASEURL": f"http://127.0.0.1:{relay.server_port}",
                "MICU_SAVE_DIR_ROOT": str(root), "MICU_SAVE_DIR": str(root),
                "MICU_INPUT_ROOT": str(root), "MICU_RESPONSE_FORMAT": "b64_json",
                "MICU_USE_SHELL_PROXY": "0", "MICU_RUN_LIVE_TESTS": "1"})
    results = []
    report = {"status": "running", "base_url": args.base_url, "models": models,
              "requested_size": "1024x1024", "planned_tool_calls": len(cases),
              "price_rmb_per_request_as_provided_by_user": 0.1, "results": results}
    def persist() -> None:
        with relay.lock:
            report["upstream_image_posts"] = [dict(item) for item in relay.requests]
        report["completed_tool_calls"] = len(results)
        report["successes"] = sum(bool(item["ok"]) for item in results)
        temporary = root / ".report.pending"
        temporary.write_text(json.dumps(scrub(report, key), ensure_ascii=False, indent=2) + "\n")
        temporary.replace(report_path)
    persist()
    print(json.dumps({"event": "plan", "models": models, "tool_calls": len(cases),
                      "report_path": str(report_path)}), flush=True)
    try:
        with ThreadPoolExecutor(max_workers=args.workers) as pool:
            futures = [pool.submit(run_case, case, env, binary, source, key) for case in cases]
            for future in as_completed(futures):
                result = future.result()
                results.append(result)
                results.sort(key=lambda item: item["index"])
                persist()
                detail = result.get("result", {}).get("error", "")
                if not detail:
                    errors = result.get("result", {}).get("errors", [])
                    detail = errors[0] if errors else result.get("error_type", "")
                print(json.dumps({"event": "completed", **{name: result.get(name) for name in
                      ("index", "model", "tool", "quality", "ok", "elapsed_seconds")},
                      "detail": str(detail)[:240]}, ensure_ascii=False), flush=True)
        report["status"] = "completed"
        persist()
    finally:
        relay.shutdown()
        relay.server_close()
        thread.join(timeout=2)
        relay.client.close()
    print(json.dumps({"event": "summary", "tool_calls": len(results), "successes": report["successes"],
                      "image_posts": len(relay.requests), "report_path": str(report_path)}), flush=True)


if __name__ == "__main__":
    main()
