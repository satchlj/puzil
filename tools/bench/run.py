#!/usr/bin/env python3
"""Benchmarks Puzil offline and compares against a baseline.

    tools/bench/run.py                 5 app runs + the headless benchmark; compare to baseline
    tools/bench/run.py --runs 9        more runs, tighter medians
    tools/bench/run.py --save-baseline make this result the baseline
    tools/bench/run.py --no-build      measure the binaries already built
    tools/bench/run.py --app PATH      measure another build of the app (A/B comparisons)

Nothing here can send anything: the app runs in benchmark mode (PUZIL_BENCH), which turns
the network off before any HTTP client exists and only reads its cache; the headless
benchmark builds no network client. Every run works on a fresh copy of the data folder
(PUZIL_DATA_DIR, default reference/data), so the real one is never written.

Results land in reference/bench/ (gitignored: they're specific to this machine and data).
"""

import argparse, json, os, shutil, statistics, subprocess, sys, tempfile, time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "reference" / "bench"

# (key, label, unit, target, lower_is_better). Targets from docs/PLAN.md §7: cached UI in
# 100 ms, 130 MB of physical footprint at rest after browsing at the benchmark's 1280×860 window,
# 120 fps scrolling (8.3 ms of work per frame).
HEADLINE = [
    ("first_frame_ms", "Launch to first frame", "ms", 100, True),
    ("launch_app_built_ms", "  · macOS app created", "ms", None, True),
    ("launch_gpuikit_init_done_ms", "  · UI toolkit ready", "ms", None, True),
    ("launch_window_opened_ms", "  · window open", "ms", None, True),
    ("idle_footprint_mb", "Memory at launch, at rest (footprint)", "MB", 130, True),
    ("final_footprint_mb", "Memory while browsing (footprint)", "MB", None, True),
    ("settled_footprint_mb", "Memory after browsing, at rest", "MB", 130, True),
    ("final_rss_mb", "  · RSS after browsing", "MB", None, True),
    ("scroll_frame_work_ms.p50", "Scroll frame work, median", "ms", None, True),
    ("scroll_frame_work_ms.p95", "Scroll frame work, p95", "ms", 8.33, True),
    ("scroll_frame_work_ms.p99", "Scroll frame work, p99", "ms", 8.33, True),
    ("open_frame_work_ms.p95", "Open-conversation frame work, p95", "ms", None, True),
    ("open_ms_p50", "Open conversation to painted (vsync-bound)", "ms", None, True),
    ("reopen_frame_work_ms.p95", "Back to a conversation, frame work p95", "ms", None, True),
    ("conversation_render_us.p95", "Conversation render(), p95", "µs", None, True),
    ("cached_model_ms", "Cached model load (in app)", "ms", None, True),
    ("cpu_seconds", "CPU time for the whole run", "s", None, True),
    ("cli.load_state_ms", "Cached model load", "ms", None, True),
    ("cli.open_conv_query_ms", "Open-conversation store query", "ms", None, True),
    ("cli.search_us_median", "Search, median query", "µs", None, True),
    ("cli.search_us_max", "Search, slowest query", "µs", None, True),
    ("cli.render_ast_us_per_msg", "Parse message HTML", "µs/msg", None, True),
    ("cli.render_index_us_per_msg", "Build text index", "µs/msg", None, True),
    ("cli.math_us_per_formula", "Typeset a formula (cold)", "µs", None, True),
    ("cli.replay_us_per_event", "Apply an event", "µs", None, True),
]
NOISE = 0.05  # changes under 5%, or within the runs' spread, are reported as "same"


def get(d, key):
    for k in key.split("."):
        if not isinstance(d, dict) or k not in d:
            return None
        d = d[k]
    return d if isinstance(d, (int, float)) else None


def fresh_copy(src: Path) -> Path:
    tmp = Path(tempfile.mkdtemp(prefix="puzil-bench-"))
    shutil.copytree(src, tmp / "data", ignore=shutil.ignore_patterns("logs"))
    # APFS copies are clones whose pages aren't in the disk cache yet. Read them once, so the
    # app sees its cache warm, as it usually is (it's in use all day). A cold read is ~10x
    # slower and makes opening a conversation take an extra frame.
    for f in (tmp / "data").glob("*.sqlite*"):
        f.read_bytes()
    return tmp


APP = ROOT / "target/dist/puzil-app"


def run_app(data: Path, env_extra) -> dict:
    tmp = fresh_copy(data)
    out = tmp / "out.json"
    env = dict(os.environ, PUZIL_BENCH=str(out), PUZIL_DATA_DIR=str(tmp / "data"), PUZIL_DEV_SECRETS="file", **env_extra)
    try:
        subprocess.run([str(APP)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=180, check=False)
        return json.loads(out.read_text())
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def run_cli(data: Path) -> dict:
    tmp = fresh_copy(data)
    args = [str(ROOT / "target/dist/puzil"), "bench"]
    corpus, rec = ROOT / "reference/corpus.jsonl", ROOT / "reference/rec-selfdm.jsonl"
    # Empty means "the default": the store's own messages, and no replay.
    args += [str(corpus) if corpus.exists() else "", str(rec) if rec.exists() else ""]
    try:
        r = subprocess.run(args, env=dict(os.environ, PUZIL_DATA_DIR=str(tmp / "data")), capture_output=True, text=True, timeout=600)
        if r.returncode != 0:
            sys.exit(f"headless benchmark failed:\n{r.stderr}")
        return json.loads(r.stdout)
    finally:
        shutil.rmtree(tmp, ignore_errors=True)


def median_of(runs, key):
    vals = [v for v in (get(r, key) for r in runs) if v is not None]
    return (statistics.median(vals), (max(vals) - min(vals)) if len(vals) > 1 else 0.0) if vals else (None, None)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--runs", type=int, default=7)
    ap.add_argument("--warmup", type=int, default=1, help="app runs to discard first (cold disk cache)")
    ap.add_argument("--data", default=os.environ.get("PUZIL_DATA_DIR", str(ROOT / "reference/data")))
    ap.add_argument("--save-baseline", action="store_true")
    ap.add_argument("--no-build", action="store_true")
    ap.add_argument("--app", help="the app binary to measure (default: this checkout's release build)")
    a = ap.parse_args()
    data = Path(a.data)
    global APP
    if a.app:
        APP = Path(a.app).resolve()
    if not a.no_build:
        subprocess.run(["cargo", "build", "--profile", "dist", "-q", "-p", "puzil-app", "-p", "puzil-cli"], cwd=ROOT, check=True)
    # Same look every run: theme and mode affect what's drawn.
    look = {"PUZIL_THEME": "dark", "PUZIL_PALETTE": "neutral"}
    for i in range(a.warmup):
        print(f"warm-up run {i + 1}/{a.warmup}…", file=sys.stderr, flush=True)
        run_app(data, look)
    runs = []
    for i in range(a.runs):
        print(f"app run {i + 1}/{a.runs}…", file=sys.stderr, flush=True)
        runs.append(run_app(data, look))
    print("headless benchmark…", file=sys.stderr, flush=True)
    cli = run_cli(data)
    for r in runs:
        r["cli"] = cli

    # The same build runs 2-3x slower in some runs: the main thread got an efficiency core or
    # a low clock because of whatever else the machine was doing. cpu_probe_us times a fixed
    # computation on the main thread in each run. Timings come from the runs whose probe is
    # within 15% of the fastest, so builds compare at full speed; memory uses every run.
    probes = {i: get(r, "cpu_probe_us.p50") for i, r in enumerate(runs)}
    fastest = min((p for p in probes.values() if p), default=None)
    full_speed = [r for i, r in enumerate(runs) if fastest is None or (probes[i] or 0) <= fastest * 1.15] or runs
    result = {"when": time.strftime("%Y-%m-%dT%H:%M:%S"), "commit": subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip(), "runs": a.runs, "full_speed_runs": len(full_speed), "metrics": {}}
    for key, label, unit, *_ in HEADLINE:
        timing = unit in ("ms", "µs", "µs/msg") and not key.startswith("cli.")
        m, spread = median_of(full_speed if timing else runs, key)
        if m is not None:
            result["metrics"][key] = {"median": m, "spread": spread}
    result["raw"] = runs

    OUT.mkdir(parents=True, exist_ok=True)
    stamp = time.strftime("%Y%m%d-%H%M%S")
    (OUT / f"{stamp}.json").write_text(json.dumps(result, indent=2))
    base_path = OUT / "baseline.json"
    base = json.loads(base_path.read_text())["metrics"] if base_path.exists() else {}

    print(f"\nPuzil benchmark · {result['commit']} · {a.runs} runs ({len(full_speed)} at full speed, used for timings) · medians (spread)\n")
    print(f"{'':44} {'now':>14} {'baseline':>10} {'change':>9}  target")
    for key, label, unit, target, lower in HEADLINE:
        m = result["metrics"].get(key)
        if not m:
            continue
        now = f"{m['median']:.2f} {unit}"
        spread = f"±{m['spread'] / 2:.2f}" if m["spread"] else ""
        b = base.get(key, {}).get("median")
        change, mark = "", ""
        if b:
            d = (m["median"] - b) / b
            better = d < 0 if lower else d > 0
            # Half of each result's spread is how far its median could move by chance.
            noise = max(NOISE, (m["spread"] + base[key].get("spread", 0)) / 2 / b)
            mark = " same" if abs(d) < noise else (" better" if better else " WORSE")
            change = f"{d * 100:+.0f}%"
        tgt = ""
        if target is not None:
            ok = m["median"] <= target if lower else m["median"] >= target
            tgt = f"{'✓' if ok else '✗'} {target:g}"
        print(f"{label:44} {now:>14} {f'{b:.2f}' if b else '—':>10} {change:>6}{mark:<7} {tgt}  {spread}")
    if a.save_baseline:
        shutil.copy(OUT / f"{stamp}.json", base_path)
        print(f"\nsaved as the baseline ({base_path.relative_to(ROOT)})")
    print(f"\nfull result: {(OUT / f'{stamp}.json').relative_to(ROOT)}")


if __name__ == "__main__":
    main()
