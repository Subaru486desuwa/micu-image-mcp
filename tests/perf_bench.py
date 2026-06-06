"""性能基线测试。

串行跑 image2 / Grok 在不同 size 下的 image_generate，收集延迟、actual_size。
默认 --smoke 跑最小集合（约 6 张图，~3 分钟）；--full 跑完整 sweep。

用法：
    # smoke（默认）
    python tests/perf_bench.py

    # 跑完整 sweep, 每组重复 3 次
    python tests/perf_bench.py --full --repeat 3

    # 只跑 Grok
    python tests/perf_bench.py --channels grok

    # 干跑（不打 API，只验证导入 / 校验链路通）
    python tests/perf_bench.py --dry-run
"""
from __future__ import annotations

import argparse
import asyncio
import os
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
    summarize,
    write_report,
)


# (channel, model, size) 组合
IMAGE2_SMOKE: list[tuple[str, str, str]] = [
    ("image2", "gpt-image-2", "1024x1024"),
    ("image2", "gpt-image-2-pro", "2048x2048"),
]
IMAGE2_FULL: list[tuple[str, str, str]] = [
    ("image2", "gpt-image-2", "1024x1024"),
    ("image2", "gpt-image-2", "1280x720"),
    ("image2", "gpt-image-2", "1024x1536"),
    ("image2", "gpt-image-2-pro", "2048x2048"),
    ("image2", "gpt-image-2-pro", "2048x1152"),
    ("image2", "gpt-image-2-pro", "3840x2160"),
]
GROK_SMOKE: list[tuple[str, str, str]] = [
    ("grok", "grok-imagine-image-lite", "1024x1024"),
    ("grok", "grok-imagine-image", "2048x2048"),
]
GROK_FULL: list[tuple[str, str, str]] = [
    ("grok", "grok-imagine-image-lite", "1024x1024"),
    ("grok", "grok-imagine-image-lite", "1536x1024"),
    ("grok", "grok-imagine-image", "1024x1024"),
    ("grok", "grok-imagine-image", "2048x2048"),
    ("grok", "grok-imagine-image-pro", "1024x1024"),
]

PROMPT = (
    "A minimalist studio photograph of a single red apple on a white background, "
    "soft natural lighting, 50mm lens, ultra clean composition."
)


def select_combinations(args) -> list[tuple[str, str, str]]:
    src_image2 = IMAGE2_FULL if args.full else IMAGE2_SMOKE
    src_grok = GROK_FULL if args.full else GROK_SMOKE
    combos: list[tuple[str, str, str]] = []
    if args.channels in ("image2", "all"):
        if not has_image2_key() and not args.dry_run:
            print("[!!] MICU_API_KEY 未配置，跳过 image2 组")
        else:
            combos.extend(src_image2)
    if args.channels in ("grok", "all"):
        if not has_grok_key() and not args.dry_run:
            print("[!!] MICU_GROK_API_KEY 未配置，跳过 grok 组")
        else:
            combos.extend(src_grok)
    return combos


async def run_one(server, *, channel: str, model: str, size: str, dry_run: bool) -> Trial:
    label = f"{channel}|{model}|{size}"
    if dry_run:
        # 不打 API：只验证入口校验 + 路由 + 推理（用一个故意会被入口拦的请求）
        t0 = time.perf_counter()
        r = await server.image_generate(
            prompt="dry run smoke",
            size=size,
            model=model,
            api_key="sk-fake-dry-run-token",
        )
        elapsed = (time.perf_counter() - t0) * 1000
        return Trial(
            label=label,
            model=model,
            size=size,
            ok=False,
            wall_ms=elapsed,
            error=str(r.get("error", "dry run"))[:240],
            notes=list(r.get("notes", []))[:8],
            extra={"dry_run": True},
        )

    t0 = time.perf_counter()
    try:
        r = await server.image_generate(
            prompt=PROMPT,
            size=size,
            n=1,
            model=model,
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
        )
    elapsed = (time.perf_counter() - t0) * 1000

    saved = r.get("saved") or []
    first = saved[0] if saved else {}
    actual_t = parse_actual_size(first.get("actual_size"))

    # image_generate 入口拒走 r['error']；HTTP 失败仅走 r['errors']
    err_text: str | None = None
    if not r.get("ok"):
        err_text = str(r.get("error") or "")
        if not err_text:
            errs = r.get("errors") or []
            err_text = str(errs[0]) if errs else ""
        err_text = err_text[:240]

    return Trial(
        label=label,
        model=model,
        size=size,
        ok=bool(r.get("ok")),
        wall_ms=elapsed,
        saved_count=len(saved),
        actual_size=actual_t,
        actual_megapixels=first.get("actual_megapixels"),
        notes=list(r.get("notes") or [])[:8],
        error=err_text,
        extra={"size_bytes": first.get("size_bytes")},
    )


async def main_async(args) -> int:
    combos = select_combinations(args)
    if not combos:
        print("[ERR] 没有可跑的组合（缺 key 或 --channels 排除了全部）")
        return 1

    save_root = ensure_save_dir("perf")
    server = import_server()

    print(f"[..] 沙箱根: {save_root}")
    print(f"[..] {len(combos)} 组 × repeat={args.repeat} = {len(combos) * args.repeat} 张")

    trials: list[Trial] = []
    for i, (channel, model, size) in enumerate(combos):
        for r in range(args.repeat):
            tag = f"{i+1}/{len(combos)} rep {r+1}/{args.repeat}"
            print(f"[..] {tag}  {channel} {model} {size}", end="", flush=True)
            t = await run_one(
                server,
                channel=channel,
                model=model,
                size=size,
                dry_run=args.dry_run,
            )
            mark = "OK" if t.ok else "ER"
            print(f"  [{mark}] {t.wall_ms:.0f}ms  actual={t.actual_size}")
            trials.append(t)

    summary = summarize(trials)
    out_dir = Path(args.out_dir).expanduser()
    json_path, md_path = write_report(
        title="perf_bench",
        summary=summary,
        raw_trials=trials,
        out_dir=out_dir,
        meta={
            "mode": "full" if args.full else "smoke",
            "channels": args.channels,
            "repeat": args.repeat,
            "dry_run": args.dry_run,
            "save_root": str(save_root),
        },
    )
    print(f"\n[OK] 报告:\n  {md_path}\n  {json_path}")

    # 退出码：dry_run 永远 0；否则 success_rate < 50% 视为整体失败
    if args.dry_run:
        return 0
    bad = [k for k, s in summary.items() if s["success_rate"] < 0.5]
    if bad:
        print(f"[!!] 成功率 < 50% 的组: {bad}")
        return 2
    return 0


def parse_args() -> argparse.Namespace:
    p = argparse.ArgumentParser(description="米醋 MCP 性能基线测试")
    p.add_argument("--full", action="store_true", help="跑完整 sweep（默认 smoke）")
    p.add_argument("--repeat", type=int, default=1, help="每组重复次数")
    p.add_argument("--channels", choices=["image2", "grok", "all"], default="all")
    p.add_argument("--out-dir", default=str(Path(__file__).parent / "reports"))
    p.add_argument("--dry-run", action="store_true",
                   help="不打真实 API，只跑入口校验链路（看脚本本身能不能跑通）")
    return p.parse_args()


def main() -> None:
    args = parse_args()
    rc = asyncio.run(main_async(args))
    sys.exit(rc)


if __name__ == "__main__":
    main()
