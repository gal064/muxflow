#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy>=1.26"]
# ///
"""Agent-switch latency, GPUI, Linux/Wayland: click an agent row in the
sidebar, time until the target is on screen.

Runs in the same private nested Hyprland as bench_linux.py, with its Context,
tmux isolation and helper. See README.md, "Agent switch".

Layout (one tmux server, on the local host or the SSH host):

    workspace w1, tab 1:  a1 (claude, cyan)  | a2 (codex, yellow)
    workspace w1, tab 2:  a3 (claude, orange)
    workspace w2, tab 1:  b1 (codex, violet)

Each pane runs workloads/marker-agent.pl under the agent's name, so the
helper lists it as that agent. Cases, each switch timed from the click's
button release (workloads/vpointer.c) to the first compositor capture that
shows it:

    same-tab         a1 <-> a2   the target's cursor turns filled (focused)
    other-tab        a1 <-> a3   the target's colour block is on screen
    other-workspace  a1 <-> b1   the target's colour block is on screen

Every switch also keeps `focused` (the target's cursor drawn filled) and `row`
(the clicked row drawn selected). All three come from pixels only, so any
build is measured the same way.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shlex
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import bench_linux as bench  # noqa: E402
import benchlib as bl  # noqa: E402

log = bench.log
WINDOW = bench.WINDOW
OUTPUT_SIZE = (1920, 1080)

# key: (agent name, colour)
TARGETS = {
    "a1": ("claude", (0, 255, 255)),
    "a2": ("codex", (255, 255, 0)),
    "a3": ("claude", (255, 136, 0)),
    "b1": ("codex", (136, 0, 255)),
}
CASES = {"same-tab": "a2", "other-tab": "a3", "other-workspace": "b1"}
HOME = "a1"
MARK_COLS, MARK_ROWS = 24, 5
CURSOR_ROW_OFFSET = MARK_ROWS + 2  # marker-agent.pl parks the cursor two rows below the block
# The sidebar at the bench's 1600x1000 window, in window coordinates: agent
# rows are the lines below the Agents header with text in both the title
# column and the right-hand workspace column.
SIDEBAR_X = (8, 232)
TITLE_COLUMN = (36, 70)
WORKSPACE_COLUMN = (198, 226)


def mask_of(image: np.ndarray, rgb: tuple[int, int, int], tolerance: int = 14) -> np.ndarray:
    diff = np.abs(image.astype(np.int16) - np.array(rgb, dtype=np.int16))
    return np.all(diff <= tolerance, axis=-1)


class Pointer:
    """workloads/vpointer.c on the private compositor only."""

    def __init__(self, binary: Path, compositor: bench.Compositor):
        if compositor.mode != "private" or compositor.wayland_display == os.environ.get("WAYLAND_DISPLAY"):
            raise RuntimeError("refusing to drive a pointer outside the private compositor")
        env = dict(os.environ, WAYLAND_DISPLAY=compositor.wayland_display)
        self.proc = subprocess.Popen([str(binary), str(OUTPUT_SIZE[0]), str(OUTPUT_SIZE[1])], env=env,
                                     stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True, bufsize=1)

    def _do(self, verb: str, x: int, y: int) -> float:
        self.proc.stdin.write(f"{verb} {x} {y}\n")
        self.proc.stdin.flush()
        answer = self.proc.stdout.readline().split()
        if not answer or answer[0] != "ok":
            raise RuntimeError(f"vpointer: {answer}")
        return float(answer[1])

    def move(self, x: int, y: int) -> float:
        return self._do("move", x + WINDOW[0], y + WINDOW[1])

    def click(self, x: int, y: int) -> float:
        """Clicks at window coordinates; returns the release time (monotonic)."""
        return self._do("click", x + WINDOW[0], y + WINDOW[1])

    def close(self) -> None:
        try:
            self.proc.stdin.write("quit\n")
            self.proc.stdin.flush()
            self.proc.wait(3)
        except (OSError, subprocess.TimeoutExpired):
            self.proc.kill()


def layout_script(tmux: str, cwd: str, marker: str) -> str:
    """Builds the switch layout on the bench tmux server (`tmux` must name its
    socket) and starts the marker agents; prints `key pane_id` lines."""
    if " -L " not in f" {tmux} " and " -S " not in f" {tmux} ":
        raise ValueError("the layout's tmux must name its socket")
    lines = [
        "set -e",
        f"a1=$({tmux} new-session -d -s w1 -c {cwd} -x 200 -y 50 -P -F '#{{pane_id}}')",
        f"a2=$({tmux} split-window -d -h -t \"$a1\" -c {cwd} -P -F '#{{pane_id}}')",
        f"a3=$({tmux} new-window -d -t w1 -c {cwd} -P -F '#{{pane_id}}')",
        f"b1=$({tmux} new-session -d -s w2 -c {cwd} -x 200 -y 50 -P -F '#{{pane_id}}')",
    ]
    for key, (name, rgb) in TARGETS.items():
        hex_colour = "".join(f"{c:02x}" for c in rgb)
        command = f"exec -a {name} perl {marker} {hex_colour} {MARK_COLS} {MARK_ROWS}"
        lines.append(f"{tmux} respawn-pane -k -t \"${key}\" {shlex.quote(command)}")
        lines.append(f"echo {key} ${key}")
    return "\n".join(lines) + "\n"


class Screen:
    """What one capture says about the targets."""

    def __init__(self, ctx: bench.Context):
        self.ctx = ctx
        self.marker_px: dict[str, int] = {}
        self.cursor_cells: dict[str, tuple[int, int, int, int]] = {}
        self.term_bg: np.ndarray | None = None
        self.accent: np.ndarray | None = None
        self.rows: dict[str, int] = {}  # target -> row centre y (window coordinates)
        self.row_probe_x = 14  # inside the row's left padding, left of its glyph
        self.grab_ms: list[float] = []

    def grab(self) -> tuple[np.ndarray, float]:
        start = time.monotonic()
        image = self.ctx.compositor.grab()
        stamp = time.monotonic()
        self.grab_ms.append((stamp - start) * 1000)
        return image, stamp

    def count(self, image: np.ndarray, key: str) -> int:
        x0 = SIDEBAR_X[1]  # the terminal area only
        return int(np.count_nonzero(mask_of(image[:, x0:], TARGETS[key][1])))

    def visible(self, image: np.ndarray, key: str) -> bool:
        return key in self.marker_px and self.count(image, key) >= 0.6 * self.marker_px[key]

    def focused(self, image: np.ndarray, key: str) -> bool:
        """The target's cursor cell: filled (focused), empty (focused, in a
        blink's off phase: a pane bootstrapped from a capture may blink) or
        outlined (unfocused, drawn hollow). Needs the target on screen."""
        if key not in self.cursor_cells or self.term_bg is None or not self.visible(image, key):
            return False
        x, y, w, h = self.cursor_cells[key]
        cell = image[y : y + h, x : x + w]
        if cell.size == 0:
            return False
        differs = float(np.any(np.abs(cell.astype(np.int16) - self.term_bg) > 40, axis=-1).mean())
        return differs > 0.6 or differs < 0.08

    def row_selected(self, image: np.ndarray, key: str) -> bool:
        if self.accent is None or key not in self.rows:
            return False
        y, x = self.rows[key], self.row_probe_x
        patch = image[y - 1 : y + 2, x - 1 : x + 2].astype(np.int16)
        return bool(np.all(np.abs(patch - self.accent) <= 8))

    # calibration --------------------------------------------------------

    def calibrate_marker(self, image: np.ndarray, key: str) -> bool:
        x_off = SIDEBAR_X[1]
        mask = mask_of(image[:, x_off:], TARGETS[key][1])
        count = int(np.count_nonzero(mask))
        if count < 500:
            return False
        ys, xs = np.nonzero(mask)
        x0, x1, y0, y1 = int(xs.min()) + x_off, int(xs.max()) + x_off, int(ys.min()), int(ys.max())
        cell_w, cell_h = (x1 - x0 + 1) / MARK_COLS, (y1 - y0 + 1) / MARK_ROWS
        self.marker_px[key] = count
        self.cursor_cells[key] = (x0, int(round(y0 + CURSOR_ROW_OFFSET * cell_h)), int(round(cell_w)),
                                  int(round(cell_h)))
        if self.term_bg is None:
            self.term_bg = image[int(y0 + (CURSOR_ROW_OFFSET + 0.5) * cell_h), int(x0 + 4 * cell_w)].astype(np.int16)
        return True

    @staticmethod
    def agent_row_lines(image: np.ndarray) -> list[int]:
        """Centre y of each agent row in the sidebar's Agents section."""
        sidebar = image[:, SIDEBAR_X[0] : SIDEBAR_X[1]].astype(np.int16)
        bg = np.median(sidebar[:, -4:].reshape(-1, 3), axis=0)
        ink = np.any(np.abs(sidebar - bg) > 50, axis=-1)
        title = ink[:, TITLE_COLUMN[0] - SIDEBAR_X[0] : TITLE_COLUMN[1] - SIDEBAR_X[0]].any(axis=1)
        workspace = ink[:, WORKSPACE_COLUMN[0] - SIDEBAR_X[0] : WORKSPACE_COLUMN[1] - SIDEBAR_X[0]].any(axis=1)
        both = title & workspace
        # The Agents header line ("AGENTS  History  workspace") also has both;
        # the section starts below the sidebar's split handle, so keep lines in
        # the lower part only and drop the first (the header).
        lines, start = [], None
        for y in range(image.shape[0]):
            if both[y] and start is None:
                start = y
            elif not both[y] and start is not None:
                if y - start >= 6:
                    lines.append((start + y) // 2)
                start = None
        return lines


def wait_for(screen: Screen, checks: dict, t0: float, timeout: float) -> tuple[dict, int]:
    """Polls captures until every check is true; returns first-true times (ms after t0)."""
    seen: dict[str, float | None] = {name: None for name in checks}
    frames = 0
    deadline = t0 + timeout
    while time.monotonic() < deadline and any(v is None for v in seen.values()):
        image, stamp = screen.grab()
        frames += 1
        for name, check in checks.items():
            if seen[name] is None and check(image):
                seen[name] = round((stamp - t0) * 1000, 1)
    return seen, frames


def map_rows(ctx: bench.Context, screen: Screen, pointer: Pointer, settle: float, evidence: Path) -> None:
    """Clicks each agent row once and keeps which target it puts in focus."""
    image, _ = screen.grab()
    split = int(image.shape[0] * 0.45)
    lines = [y for y in Screen.agent_row_lines(image) if y > split]
    if lines:
        lines = lines[1:]  # the Agents header
    log(f"  agent row lines: {lines}")
    for _ in range(3):  # a row may not answer the first time (the one already selected)
        for y in [y for y in lines if y not in screen.rows.values()]:
            pointer.click(TITLE_COLUMN[0] + 20, y)
            time.sleep(settle / 2)  # long enough for any build at this link
            votes: dict[str, int] = {}
            end = time.monotonic() + settle / 2
            while time.monotonic() < end:
                image, _ = screen.grab()
                for key in TARGETS:
                    if key not in screen.marker_px:
                        screen.calibrate_marker(image, key)
                focused = [key for key in TARGETS if screen.focused(image, key)]
                if len(focused) == 1:
                    votes[focused[0]] = votes.get(focused[0], 0) + 1
                time.sleep(0.05)
            best = max(votes, key=votes.get) if votes else None
            if best and best not in screen.rows:
                screen.rows[best] = y
                if screen.accent is None:
                    screen.accent = image[y, screen.row_probe_x].astype(np.int16)
            else:
                log(f"  row at y={y}: focused votes {votes}")
            pointer.move(900, 980)
        if len(screen.rows) == len(TARGETS):
            break
    ctx.compositor.save_png(evidence / "switch-rows.png")
    log(f"  rows: {screen.rows}; accent {screen.accent}; term bg {screen.term_bg}; markers {screen.marker_px}")


def run_cases(ctx: bench.Context, screen: Screen, pointer: Pointer, switches: int, timeout: float,
              settle: float, seed: int) -> dict[str, list[dict]]:
    rng = random.Random(seed)
    results: dict[str, list[dict]] = {case: [] for case in CASES}

    def go(case: str, key: str) -> dict:
        painted = (lambda im: screen.focused(im, key)) if case == "same-tab" else (lambda im: screen.visible(im, key))
        checks = {"painted": painted, "focused": lambda im: screen.focused(im, key),
                  "row": lambda im: screen.row_selected(im, key)}
        t0 = pointer.click(TITLE_COLUMN[0] + 20, screen.rows[key])
        seen, frames = wait_for(screen, checks, t0, timeout)
        pointer.move(900, 980)
        return {"target": key, "frames": frames, **{f"{k}_ms": v for k, v in seen.items()}}

    for case, target in CASES.items():
        # Start from the home agent, settled.
        go(case, HOME)
        time.sleep(settle)
        for index in range(switches):
            key = target if index % 2 == 0 else HOME
            sample = go(case, key)
            sample["case"] = case
            results[case].append(sample)
            log(f"    {case} -> {key}: painted {sample['painted_ms']} ms, focused {sample['focused_ms']} ms, "
                f"row {sample['row_ms']} ms")
            time.sleep(settle + rng.uniform(0.2, 0.6))
    return results


def summarize(samples: list[dict], field: str) -> dict:
    values = [s[f"{field}_ms"] for s in samples if s.get(f"{field}_ms") is not None]
    return {
        f"{field}_ms_median": round(statistics.median(values), 1) if values else None,
        f"{field}_ms_p95": round(bl.percentile(values, 0.95), 1) if values else None,
        f"{field}_ms_min": round(min(values), 1) if values else None,
        f"{field}_ms_max": round(max(values), 1) if values else None,
        f"{field}_missed": sum(1 for s in samples if s.get(f"{field}_ms") is None),
    }


def one_run(ctx: bench.Context, args, label: str, run: int, vpointer: Path) -> None:
    log(f"{label} run {run}: switch block")
    out = ctx.results / label
    evidence = out / "evidence"
    evidence.mkdir(parents=True, exist_ok=True)
    ctx.reset(None, seeded=True)
    if ctx.host == "ssh":
        ctx.remote.check_isolated()
        script = layout_script(bench.RTMUX, "~", "~/bench/marker-agent.pl")
        listing = ctx.remote.ssh(script)
        ctx.remote.ssh("~/.local/bin/muxflow-host hook install --yes >/dev/null")
    else:
        script = layout_script(f"tmux -L {bench.TMUX_SOCKET}", shlex.quote(str(ctx.home / "work")),
                               shlex.quote(str(ctx.data / "marker-agent.pl")))
        listing = ctx.local.run(script)
        bench.install_local_hooks(ctx)
    log(f"  panes: {' '.join(listing.split())}")
    t0 = ctx.launch("gpui")
    screen = Screen(ctx)
    content, _ = wait_for(screen, {"a1": lambda im: int(np.count_nonzero(mask_of(im, TARGETS['a1'][1]))) > 2000},
                          t0, 90)
    log(f"  first marker on screen after {content['a1']} ms")
    time.sleep(args.settle)
    pointer = Pointer(vpointer, ctx.compositor)
    try:
        map_rows(ctx, screen, pointer, settle=max(3.0, 6 * args.rtt_hint), evidence=evidence)
        missing = [k for k in TARGETS if k not in screen.rows]
        if missing:
            ctx.compositor.save_png(evidence / f"switch-run{run}-rows-missing.png")
            raise RuntimeError(f"agent rows not found for {missing}")
        results = run_cases(ctx, screen, pointer, args.switches, args.timeout, args.between, 7000 + run)
        ctx.compositor.save_png(evidence / f"switch-run{run}-end.png")
    finally:
        pointer.close()
    grab_ms = statistics.median(screen.grab_ms) if screen.grab_ms else None
    for case, samples in results.items():
        metrics = {"switches": len(samples), "capture_interval_ms": round(grab_ms, 1) if grab_ms else None,
                   "link_one_way_ms": args.link_delay if ctx.host == "ssh" else None}
        for field in ("painted", "focused", "row"):
            metrics.update(summarize(samples, field))
        bench.save(ctx_for(ctx, label), f"switch-{case}", "gpui", run, {"metrics": metrics, "samples": samples})
    ctx.stop_app()


def ctx_for(ctx: bench.Context, label: str):
    """bench.save writes under ctx.host; the switch results go under the link label."""
    class View:
        results = ctx.results
        host = label
    return View()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", required=True, help="scratch root (HOME, runtime, tmux dirs live here)")
    parser.add_argument("--repo", default=str(HERE.parents[2]))
    parser.add_argument("--bin-dir", help="muxflow-gpui and muxflow-host (default: <repo>/target/release)")
    parser.add_argument("--results", required=True)
    parser.add_argument("--host", choices=("local", "ssh"), default="local")
    parser.add_argument("--label", help="results subdirectory (default: the host, or ssh-rtt<2xdelay>)")
    parser.add_argument("--link-delay", type=int, default=0,
                        help="ssh only: one-way delay in ms written to <loopback dir>/delay_ms")
    parser.add_argument("--loopback", help="loopback-ssh.sh dir (its ssh_config is used)")
    parser.add_argument("--ssh-target", default="muxflow-bench")
    parser.add_argument("--ssh-control-target", default="muxflow-bench-direct")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--switches", type=int, default=10, help="per case per run, alternating directions")
    parser.add_argument("--timeout", type=float, default=20.0)
    parser.add_argument("--settle", type=float, default=8.0, help="after launch")
    parser.add_argument("--between", type=float, default=1.0, help="pause after each switch completes")
    args = parser.parse_args()

    ssh_config = None
    if args.host == "ssh":
        if not args.loopback:
            raise SystemExit("--loopback <dir> is required for --host ssh (see loopback-ssh.sh)")
        loopback = Path(args.loopback).resolve()
        (loopback / "delay_ms").write_text(f"{args.link_delay}\n")
        ssh_config = str(loopback / "ssh_config")
    args.rtt_hint = 2 * args.link_delay / 1000
    label = args.label or (args.host if args.host == "local" or args.link_delay == 0 else f"ssh-rtt{2 * args.link_delay}")
    ns = argparse.Namespace(root=args.root, repo=args.repo, results=args.results, ssh_config=ssh_config,
                            ssh_target=args.ssh_target, ssh_control_target=args.ssh_control_target,
                            compositor="private", settle=args.settle, window=0, typing_keys=0, seq=0,
                            bin_dir=args.bin_dir)
    ctx = bench.Context(ns)
    ctx.host = args.host
    ctx.results.mkdir(parents=True, exist_ok=True)
    ctx.data.mkdir(parents=True, exist_ok=True)
    shutil.copy(HERE / "workloads" / "marker-agent.pl", ctx.data / "marker-agent.pl")
    vpointer = ctx.data / "vpointer"
    subprocess.run(["cc", "-O2", "-o", str(vpointer), str(HERE / "workloads" / "vpointer.c"), "-lwayland-client"],
                   check=True)
    meta_path = ctx.results / f"meta-switch-{label}.json"
    meta = {
        "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "label": label, "host": args.host,
        "link_one_way_ms": args.link_delay if args.host == "ssh" else None, "runs": args.runs,
        "switches": args.switches,
        "binaries": {name: subprocess.run(["sha256sum", str(ctx.target_dir / name)], capture_output=True,
                                          text=True).stdout.split()[0] for name in ("muxflow-gpui", "muxflow-host")},
        "loadavg_start": Path("/proc/loadavg").read_text().split()[:3],
        "io_pressure_start": Path("/proc/pressure/io").read_text().splitlines()[0],
    }

    def interrupted(*_):
        raise KeyboardInterrupt

    import signal
    signal.signal(signal.SIGTERM, interrupted)
    try:
        ctx.compositor.start()
        if args.host == "ssh":
            ctx.remote.prepare()
            ctx.remote.scp([HERE / "workloads" / "marker-agent.pl"], "bench/")
        for run in range(1, args.runs + 1):
            one_run(ctx, args, label, run, vpointer)
    finally:
        try:
            ctx.stop_app()
            host = ctx.host
            ctx.host = "local"
            ctx.stop_host_side()
            ctx.host = host
            bench.close_remote(ctx)  # logs, never raises
        finally:
            try:
                ctx.compositor.stop()
            finally:
                meta["loadavg_end"] = Path("/proc/loadavg").read_text().split()[:3]
                meta["io_pressure_end"] = Path("/proc/pressure/io").read_text().splitlines()[0]
                meta_path.write_text(json.dumps(meta, indent=2))
        log("stopped the app, its helper daemon, the bench tmux servers and the compositor")
    return 0


if __name__ == "__main__":
    sys.exit(main())
