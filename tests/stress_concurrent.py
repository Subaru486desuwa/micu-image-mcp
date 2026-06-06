"""并发压力测试。

目标:
  1. 1K 多张并发 (n>1 → server 内 5 并发) → 吞吐线性 ≈ 5x
  2. ≥2K 多请求并发 → 进程内 Semaphore(1) + 跨进程 flock 强串行
  3. ≥4K 撞 CF 524 → fail-fast 不重试 (锁视角)
  4. Grok 并发 → 不走锁，可线性并发

模式:
  inprocess (默认): asyncio.gather N 个 image_generate 调用。
    验证进程内 Semaphore + 跨进程 flock (与自身串行，等价于进程内 1)。

  multiprocess: spawn N 个 python 子进程, 各自打 1 个 image_generate.
    这是更真实的多 Claude Code 窗口场景, 验证跨进程 flock。

用法:
  # 默认 smoke: inprocess, concurrency=3, image2 1024² × 3
  python tests/stress_concurrent.py

  # 验证 2K 锁串行
  python tests/stress_concurrent.py --size 2048x2048 --concurrency 4

  # 跨进程模式 (开多窗口模拟)
  python tests/stress_concurrent.py --mode multiprocess --concurrency 3 --size 2048x2048

  # Grok 并发
  python tests/stress_concurrent.py --model grok-imagine-image-lite --size 1024x1024 --concurrency 5
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import statistics
import subprocess
import sys
import time
from pathlib import Path

from _common import (
    Trial,
    ensure_save_dir,
    has_grok_key,
    has_image2_key,
    import_server,
    parse_actual_size,
    percentile,
    summarize,
    write_report,
)

PROMPT = (
    "A minimalist studio photograph of a single red apple on a white background, "
    "soft natural lighting, 50mm lens, ultra clean composition."
)


# ---------- 单进程内并发 ----------

def _first_error(r: dict) -> str:
    """image_generate 入口拒填 r['error']；HTTP 路径仅填 r['errors']。统一取一个非空 str。"""
    if r.get("error"):
        return str(r["error"])
    errs = r.get("errors") or []
    return str(errs[0]) if errs else ""


async def _one_call(server, *, model: str, size: str, idx: int) -> Trial:
    label = f"{model}|{size}|c{idx}"
    t0 = time.perf_counter()
    try:
        r = await server.image_generate(
            prompt=PROMPT,
            size=size,
            n=1,
            model=model,
            basename=f"stress_c{idx}_{int(time.time()*1000)}",
        )
    except Exception as e:  # noqa: BLE001
        elapsed = (time.perf_counter() - t0) * 1000
        return Trial(
            label=label,
            model=model,
            size=size,
            ok=False,
            wall_ms=elapsed,
            error=f"{type(e).__name__}: {e}",
            extra={"worker_idx": idx},
        )
    elapsed = (time.perf_counter() - t0) * 1000
    saved = r.get("saved") or []
    first = saved[0] if saved else {}
    actual_t = parse_actual_size(first.get("actual_size"))
    notes = list(r.get("notes") or [])
    wait_note = next((n for n in notes if "锁" in n or "lock" in n.lower()), None)
    return Trial(
        label=label,
        model=model,
        size=size,
        ok=bool(r.get("ok")),
        wall_ms=elapsed,
        saved_count=len(saved),
        actual_size=actual_t,
        notes=notes[:8],
        error=_first_error(r)[:240] if not r.get("ok") else None,
        extra={
            "worker_idx": idx,
            "lock_wait_note": wait_note,
            "size_bytes": first.get("size_bytes"),
        },
    )


async def run_inprocess(args) -> tuple[list[Trial], float]:
    server = import_server()
    print(f"[..] inprocess: concurrency={args.concurrency} model={args.model} size={args.size}")
    t0 = time.perf_counter()
    tasks = [
        _one_call(server, model=args.model, size=args.size, idx=i)
        for i in range(args.concurrency)
    ]
    trials = await asyncio.gather(*tasks)
    total = (time.perf_counter() - t0) * 1000
    return list(trials), total


# ---------- 跨进程并发 ----------

WORKER_SCRIPT = r"""
import asyncio, json, os, sys, time
sys.path.insert(0, os.environ["MICU_REPO_ROOT"])
import server
def _first_error(r):
    if r.get("error"):
        return str(r["error"])
    errs = r.get("errors") or []
    return str(errs[0]) if errs else ""
async def main():
    t0 = time.perf_counter()
    r = await server.image_generate(
        prompt=os.environ["MICU_PROMPT"],
        size=os.environ["MICU_SIZE"],
        n=1,
        model=os.environ["MICU_MODEL"],
        basename=f"stress_mp_{os.getpid()}_{int(time.time()*1000)}",
    )
    elapsed_ms = (time.perf_counter() - t0) * 1000
    saved = r.get("saved") or []
    first = saved[0] if saved else {}
    actual = first.get("actual_size")
    payload = {
        "ok": bool(r.get("ok")),
        "wall_ms": elapsed_ms,
        "saved_count": len(saved),
        "actual_size": actual,
        "notes": list(r.get("notes") or [])[:8],
        "error": _first_error(r)[:240] if not r.get("ok") else None,
        "pid": os.getpid(),
    }
    sys.stdout.write(json.dumps(payload))
asyncio.run(main())
"""


def _spawn_worker(idx: int, *, model: str, size: str, save_root: Path) -> subprocess.Popen:
    env = os.environ.copy()
    env["MICU_REPO_ROOT"] = str(Path(__file__).resolve().parent.parent)
    env["MICU_PROMPT"] = PROMPT
    env["MICU_SIZE"] = size
    env["MICU_MODEL"] = model
    env["MICU_SAVE_DIR_ROOT"] = str(save_root)
    env["MICU_SAVE_DIR"] = str(save_root)
    return subprocess.Popen(
        [sys.executable, "-c", WORKER_SCRIPT],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def run_multiprocess(args, save_root: Path) -> tuple[list[Trial], float]:
    print(f"[..] multiprocess: workers={args.concurrency} model={args.model} size={args.size}")
    t0 = time.perf_counter()
    procs = [
        _spawn_worker(i, model=args.model, size=args.size, save_root=save_root)
        for i in range(args.concurrency)
    ]
    trials: list[Trial] = []
    for i, p in enumerate(procs):
        try:
            out, errout = p.communicate(timeout=args.worker_timeout)
        except subprocess.TimeoutExpired:
            p.kill()
            out, errout = p.communicate()
            trials.append(Trial(
                label=f"{args.model}|{args.size}|w{i}",
                model=args.model,
                size=args.size,
                ok=False,
                wall_ms=args.worker_timeout * 1000,
                error=f"worker {i} timeout > {args.worker_timeout}s",
                extra={"worker_idx": i, "stderr_tail": errout[-300:].decode(errors="replace")},
            ))
            continue
        try:
            payload = json.loads(out.decode("utf-8", errors="replace"))
        except Exception as e:  # noqa: BLE001
            trials.append(Trial(
                label=f"{args.model}|{args.size}|w{i}",
                model=args.model,
                size=args.size,
                ok=False,
                wall_ms=0,
                error=f"worker {i} bad output: {e}",
                extra={"stderr_tail": errout[-300:].decode(errors="replace")},
            ))
            continue
        actual = payload.get("actual_size")
        actual_t = (int(actual[0]), int(actual[1])) if isinstance(actual, list) and len(actual) == 2 else None
        notes = payload.get("notes") or []
        wait_note = next((n for n in notes if "锁" in n or "lock" in n.lower()), None)
        trials.append(Trial(
            label=f"{args.model}|{args.size}|w{i}",
            model=args.model,
            size=args.size,
            ok=bool(payload.get("ok")),
            wall_ms=float(payload.get("wall_ms", 0)),
            saved_count=int(payload.get("saved_count", 0)),
            actual_size=actual_t,
            notes=notes,
            error=payload.get("error"),
            extra={
                "worker_idx": i,
                "worker_pid": payload.get("pid"),
                "lock_wait_note": wait_note,
            },
        ))
    total = (time.perf_counter() - t0) * 1000
    return trials, total


# ---------- 主流程 ----------

def derived_metrics(trials: list[Trial], total_wall_ms: float) -> dict:
    """
    Concurrency 解释：
      并发执行下 wall_ms 已经包含锁/排队等待。直接 sum(wall_ms) 会把"等锁"算进串行下界，
      串行场景里得到的 efficiency 反而 ≈ 0.5（误导成"部分并发"）。
      正确做法：用 min(wall_ms) 近似"单张净耗时"，N × min 当串行下界，再与 total_wall 比。
      串行场景 total ≈ N × min → efficiency ≈ 1；
      并发场景 total ≈ min   → efficiency ≈ 1/N。
    """
    oks = [t for t in trials if t.ok]
    lat = [t.wall_ms for t in oks]
    n = len(trials)
    min_lat = min(lat) if lat else 0
    serial_estimate_ms = min_lat * n if lat else 0
    waited = [t for t in trials if t.extra.get("lock_wait_note")]
    return {
        "total_wall_ms": round(total_wall_ms, 1),
        "n_total": n,
        "n_ok": len(oks),
        "n_fail": n - len(oks),
        "p50_ms": round(percentile(lat, 50) or 0, 1) if lat else None,
        "p95_ms": round(percentile(lat, 95) or 0, 1) if lat else None,
        "mean_ms": round(statistics.fmean(lat), 1) if lat else None,
        "min_ms": round(min_lat, 1) if lat else None,
        "max_ms": round(max(lat), 1) if lat else None,
        "serial_estimate_ms": round(serial_estimate_ms, 1),
        "concurrency_efficiency": round(total_wall_ms / serial_estimate_ms, 3) if serial_estimate_ms else None,
        "lock_wait_observed": len(waited),
    }


def main() -> None:
    p = argparse.ArgumentParser(description="米醋 MCP 并发压力测试")
    p.add_argument("--mode", choices=["inprocess", "multiprocess"], default="inprocess")
    p.add_argument("--concurrency", type=int, default=3)
    p.add_argument("--model", default="gpt-image-2")
    p.add_argument("--size", default="1024x1024")
    p.add_argument("--worker-timeout", type=int, default=300,
                   help="multiprocess 模式下单个 worker 最大等待秒数")
    p.add_argument("--out-dir", default=str(Path(__file__).parent / "reports"))
    args = p.parse_args()

    save_root = ensure_save_dir(f"stress_{args.mode}")
    print(f"[..] 沙箱根: {save_root}")

    # key 校验
    is_grok = args.model.startswith("grok-")
    if is_grok and not has_grok_key():
        print("[ERR] 选了 grok model 但 MICU_GROK_API_KEY 未配置")
        sys.exit(1)
    if not is_grok and not has_image2_key():
        print("[ERR] MICU_API_KEY 未配置")
        sys.exit(1)

    if args.mode == "inprocess":
        trials, total_ms = asyncio.run(run_inprocess(args))
    else:
        trials, total_ms = run_multiprocess(args, save_root)

    metrics = derived_metrics(trials, total_ms)
    summary = summarize(trials, group_key=lambda t: f"{t.model}|{t.size}")
    for k in summary:
        summary[k].update({"derived": metrics})

    out_dir = Path(args.out_dir).expanduser()
    json_path, md_path = write_report(
        title=f"stress_{args.mode}",
        summary=summary,
        raw_trials=trials,
        out_dir=out_dir,
        meta={
            "mode": args.mode,
            "concurrency": args.concurrency,
            "model": args.model,
            "size": args.size,
            **metrics,
        },
    )

    print()
    print("=== 关键指标 ===")
    for k, v in metrics.items():
        print(f"  {k:30s} {v}")
    print(f"\n[OK] 报告:\n  {md_path}\n  {json_path}")

    # 解读
    eff = metrics.get("concurrency_efficiency")
    if eff is not None:
        if eff > 0.85:
            print(f"[结论] efficiency={eff} ≈ 1 → 强串行（≥2K 锁生效 / 单线程顺序）")
        elif eff < (1.0 / args.concurrency) * 1.6:
            print(f"[结论] efficiency={eff} ≈ 1/{args.concurrency} → 强并发（无锁 / 多 worker 真并行）")
        else:
            print(f"[结论] efficiency={eff} 中间值（部分排队 / 锁等待 + 部分并行）")

    bad = metrics["n_fail"]
    sys.exit(0 if bad == 0 else 2)


if __name__ == "__main__":
    main()
