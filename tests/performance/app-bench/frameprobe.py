#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy>=1.26"]
# ///
"""Frame rate and per-frame cost of the GPUI app with 6 working agents, no output.

Runs on the private nested compositor only. For each variant (`spinner`: the
working spinners; `glyphs`: the accessibility option's static state dots, the
same scene with no motion), one launch measures CPU per thread and GPU busy
time, and a second launch with WAYLAND_DEBUG=client counts wl_surface commits,
one per presented frame. Each launch is measured visible, on another
workspace, and shown again. Local host only.

  frameprobe.py --root <scratch> --target-dir <dir with muxflow-gpui and muxflow-host> \
                --results <dir> [--variants spinner,glyphs] [--seconds 20]

The scratch root needs `data/agents` from a bench_linux.py run with --recordings.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import benchlib as bl  # noqa: E402
import bench_linux as b  # noqa: E402

TICK = os.sysconf("SC_CLK_TCK")


def threads(pid: int) -> dict[str, int]:
    out: dict[str, int] = {}
    for tid in os.listdir(f"/proc/{pid}/task"):
        try:
            raw = open(f"/proc/{pid}/task/{tid}/stat", "rb").read()
        except OSError:
            continue
        comm = raw[raw.find(b"(") + 1 : raw.rfind(b")")].decode()
        rest = raw[raw.rfind(b")") + 2 :].split()
        out[f"{comm}:{tid}"] = int(rest[11]) + int(rest[12])
    return out


def gpu_ns(pid: int) -> int:
    return sum(v for c in bl.drm_clients(pid).values() for k, v in c.items() if k.startswith("engine:"))


def measure(pid: int, seconds: float) -> dict:
    t0, g0, th0 = time.monotonic(), gpu_ns(pid), threads(pid)
    time.sleep(seconds)
    t1, g1, th1 = time.monotonic(), gpu_ns(pid), threads(pid)
    wall = t1 - t0
    per = {}
    for key, ticks in th1.items():
        delta = ticks - th0.get(key, 0)
        if delta:
            name = key.split(":")[0]
            per[name] = per.get(name, 0) + delta
    total = sum(per.values())
    return {
        "wall_s": round(wall, 2),
        "cpu_pct": round(100 * total / TICK / wall, 2),
        "gpu_pct": round(100 * (g1 - g0) / 1e9 / wall, 2),
        "threads_pct": {k: round(100 * v / TICK / wall, 2) for k, v in sorted(per.items(), key=lambda x: -x[1])},
    }


def commits(log: Path, start: int, end: int, wall: float) -> dict:
    data = log.read_bytes()[start:end].decode(errors="replace")
    surfaces = re.findall(r"-> wl_surface#(\d+)\.commit\(\)", data)
    per_surface: dict[str, int] = {}
    for s in surfaces:
        per_surface[s] = per_surface.get(s, 0) + 1
    return {"commits_per_s": round(len(surfaces) / wall, 1),
            "per_surface_per_s": {k: round(v / wall, 1) for k, v in per_surface.items()}}


def run(ctx: b.Context, variant: str, glyphs: bool, debug: bool, seconds: float) -> dict:
    ctx.reset("agents", seeded=True)
    if glyphs:
        path = ctx.home / ".config" / "dev.muxflow.desktop" / "app-state.json"
        state = json.loads(path.read_text())
        state["shell"]["agentStateGlyphs"] = True
        path.write_text(json.dumps(state))
    b.install_local_hooks(ctx)
    base_env = ctx.app_env
    log = ctx.results / "logs" / f"probe-{variant}-{'debug' if debug else 'cpu'}.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    log.write_text("")
    if debug:
        ctx.app_env = lambda: dict(base_env(), WAYLAND_DEBUG="client")
    t0 = ctx.launch("gpui")
    ctx.app_env = base_env
    pid = ctx.app_proc.pid
    real_log = ctx.results / "logs" / f"gpui-{ctx.host}.log"
    ctx.wait_content(t0)
    panes = b.start_agents(ctx, "idle")
    time.sleep(2.0)
    for index, pane in enumerate(panes):
        b.agent_hook(ctx, pane, index, "UserPromptSubmit")
    time.sleep(ctx.settle)
    ctx.compositor.save_png(ctx.results / f"probe-{variant}.png")
    result = {"variant": variant, "glyphs": glyphs}
    for phase in ("visible", "hidden", "reshown"):
        if phase == "hidden":
            ctx.compositor.show_workspace(2)
            time.sleep(3.0)
        if phase == "reshown":
            ctx.compositor.show_workspace(1)
            time.sleep(1.0)
        if debug:
            start = real_log.stat().st_size
            t = time.monotonic()
            time.sleep(seconds)
            wall = time.monotonic() - t
            result[phase] = commits(real_log, start, real_log.stat().st_size, wall)
        else:
            result[phase] = measure(pid, seconds)
    ctx.compositor.show_workspace(1)
    ctx.stop_app()
    if debug:
        real_log.rename(log)
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--root", required=True)
    parser.add_argument("--target-dir", required=True)
    parser.add_argument("--results", required=True)
    parser.add_argument("--variants", default="spinner,glyphs")
    parser.add_argument("--seconds", type=float, default=20.0)
    parser.add_argument("--settle", type=float, default=10.0)
    args = parser.parse_args()
    ns = argparse.Namespace(root=args.root, repo=str(HERE.parents[2]), results=args.results,
                            ssh_config=None, ssh_target="x", compositor="private", settle=args.settle,
                            window=args.seconds, typing_keys=0, seq=0)
    ctx = b.Context(ns)
    ctx.target_dir = Path(args.target_dir)
    ctx.host_binary = ctx.target_dir / "muxflow-host"
    Path(args.results).mkdir(parents=True, exist_ok=True)
    out = []
    ctx.compositor.start()
    try:
        for variant in args.variants.split(","):
            for debug in (False, True):
                r = run(ctx, variant, variant == "glyphs", debug, args.seconds)
                print(json.dumps(r), flush=True)
                out.append(r)
    finally:
        ctx.stop_app()
        ctx.stop_host_side()
        ctx.compositor.stop()
    (Path(args.results) / "frameprobe.json").write_text(json.dumps(out, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
