"""Offline session concurrency benchmark; injected provider latency is exactly 200 ms."""
from __future__ import annotations

import argparse
import asyncio
import json
import os
from pathlib import Path
import statistics
import tempfile
import time

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

from tests.contract.contract_cases import REPO_ROOT
from tests.contract.mock_micu_api import MockMicuApi
from tests.contract.stdio_driver import isolated_server_env


def value(result):
    payload = result.model_dump(by_alias=True)
    if payload.get("structuredContent") is not None:
        return payload["structuredContent"]
    return json.loads(payload["content"][0]["text"])


async def sample(workers: int):
    with tempfile.TemporaryDirectory(prefix="micu-session-concurrency-") as directory, MockMicuApi() as api:
        root = Path(directory)
        env = isolated_server_env(root, api.env())
        env.update(TOKIO_WORKER_THREADS=str(workers), MICU_INPUT_ROOT=str(root), MICU_RESPONSE_FORMAT="b64_json")
        params = StdioServerParameters(command=str(REPO_ROOT / "target/release/micu-image-mcp"), env=env)
        async with stdio_client(params) as (read, write), ClientSession(read, write) as session:
            await session.initialize()
            async def generate(index):
                started = time.perf_counter()
                result = value(await session.call_tool("image_generate", {
                    "prompt": f"benchmark {index} [scenario:concurrency_probe]", "session_id": "new",
                    "size": "1024x1024", "quality": "low",
                }))
                assert result["ok"] is True, result
                return result, (time.perf_counter() - started) * 1000
            started = time.perf_counter()
            sequential = [await generate(index) for index in range(4)]
            sequential_ms = (time.perf_counter() - started) * 1000
            started = time.perf_counter()
            parallel = await asyncio.gather(*(generate(index + 4) for index in range(4)))
            parallel_ms = (time.perf_counter() - started) * 1000
            assert len({result["session"]["id"] for result, _ in sequential + parallel}) == 8
            identifier = parallel[0][0]["session"]["id"]
            before = len([r for r in api.state.snapshot() if r["method"] == "POST"])
            competing = await asyncio.gather(*(session.call_tool("image_edit", {
                "prompt": "continue [scenario:concurrency_probe]", "session_id": identifier,
            }) for _ in range(2)))
            results = [value(result) for result in competing]
            assert sorted(result["ok"] for result in results) == [False, True], results
            assert next(result for result in results if result["ok"])["session"]["revision"] == 2
            after = len([r for r in api.state.snapshot() if r["method"] == "POST"])
            assert after - before == 1
            return {"workers": workers, "sequential_four_ms": round(sequential_ms, 3),
                    "parallel_four_ms": round(parallel_ms, 3),
                    "parallel_call_median_ms": round(statistics.median(ms for _, ms in parallel), 3),
                    "max_mock_api_in_flight": api.state.metrics()["max_active_api_requests"],
                    "independent_sessions": 8, "same_session_overlap": "one success, one busy, one upstream request"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--repeat", type=int, default=3)
    args = parser.parse_args()
    if os.environ.get("MICU_RUN_LIVE_TESTS") == "1":
        raise SystemExit("strictly offline benchmark refuses live mode")
    samples = []
    for workers in (1, 2, 4):
        for _ in range(args.repeat):
            result = asyncio.run(sample(workers))
            samples.append(result)
            print(json.dumps(result), flush=True)
    report = {"host": "osaka", "injected_api_delay_ms": 200, "paid_image_requests": 0,
              "samples": samples, "median_by_workers": {
                  str(workers): {field: round(statistics.median(s[field] for s in samples if s["workers"] == workers), 3)
                                 for field in ("sequential_four_ms", "parallel_four_ms", "parallel_call_median_ms")}
                  for workers in (1, 2, 4)}}
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
