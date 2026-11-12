#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# ///
"""Builds summary.md from an app-bench results directory (Linux or macOS runner).

Each cell is the median over runs with the [min-max] spread; the ratio is
Tauri / GPUI, so above 1 means Tauri uses more (or takes longer).

Usage: summarize.py <results-dir>
"""

from __future__ import annotations

import json
import statistics
import sys
from pathlib import Path

SCENARIOS = [
    ("startup-cold", "Startup, cold (fresh app state, no helper daemon)"),
    ("startup-warm", "Startup, warm (seeded state, daemon and tmux running)"),
    ("idle-1x1", "Idle, 1 workspace x 1 terminal"),
    ("idle-8x2", "Idle, 8 tabs x 2 panes"),
    ("heavy-seq", "Heavy output: seq 1 3000000"),
    ("heavy-cat", "Heavy output: cat of 200k colored lines"),
    ("stream", "Agent-like line stream (60 lines/s, 8x2 layout)"),
    ("agents-replay", "Recorded agents: 3 Claude + 3 Codex replays, rows working/done"),
    ("spinner-visible", "6 agents working, no output, window visible"),
    ("spinner-hidden", "6 agents working, no output, window on another workspace"),
    ("editor", "Editor: Quick Open and open a 5 MB file (no scrolling)"),
    ("explorer", "Explorer: expand a 10,000-entry directory, 300 x Down"),
    ("typing", "Typing latency (keystroke to pixels)"),
    ("compositor-baseline", "Compositor alone, no app window"),
]

# (label, path into the result, unit, decimals)
METRICS = {
    "startup": [
        ("Window mapped", ("metrics", "window_map_ms"), "ms", 0),
        ("First terminal content", ("metrics", "first_content_ms"), "ms", 0),
        ("CPU during startup window", ("cpu_s", "app"), "CPU-s", 2),
        ("App PSS (max)", ("pss_mb", "app", "max"), "MB", 0),
    ],
    "steady": [
        ("App CPU", ("cpu_pct", "app"), "% core", 1),
        ("App GPU (all engines)", ("gpu_pct", "app"), "% busy", 1),
        ("Local host helper CPU", ("cpu_pct", "host"), "% core", 1),
        ("tmux CPU", ("cpu_pct", "tmux"), "% core", 1),
        ("ssh client CPU", ("cpu_pct", "ssh"), "% core", 1),
        ("Compositor CPU", ("cpu_pct", "compositor"), "% core", 1),
        ("Compositor GPU", ("gpu_pct", "compositor"), "% busy", 1),
        ("App PSS (mean)", ("pss_mb", "app", "mean"), "MB", 0),
        ("App RSS (mean)", ("rss_mb", "app", "mean"), "MB", 0),
        ("Host helper PSS (mean)", ("pss_mb", "host", "mean"), "MB", 0),
        ("Remote helper CPU", ("remote", "cpu_pct", "host"), "% core", 1),
        ("Remote tmux CPU", ("remote", "cpu_pct", "tmux"), "% core", 1),
        ("Remote sshd CPU", ("remote", "cpu_pct", "sshd"), "% core", 1),
        ("Remote helper RSS (mean)", ("remote", "rss_mb", "host", "mean"), "MB", 0),
    ],
    "heavy": [
        ("Wall time to final line", ("metrics", "wall_s"), "s", 2),
        ("App CPU-seconds", ("cpu_s", "app"), "CPU-s", 2),
        ("Local host helper CPU-seconds", ("cpu_s", "host"), "CPU-s", 2),
        ("tmux CPU-seconds", ("cpu_s", "tmux"), "CPU-s", 2),
        ("ssh client CPU-seconds", ("cpu_s", "ssh"), "CPU-s", 2),
        ("Compositor CPU-seconds", ("cpu_s", "compositor"), "CPU-s", 2),
        ("App PSS (peak sample)", ("pss_mb", "app", "max"), "MB", 0),
        ("App RSS (peak sample)", ("rss_mb", "app", "max"), "MB", 0),
        ("Remote helper CPU-seconds", ("remote", "cpu_s", "host"), "CPU-s", 2),
        ("Remote tmux CPU-seconds", ("remote", "cpu_s", "tmux"), "CPU-s", 2),
    ],
    "action": [
        ("Action window", ("duration_s",), "s", 1),
        ("App CPU-seconds", ("cpu_s", "app"), "CPU-s", 2),
        ("App GPU (all engines)", ("gpu_pct", "app"), "% busy", 1),
        ("Local host helper CPU-seconds", ("cpu_s", "host"), "CPU-s", 2),
        ("Compositor CPU-seconds", ("cpu_s", "compositor"), "CPU-s", 2),
        ("App PSS (peak sample)", ("pss_mb", "app", "max"), "MB", 0),
        ("App RSS (peak sample)", ("rss_mb", "app", "max"), "MB", 0),
        ("Remote helper CPU-seconds", ("remote", "cpu_s", "host"), "CPU-s", 2),
    ],
    "typing": [
        ("Median latency", ("metrics", "latency_ms_median"), "ms", 1),
        ("p95 latency", ("metrics", "latency_ms_p95"), "ms", 1),
        ("Min latency", ("metrics", "latency_ms_min"), "ms", 1),
        ("Missed keys", ("metrics", "missed"), "", 0),
    ],
}


def kind(scenario: str) -> str:
    if scenario.startswith("startup"):
        return "startup"
    if scenario.startswith("heavy"):
        return "heavy"
    if scenario == "typing":
        return "typing"
    if scenario in ("editor", "explorer"):
        return "action"
    return "steady"


def dig(data: dict, path: tuple) -> float | None:
    for key in path:
        if not isinstance(data, dict) or key not in data:
            return None
        data = data[key]
    return data if isinstance(data, (int, float)) else None


def cell(values: list[float], decimals: int) -> str:
    if not values:
        return "n/a"
    median = statistics.median(values)
    text = f"{median:.{decimals}f}"
    if len(values) > 1:
        text += f" [{min(values):.{decimals}f}-{max(values):.{decimals}f}]"
    if len(values) < 3:
        text += f" (n={len(values)})"
    return text


def ratio(tauri: list[float], gpui: list[float]) -> str:
    if not tauri or not gpui:
        return "n/a"
    a, b = statistics.median(tauri), statistics.median(gpui)
    if b == 0:
        return "n/a (GPUI 0)" if a else "="
    return f"{a / b:.2f}x"


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__.strip(), file=sys.stderr)
        return 2
    root = Path(sys.argv[1])
    meta = json.loads((root / "meta.json").read_text()) if (root / "meta.json").exists() else {}
    lines = ["# Muxflow desktop benchmark: Tauri vs GPUI", ""]
    if meta:
        lines += [
            f"Run started {meta.get('started')}; {meta.get('runs')} runs per cell; "
            f"steady windows {meta.get('window_s')} s after {meta.get('settle_s')} s settle; "
            f"compositor: {meta.get('compositor')}.",
            "",
        ]
    lines += [
        "Cells: median [min-max] over runs. Ratio = Tauri / GPUI (above 1: Tauri uses more or takes longer).",
        "CPU in % of one core; GPU as % engine-busy time from DRM fdinfo; memory summed over the process tree.",
        "",
    ]
    if (root / "notes.md").exists():  # run-specific caveats written by the operator
        lines += [(root / "notes.md").read_text().strip(), ""]
    if (root / "sizes.md").exists():
        lines += [(root / "sizes.md").read_text().strip(), ""]
    for host in ("local", "ssh"):
        host_dir = root / host
        if not host_dir.exists():
            continue
        lines += [f"## Host: {host}", ""]
        for scenario, title in SCENARIOS:
            scenario_dir = host_dir / scenario
            if not scenario_dir.exists():
                continue
            results: dict[str, list[dict]] = {}
            for path in sorted(scenario_dir.glob("*-run*.json")):
                data = json.loads(path.read_text())
                results.setdefault(data["app"], []).append(data)
            lines += [f"### {title}", ""]
            if scenario == "compositor-baseline":
                data = results.get("none", [{}])[0]
                lines += [f"Compositor CPU {data.get('cpu_pct', {}).get('compositor', 'n/a')} % core, "
                          f"GPU {data.get('gpu_pct', {}).get('compositor', 'n/a')} % busy over "
                          f"{data.get('duration_s', 'n/a')} s.", ""]
                continue
            lines += ["| Metric | Tauri | GPUI | Tauri/GPUI |", "|---|---|---|---|"]
            for label, path, unit, decimals in METRICS[kind(scenario)]:
                tauri = [v for v in (dig(r, path) for r in results.get("tauri", [])) if v is not None]
                gpui = [v for v in (dig(r, path) for r in results.get("gpui", [])) if v is not None]
                if not tauri and not gpui:
                    continue
                name = f"{label} ({unit})" if unit else label
                lines.append(f"| {name} | {cell(tauri, decimals)} | {cell(gpui, decimals)} | {ratio(tauri, gpui)} |")
            errors = [f"{r['app']} run {r['run']}: {r['metrics']['error']}"
                      for runs in results.values() for r in runs if r.get("metrics", {}).get("error")]
            if errors:
                lines += ["", "Errors: " + "; ".join(errors)]
            lines.append("")
    (root / "summary.md").write_text("\n".join(lines) + "\n")
    print(root / "summary.md")
    return 0


if __name__ == "__main__":
    sys.exit(main())
