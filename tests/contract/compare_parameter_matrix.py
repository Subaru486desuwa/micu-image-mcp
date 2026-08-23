from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

from tests.contract.contract_cases import REPO_ROOT


BASELINE = REPO_ROOT / "tests" / "fixtures" / "image2-parameter-matrix-before-path-refactor.json"


def _run(command: list[str], env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        command,
        cwd=REPO_ROOT,
        env=env,
        check=False,
        capture_output=True,
        text=True,
    )


def compare(output: Path | None = None) -> dict[str, object]:
    baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
    source = REPO_ROOT / baseline["source"]
    source_hash = hashlib.sha256(source.read_bytes()).hexdigest()
    if source_hash != baseline["source_sha256"]:
        raise AssertionError(
            f"parameter matrix source drifted: {source_hash} != {baseline['source_sha256']}"
        )
    env = os.environ.copy()
    env["MICU_RUN_LIVE_TESTS"] = "0"
    collect = _run(
        [sys.executable, "-m", "pytest", "--collect-only", "-q", str(source)], env
    )
    if collect.returncode != 0:
        raise AssertionError(collect.stdout + collect.stderr)
    nodeids = [line for line in collect.stdout.splitlines() if line.startswith("tests/")]
    if nodeids != baseline["nodeids"]:
        raise AssertionError("42-case parameter matrix nodeids changed")
    started = time.perf_counter()
    executed = _run([sys.executable, "-m", "pytest", "-q", str(source)], env)
    elapsed = time.perf_counter() - started
    if executed.returncode != 0:
        raise AssertionError(executed.stdout + executed.stderr)
    payload: dict[str, object] = {
        "baseline_commit": baseline["baseline_commit"],
        "baseline_result": baseline["baseline_result"],
        "after_result": "42 passed",
        "after_elapsed_seconds": round(elapsed, 3),
        "node_count": len(nodeids),
        "nodeids_equal": True,
        "source_sha256": source_hash,
        "status": "PASS",
    }
    if output is not None:
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(
            json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    return payload


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    payload = compare(args.output)
    print(
        f"parameter matrix before/after: {payload['status']} "
        f"({payload['node_count']} cases)"
    )


if __name__ == "__main__":
    main()
