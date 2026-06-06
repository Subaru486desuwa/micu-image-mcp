"""压测 / 性能测试共用工具。

设计：直接 in-process import server，async 调 image_generate / image_edit。
不走 stdio MCP，避免子进程开销污染时延样本。
"""
from __future__ import annotations

import json
import os
import statistics
import sys
import time
import re
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any


_SIZE_RE = re.compile(r"^(\d+)\s*[x×]\s*(\d+)$")


def parse_actual_size(v: Any) -> tuple[int, int] | None:
    """server.py 历史上把 saved[i].actual_size 存成 'WxH' 字符串；也兼容 [w,h] / (w,h)。"""
    if v is None:
        return None
    if isinstance(v, (list, tuple)) and len(v) == 2:
        try:
            return (int(v[0]), int(v[1]))
        except (TypeError, ValueError):
            return None
    if isinstance(v, str):
        m = _SIZE_RE.match(v.strip().lower())
        if m:
            return (int(m.group(1)), int(m.group(2)))
    return None


REPO_ROOT = Path(__file__).resolve().parent.parent
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))


def ensure_save_dir(label: str) -> Path:
    """统一把测试图扔到 /tmp/micu-bench-<label>，避免污染用户 ~/Pictures。"""
    root = Path(os.environ.get("MICU_BENCH_DIR", "/tmp/micu-bench")) / label
    root.mkdir(parents=True, exist_ok=True)
    # server.py 启动时把 MICU_SAVE_DIR_ROOT 当沙箱根，必须在 import server 之前设
    os.environ["MICU_SAVE_DIR_ROOT"] = str(root)
    os.environ["MICU_SAVE_DIR"] = str(root)
    return root


def import_server():
    """import server 之后再调；ensure_save_dir 必须先跑。"""
    import server  # type: ignore[import-not-found]
    return server


@dataclass
class Trial:
    label: str            # 任一组合标识："image2|2048x2048" / "grok|1024x1024"
    model: str
    size: str
    ok: bool
    wall_ms: float
    saved_count: int = 0
    actual_size: tuple[int, int] | None = None
    actual_megapixels: float | None = None
    notes: list[str] = field(default_factory=list)
    error: str | None = None
    extra: dict[str, Any] = field(default_factory=dict)

    def to_jsonable(self) -> dict[str, Any]:
        d = asdict(self)
        if d.get("actual_size") is not None:
            d["actual_size"] = list(d["actual_size"])
        return d


def percentile(values: list[float], pct: float) -> float | None:
    if not values:
        return None
    s = sorted(values)
    if len(s) == 1:
        return s[0]
    # 简单线性插值，足够 N=10-50 的小样本
    k = (len(s) - 1) * pct / 100.0
    lo = int(k)
    hi = min(lo + 1, len(s) - 1)
    frac = k - lo
    return s[lo] + (s[hi] - s[lo]) * frac


def summarize(trials: list[Trial], group_key=lambda t: t.label) -> dict[str, dict[str, Any]]:
    groups: dict[str, list[Trial]] = {}
    for t in trials:
        groups.setdefault(group_key(t), []).append(t)
    out: dict[str, dict[str, Any]] = {}
    for k, ts in groups.items():
        oks = [t for t in ts if t.ok]
        latencies = [t.wall_ms for t in oks]
        out[k] = {
            "n": len(ts),
            "ok": len(oks),
            "fail": len(ts) - len(oks),
            "success_rate": round(len(oks) / len(ts), 3) if ts else 0.0,
            "p50_ms": round(percentile(latencies, 50) or 0, 1) if latencies else None,
            "p95_ms": round(percentile(latencies, 95) or 0, 1) if latencies else None,
            "mean_ms": round(statistics.fmean(latencies), 1) if latencies else None,
            "min_ms": round(min(latencies), 1) if latencies else None,
            "max_ms": round(max(latencies), 1) if latencies else None,
            "actual_size_match": sum(
                1 for t in oks
                if t.actual_size is not None
                and f"{t.actual_size[0]}x{t.actual_size[1]}" == t.size
            ),
            "actual_megapixels_avg": round(
                statistics.fmean([t.actual_megapixels for t in oks if t.actual_megapixels]),
                2,
            ) if any(t.actual_megapixels for t in oks) else None,
            "errors": [t.error for t in ts if not t.ok and t.error],
        }
    return out


def write_report(
    title: str,
    summary: dict[str, dict[str, Any]],
    raw_trials: list[Trial],
    out_dir: Path,
    meta: dict[str, Any] | None = None,
) -> tuple[Path, Path]:
    out_dir.mkdir(parents=True, exist_ok=True)
    ts = time.strftime("%Y%m%d_%H%M%S")
    json_path = out_dir / f"{title}_{ts}.json"
    md_path = out_dir / f"{title}_{ts}.md"

    blob = {
        "title": title,
        "timestamp": ts,
        "meta": meta or {},
        "summary": summary,
        "trials": [t.to_jsonable() for t in raw_trials],
    }
    json_path.write_text(json.dumps(blob, indent=2, ensure_ascii=False), encoding="utf-8")

    lines: list[str] = []
    lines.append(f"# {title} — {ts}\n")
    if meta:
        lines.append("## Meta\n")
        for k, v in meta.items():
            lines.append(f"- **{k}**: `{v}`")
        lines.append("")
    lines.append("## Summary\n")
    headers = ["group", "n", "ok", "fail", "rate", "p50_ms", "p95_ms", "mean_ms", "actual_match"]
    lines.append("| " + " | ".join(headers) + " |")
    lines.append("|" + "|".join(["---"] * len(headers)) + "|")
    for group, s in summary.items():
        lines.append(
            "| " + " | ".join([
                f"`{group}`",
                str(s["n"]),
                str(s["ok"]),
                str(s["fail"]),
                f"{s['success_rate']:.0%}",
                str(s["p50_ms"]),
                str(s["p95_ms"]),
                str(s["mean_ms"]),
                f"{s['actual_size_match']}/{s['ok']}" if s["ok"] else "0/0",
            ]) + " |"
        )
    lines.append("")
    fail_groups = {k: v for k, v in summary.items() if v["fail"]}
    if fail_groups:
        lines.append("## Failures\n")
        for g, s in fail_groups.items():
            lines.append(f"### `{g}` — {s['fail']}/{s['n']} 失败")
            for err in s["errors"][:5]:
                lines.append(f"- `{err[:240]}`")
            lines.append("")
    md_path.write_text("\n".join(lines), encoding="utf-8")
    return json_path, md_path


def has_image2_key() -> bool:
    return bool(os.environ.get("MICU_API_KEY", "").strip())


def has_grok_key() -> bool:
    return bool(
        os.environ.get("MICU_GROK_API_KEY", "").strip()
        or os.environ.get("XAI_API_KEY", "").strip()
        or os.environ.get("GROK_API_KEY", "").strip()
    )
