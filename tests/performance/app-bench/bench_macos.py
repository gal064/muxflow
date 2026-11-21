#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy>=1.26", "pillow>=10"]
# ///
"""Tauri vs GPUI desktop benchmark, macOS runner. Same scenarios and result
layout as bench_linux.py, so summarize.py reads both. See README.md.

macOS differences, all reported in meta.json and the summary:
- CPU: `ps` cumulative CPU time per process, sampled each second. macOS has no
  cutime, so a process that exits mid-window loses its last interval.
- Tauri's WebKit runs as XPC services parented to launchd; they are attributed
  to the app by diffing com.apple.WebKit.* pids before and after launch, with
  Safari (and any other WebKit-using app you can close) closed.
- Memory: `footprint` (phys_footprint, the number Activity Monitor shows) every
  5 s, plus RSS from ps every tick. PSS does not exist on macOS.
- GPU: `powermetrics --show-process-gpu` when `sudo -n` works; otherwise "not measured".
- Window: AppleScript/System Events sets the frame; map time polls System
  Events (~50-100 ms resolution). Content detection uses `screencapture`
  (~150-300 ms per frame), so startup and heavy-output times are coarse.
- Typing latency needs the frame-accurate ScreenCaptureKit + CGEvent meter in
  tests/performance/runtime/macos (pixels.swift, built and granted once);
  without --meter it is reported as not measured.
- "Hidden" is a minimized window (Spaces cannot be switched from a script).
"""

from __future__ import annotations

import argparse
import io
import json
import os
import plistlib
import random
import re
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import benchlib as bl  # noqa: E402
from bench_linux import (  # noqa: E402  (pure helpers, no Linux calls at import)
    AGENT_NAMES, ADAPTERS, MAGENTA, PROMPT, SENTINEL, TMUX_SOCKET, count_color, files_fixture_script,
    has_green_bar, layout_script,
)

# 1600x1000 (Linux) does not fit a 1512x982-point laptop screen; same 16:10 shape at 0.9x.
WINDOW = (36, 50, 1440, 900)  # points
APPS = {"tauri": "Muxflow", "gpui": "muxflow-gpui"}


def log(message: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {message}", flush=True)


def osa(script: str) -> str:
    return subprocess.run(["osascript", "-e", script], capture_output=True, text=True).stdout.strip()


def ps_table() -> dict[int, tuple[int, float, int, str]]:
    """pid -> (ppid, cpu seconds, rss kB, command)."""
    out = subprocess.run(["ps", "-axo", "pid=,ppid=,time=,rss=,command="], capture_output=True, text=True).stdout
    table = {}
    for line in out.splitlines():
        parts = line.split(None, 4)
        if len(parts) < 5:
            continue
        pid, ppid, cpu, rss, command = parts
        seconds = 0.0
        for chunk in re.split(r"[:-]", cpu):
            seconds = seconds * 60 + float(chunk)
        if "-" in cpu:  # days-hh:mm:ss
            days, rest = cpu.split("-", 1)
            seconds = int(days) * 86400 + sum(float(x) * 60 ** i for i, x in enumerate(reversed(rest.split(":"))))
        table[int(pid)] = (int(ppid), seconds, int(rss), command)
    return table


def descendants(root: int, table) -> set[int]:
    children: dict[int, list[int]] = {}
    for pid, row in table.items():
        children.setdefault(row[0], []).append(pid)
    out, stack = set(), [root]
    while stack:
        pid = stack.pop()
        if pid in out:
            continue
        out.add(pid)
        stack.extend(children.get(pid, []))
    return out


class Ctx:
    def __init__(self, args):
        self.root = Path(args.root).resolve()
        self.repo = Path(args.repo).resolve()
        self.results = Path(args.results).resolve()
        self.home, self.runtime, self.tmux_tmpdir = self.root / "h", self.root / "r", self.root / "t"
        self.data = self.root / "data"
        self.ssh_config = Path(args.ssh_config).resolve() if args.ssh_config else None
        self.ssh_target = args.ssh_target
        self.control_path = str(self.root / "cm")
        self.host = "local"
        self.app_proc = None
        self.webkit: set[int] = set()
        self.window, self.settle, self.typing_keys, self.seq = args.window, args.settle, args.typing_keys, args.seq
        self.meter = Path(args.meter) if args.meter else None
        self.sudo = subprocess.run(["sudo", "-n", "true"], capture_output=True).returncode == 0
        release = self.repo / "target" / "release"
        self.binaries = {"tauri": release / "bundle/macos/Muxflow.app/Contents/MacOS/muxflow",
                         "gpui": release / "muxflow-gpui"}
        self.host_binary = release / "muxflow-host"
        if len(str(self.tmux_tmpdir / f"tmux-{os.getuid()}" / TMUX_SOCKET)) > 103:
            raise SystemExit("--root too long for macOS socket paths (103 bytes)")

    def base_env(self) -> dict:
        path = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
        return {"HOME": str(self.home), "CFFIXED_USER_HOME": str(self.home), "USER": os.environ.get("USER", "bench"),
                "LOGNAME": os.environ.get("USER", "bench"), "LANG": "en_US.UTF-8", "SHELL": "/bin/bash",
                "PATH": path, "ADE_HOST_RUNTIME_DIR": str(self.runtime), "TMUX_TMPDIR": str(self.tmux_tmpdir),
                "ADE_TMUX_SOCKET_NAME": TMUX_SOCKET, "TMPDIR": os.environ.get("TMPDIR", "/tmp")}

    # terminal side --------------------------------------------------------
    def sh(self, script: str, check=False) -> str:
        if self.host == "ssh":
            cmd = ["ssh", "-F", str(self.ssh_config), "-o", "BatchMode=yes", "-o", "ControlMaster=auto",
                   "-o", f"ControlPath={self.control_path}", "-o", "ControlPersist=900", self.ssh_target, script]
            result = subprocess.run(cmd, capture_output=True, text=True)
        else:
            result = subprocess.run(["bash", "-c", script], env=self.base_env(), capture_output=True, text=True)
        if check and result.returncode:
            raise RuntimeError(f"{script!r}: {result.stderr}")
        return result.stdout

    def tmux(self, args: str) -> str:
        return self.sh(("tmux " if self.host == "ssh" else f"tmux -L {TMUX_SOCKET} ") + args)

    def panes(self) -> list[str]:
        return self.tmux("list-panes -s -t w1 -F '#{pane_id}'").split()

    def send(self, text: str, enter=True, pane=None) -> None:
        target = pane or self.panes()[0]
        self.tmux(f"send-keys -t {target} -l {shlex.quote(text)}" + (f" \\; send-keys -t {target} Enter" if enter else ""))

    def path(self, name: str) -> str:
        return f"~/bench/{name}" if self.host == "ssh" else str(self.data / name)

    def agent_paths(self) -> tuple[str, str]:
        if self.host == "ssh":
            return "~/bench/agents", "~/.local/bin/muxflow-host"
        return str(self.data / "agents"), str(self.host_binary)

    # state ----------------------------------------------------------------
    def config_dir(self) -> Path:
        return self.home / "Library" / "Application Support" / "dev.muxflow.desktop"

    def stop_host_side(self) -> None:
        subprocess.run([str(self.host_binary), "daemon-stop"], env=self.base_env(), capture_output=True)
        subprocess.run(["tmux", "-L", TMUX_SOCKET, "kill-server"], env=self.base_env(), capture_output=True)

    def reset(self, layout: str | None, seeded: bool) -> None:
        self.stop_app()
        self.stop_host_side()
        for path in (self.home, self.runtime, self.tmux_tmpdir):
            shutil.rmtree(path, ignore_errors=True)
            path.mkdir(parents=True)
        os.chmod(self.runtime, 0o700)
        os.chmod(self.tmux_tmpdir, 0o700)
        (self.home / "work").mkdir()
        (self.home / ".bashrc").write_text(f"PS1={shlex.quote(PROMPT)}\nHISTFILE=/dev/null\n")
        (self.home / ".bash_profile").write_text(". ~/.bashrc\n")
        if seeded:
            self.seed_config()
        if self.host == "ssh":
            self.seed_profiles()
            self.sh("tmux kill-session -t w1 2>/dev/null; ~/.local/bin/muxflow-host daemon-stop >/dev/null 2>&1; true")
            if layout:
                self.sh(layout_script(layout, "~", "tmux"), check=True)
        elif layout:
            self.sh(layout_script(layout, shlex.quote(str(self.home / "work")), f"tmux -L {TMUX_SOCKET}"), check=True)

    def seed_config(self) -> None:
        template = json.loads((self.repo / "crates/desktop-core/src/persistedAppState.contract.json").read_text())
        template.pop("_comment", None)
        template.update(appTabs=[], workspaceUi=[], workspaceDefaults={}, commands={"shortcutOverrides": {}},
                        hostSetup={"local": "declined", "bench": "declined"})
        x, y, w, h = WINDOW
        shell = template["shell"]
        for key in list(shell):
            if key not in ("panelSurface", "agentSort"):
                shell[key] = None if not isinstance(shell[key], bool) else False
        shell.update(panelSurface="files", agentSort="workspace", panelOpen=False, sidebarCollapsed=False,
                     windowGeometry={"x": x, "y": y, "width": w, "height": h, "maximized": False,
                                     "scaleFactorMilli": 2000})
        self.config_dir().mkdir(parents=True, exist_ok=True)
        (self.config_dir() / "app-state.json").write_text(json.dumps(template, indent=2))
        if self.host == "ssh":
            self.seed_profiles()

    def seed_profiles(self) -> None:
        self.config_dir().mkdir(parents=True, exist_ok=True)
        profiles = {"schemaVersion": 1, "lastProfileId": "bench", "profiles": [
            {"id": "local", "label": "Local", "connection": {"mode": "local"}, "shown": False},
            {"id": "bench", "label": "Bench", "shown": True, "connection": {
                "mode": "ssh", "profileId": "bench", "target": self.ssh_target, "configPath": str(self.ssh_config)}}]}
        (self.config_dir() / "profiles.json").write_text(json.dumps(profiles, indent=2))

    # app ------------------------------------------------------------------
    def webkit_pids(self) -> set[int]:
        return {pid for pid, row in ps_table().items() if "com.apple.WebKit." in row[3]}

    def launch(self, app: str) -> float:
        before = self.webkit_pids()
        log_path = self.results / "logs" / f"{app}-{self.host}.log"
        log_path.parent.mkdir(parents=True, exist_ok=True)
        t0 = time.monotonic()
        self.app_proc = subprocess.Popen([str(self.binaries[app])], env=self.base_env(), cwd=str(self.home),
                                         stdout=open(log_path, "a"), stderr=subprocess.STDOUT,
                                         start_new_session=True)
        self.app_name = app
        self._before_webkit = before
        return t0

    def attribute_webkit(self) -> None:
        if self.app_name == "tauri":
            self.webkit = self.webkit_pids() - self._before_webkit

    def window_mapped(self, t0: float, timeout=30) -> float | None:
        pid = self.app_proc.pid
        while time.monotonic() - t0 < timeout:
            count = osa(f'tell application "System Events" to count windows of (first process whose unix id is {pid})')
            if count.isdigit() and int(count) > 0:
                stamp = time.monotonic()
                x, y, w, h = WINDOW
                # Two statements: setting {position, size} together is refused ("Access not allowed").
                for attribute, value in (("position", f"{{{x}, {y}}}"), ("size", f"{{{w}, {h}}}")):
                    osa(f'tell application "System Events" to tell (first process whose unix id is {pid}) to '
                        f'set {attribute} of window 1 to {value}')
                osa(f'tell application "System Events" to set frontmost of (first process whose unix id is {pid}) to true')
                return stamp
            time.sleep(0.03)
        return None

    def grab(self, region=WINDOW) -> np.ndarray:
        # cua-driver window capture instead of `screencapture -R`: screencapture is refused
        # without Screen Recording for the launching terminal; cua-driver holds its own grant.
        # It captures the app's window (placed at WINDOW), Retina pixels, ~180 ms per frame.
        pid, wid = self.app_proc.pid, self.window_id()
        if wid is None:
            return np.zeros((1, 1, 3), dtype=np.uint8)
        out = self.root / "grab.png"
        out.unlink(missing_ok=True)
        subprocess.run(["cua-driver", "get_window_state", json.dumps({
            "pid": pid, "window_id": wid, "include_accessibility_tree": False, "display_only": True,
            "max_image_dimension": 0, "screenshot_out_file": str(out)})], capture_output=True)
        if not out.exists():
            return np.zeros((1, 1, 3), dtype=np.uint8)
        return np.asarray(Image.open(out).convert("RGB"))

    def window_id(self) -> int | None:
        """CGWindowID of the app's main window. System Events has no AXWindowNumber for
        either app's window, so ask cua-driver (largest on-screen layer-0 window)."""
        pid = self.app_proc.pid
        if getattr(self, "_wid", (None, None))[0] == pid:
            return self._wid[1]
        out = subprocess.run(["cua-driver", "list_windows", json.dumps({"pid": pid})],
                             capture_output=True, text=True).stdout
        try:
            windows = [w for w in json.loads(out, strict=False)["windows"]
                       if w.get("layer") == 0 and w.get("is_on_screen") and w["bounds"]["width"] > 400]
        except (ValueError, KeyError):
            return None
        if not windows:
            return None
        best = max(windows, key=lambda w: w["bounds"]["width"] * w["bounds"]["height"])
        if abs(best["bounds"]["width"] - WINDOW[2]) > 2:
            return None  # not resized yet; do not cache a pre-resize lookup
        self._wid = (pid, int(best["window_id"]))
        return self._wid[1]

    def evidence(self, name: str) -> None:
        """Window screenshot to <results>/<host>/evidence/, as on Linux: proof the input landed."""
        path = self.results / self.host / "evidence" / f"{self.app_name}-{name}.png"
        path.parent.mkdir(parents=True, exist_ok=True)
        Image.fromarray(self.grab()).save(path)

    def wait_content(self, t0: float, timeout=60) -> float | None:
        while time.monotonic() - t0 < timeout:
            if count_color(self.grab(), MAGENTA) >= 300:
                return time.monotonic()
        return None

    def keystroke(self, text: str = "", key_code: int | None = None, modifiers: str = "") -> None:
        using = f" using {{{modifiers}}}" if modifiers else ""
        action = f"key code {key_code}" if key_code is not None else f"keystroke {json.dumps(text)}"
        osa(f'tell application "System Events" to {action}{using}')

    def minimize(self, value: bool) -> None:
        pid = self.app_proc.pid
        osa(f'tell application "System Events" to tell (first process whose unix id is {pid}) to '
            f'set value of attribute "AXMinimized" of window 1 to {str(value).lower()}')

    def stop_app(self) -> None:
        if self.app_proc is None:
            return
        pid = self.app_proc.pid
        self.app_proc.terminate()
        try:
            self.app_proc.wait(10)
        except subprocess.TimeoutExpired:
            self.app_proc.kill()
        try:
            os.killpg(pid, signal.SIGTERM)
        except OSError:
            pass
        self.app_proc = None
        self.webkit = set()


class Sampler(threading.Thread):
    def __init__(self, ctx: Ctx, interval: float):
        super().__init__(daemon=True)
        self.ctx, self.interval = ctx, interval
        self.ticks: list[dict] = []
        self.stop_event = threading.Event()
        self.gpu: subprocess.Popen | None = None
        self.gpu_file = ctx.root / "powermetrics.plist"

    def classify(self, table) -> dict[int, str]:
        cats: dict[int, str] = {}
        tmux_server = [pid for pid, row in table.items() if row[3].startswith("tmux") and f"-L {TMUX_SOCKET}" in row[3]
                       and row[0] == 1]
        for server in tmux_server:
            for pid in descendants(server, table):
                cats[pid] = "tmux" if table[pid][3].startswith("tmux") else "panes"
        if self.ctx.app_proc and self.ctx.app_proc.pid in table:
            for pid in descendants(self.ctx.app_proc.pid, table):
                command = table[pid][3]
                cats[pid] = ("host" if "muxflow-host" in command else "ssh" if command.startswith("ssh ")
                             else "tmux" if command.startswith("tmux") else "app")
        for pid in self.ctx.webkit:
            if pid in table:
                cats[pid] = "app"
        for pid, row in table.items():
            if pid in cats:
                continue
            if "muxflow-host" in row[3] and str(self.ctx.runtime) in row[3]:
                for child in descendants(pid, table):
                    cats[child] = "tmux" if table[child][3].startswith("tmux") else "host"
            elif row[3].startswith("ssh ") and "/muxflow-" in row[3] and "/ssh-" in row[3]:
                cats[pid] = "ssh"
            elif row[3].endswith("/WindowServer") or " WindowServer " in f" {row[3]} ":
                cats[pid] = "compositor"
        return cats

    def run(self) -> None:
        if self.ctx.sudo:
            self.gpu = subprocess.Popen(["sudo", "-n", "powermetrics", "--samplers", "tasks", "--show-process-gpu",
                                         "-i", str(int(self.interval * 1000)), "-f", "plist", "-o", str(self.gpu_file)],
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        previous: dict[int, float] = {}
        tick = 0
        next_due = time.monotonic()
        while True:
            stamp = time.monotonic()
            table = ps_table()
            cats = self.classify(table)
            cpu: dict[str, int] = {}
            rss: dict[str, int] = {}
            for pid, cat in cats.items():
                seconds = table[pid][1]
                if pid in previous:
                    cpu[cat] = cpu.get(cat, 0) + round((seconds - previous[pid]) * bl.CLK_TCK)
                rss[cat] = rss.get(cat, 0) + table[pid][2]
            previous = {pid: table[pid][1] for pid in cats}
            entry = {"t": stamp, "cpu": cpu if tick else {}, "rss": rss, "pids": {str(p): c for p, c in cats.items()}}
            if tick % 5 == 0:
                entry["pss"] = self.footprint(cats)  # phys_footprint stands in for PSS
            self.ticks.append(entry)
            tick += 1
            if self.stop_event.is_set():
                break
            next_due += self.interval
            self.stop_event.wait(max(0.0, next_due - time.monotonic()))
        if self.gpu:
            subprocess.run(["sudo", "-n", "kill", "-INT", str(self.gpu.pid)], capture_output=True)
            self.gpu.wait(10)

    def footprint(self, cats: dict[int, str]) -> dict[str, int]:
        out: dict[str, int] = {}
        for cat in ("app", "host", "tmux"):
            pids = [str(pid) for pid, c in cats.items() if c == cat]
            if not pids:
                continue
            target = self.ctx.root / "footprint.json"
            subprocess.run(["footprint", "-j", str(target), "-f", "bytes", *pids], capture_output=True)
            try:
                out[cat] = int(json.loads(target.read_text())["total footprint"]) // 1024
            except (OSError, KeyError, ValueError):
                pass
        return out

    def gpu_summary(self, cats_by_pid: dict[str, str], duration: float) -> dict[str, float]:
        if not self.gpu_file.exists():
            return {}
        busy: dict[str, float] = {}
        for chunk in self.gpu_file.read_bytes().split(b"\0"):
            if not chunk.strip():
                continue
            try:
                sample = plistlib.loads(chunk.strip())
            except Exception:
                continue
            seconds = sample.get("elapsed_ns", self.interval * 1e9) / 1e9
            for task in sample.get("tasks", []):
                cat = cats_by_pid.get(str(task.get("pid")))
                if cat and "gputime_ms_per_s" in task:
                    busy[cat] = busy.get(cat, 0.0) + task["gputime_ms_per_s"] / 1000 * seconds
        self.gpu_file.unlink()
        return {cat: round(100 * value / duration, 2) for cat, value in busy.items()}

    def finish(self) -> tuple[list[dict], dict]:
        self.stop_event.set()
        self.join(60)
        result = bl.summarize_ticks(self.ticks)
        cats_by_pid: dict[str, str] = {}
        for tick in self.ticks:
            cats_by_pid.update(tick.get("pids", {}))
        result["gpu_pct"] = self.gpu_summary(cats_by_pid, result.get("duration_s", 1) or 1) if self.ctx.sudo else {}
        if not self.ctx.sudo:
            result["gpu_note"] = "not measured: powermetrics needs passwordless sudo"
        return self.ticks, result


def remote_window(ctx: Ctx, tag: str, start: bool) -> list | None:
    if ctx.host != "ssh":
        return None
    if start:
        ctx.sh(f"rm -f ~/bench/stop; nohup perl ~/bench/remote-sampler.pl ~/bench/s-{tag}.txt ~/bench/stop 1 >/dev/null 2>&1 &")
        return None
    from bench_linux import remote_ticks
    return remote_ticks(ctx.sh(f"touch ~/bench/stop; sleep 1.2; cat ~/bench/s-{tag}.txt; rm -f ~/bench/s-{tag}.txt ~/bench/stop"))


def measure(ctx: Ctx, scenario: str, app: str, run: int, interval: float, action) -> None:
    tag = f"{scenario}-{app}-{run}"
    remote_window(ctx, tag, True)
    sampler = Sampler(ctx, interval)
    sampler.start()
    metrics = action() or {}
    ticks, result = sampler.finish()
    remote = remote_window(ctx, tag, False)
    if remote is not None:
        result["remote"] = bl.summarize_ticks(remote)
    if metrics:
        result["metrics"] = metrics
    out = ctx.results / ctx.host / scenario
    out.mkdir(parents=True, exist_ok=True)
    result.update(app=app, host=ctx.host, scenario=scenario, run=run)
    (out / f"{app}-run{run}.json").write_text(json.dumps(result, indent=2))
    bl.write_jsonl(str(out / f"{app}-run{run}.ticks.jsonl"), ticks)
    log(f"  {ctx.host}/{scenario}/{app}#{run}: {json.dumps(metrics)} cpu%={json.dumps(result.get('cpu_pct', {}))}")


def hook(ctx: Ctx, pane: str, index: int, event: str) -> None:
    _, host = ctx.agent_paths()
    payload = json.dumps({"hook_event_name": event, "session_id": f"app-bench-{index}"})
    inner = (f"printf '%s' {shlex.quote(payload)} | TMUX='#{{socket_path}},#{{pid}},#{{session_id}}' "
             f"TMUX_PANE='#{{pane_id}}' {host} hook ingest --adapter {ADAPTERS[AGENT_NAMES[index]]}")
    ctx.tmux(f"run-shell -t {pane} {shlex.quote(inner)}")


def start_agents(ctx: Ctx, mode: str) -> list[str]:
    root, _ = ctx.agent_paths()
    panes = ctx.panes()[:6]
    for index, pane in enumerate(panes):
        name = AGENT_NAMES[index]
        extra = "--idle" if mode == "idle" else f"--offset {5 + 17 * index}"
        ctx.tmux(f"respawn-pane -k -t {pane} {shlex.quote(f'exec {root}/bin/{name} {root}/{name} {extra}')}")
    return panes


def launch_ready(ctx: Ctx, app: str) -> tuple[float, float | None, float | None]:
    t0 = ctx.launch(app)
    mapped = ctx.window_mapped(t0)
    content = ctx.wait_content(t0)
    ctx.attribute_webkit()
    return t0, mapped, content


def run_app(ctx: Ctx, app: str, run: int, scenarios: set[str]) -> None:
    def sleep_window():
        time.sleep(ctx.window)

    if scenarios & {"startup", "idle", "heavy", "typing"}:
        ctx.reset("single", seeded=False)
        if ctx.host == "ssh":
            ctx.seed_profiles()
        if "startup" in scenarios:
            measure(ctx, "startup-cold", app, run, 0.25, lambda: startup_metrics(ctx, app))
            ctx.stop_app()
        ctx.seed_config()
        if "startup" in scenarios:
            measure(ctx, "startup-warm", app, run, 0.25, lambda: startup_metrics(ctx, app))
        else:
            launch_ready(ctx, app)
        if "idle" in scenarios:
            time.sleep(ctx.settle)
            measure(ctx, "idle-1x1", app, run, 1.0, sleep_window)
        if "heavy" in scenarios:
            for kind, command in (("seq", f"seq 1 {ctx.seq}"), ("cat", f"cat {ctx.path('colored.txt')}")):
                ctx.send("clear")
                time.sleep(1.5)
                measure(ctx, f"heavy-{kind}", app, run, 0.5, lambda c=command: heavy_action(ctx, c))
        if "typing" in scenarios:
            typing(ctx, app, run)
        ctx.stop_app()
    if scenarios & {"idle", "stream"}:
        ctx.reset("large", seeded=True)
        launch_ready(ctx, app)
        time.sleep(ctx.settle)
        if "idle" in scenarios:
            measure(ctx, "idle-8x2", app, run, 1.0, sleep_window)
        if "stream" in scenarios:
            ctx.send(f"clear; perl {ctx.path('stream.pl')} {ctx.window} 60")
            measure(ctx, "stream", app, run, 1.0, lambda: time.sleep(ctx.window + 1))
        ctx.stop_app()
    if scenarios & {"agents", "spinner"}:
        ctx.reset("agents", seeded=True)
        if ctx.host == "local":
            for name in (".claude", ".codex"):
                (ctx.home / name).mkdir(exist_ok=True)
            subprocess.run([str(ctx.host_binary), "hook", "install", "--home", str(ctx.home), "--yes"],
                           env=ctx.base_env(), capture_output=True)
        launch_ready(ctx, app)
        if "agents" in scenarios:
            panes = start_agents(ctx, "replay")
            time.sleep(1)
            for index, pane in enumerate(panes):
                hook(ctx, pane, index, "UserPromptSubmit")
            time.sleep(ctx.settle)

            def agents_action():
                started, done = time.monotonic(), 0
                while time.monotonic() - started < ctx.window - 10:
                    time.sleep(7)
                    index = done % len(panes)
                    hook(ctx, panes[index], index, "Stop")
                    time.sleep(3)
                    hook(ctx, panes[index], index, "UserPromptSubmit")
                    done += 1
                time.sleep(max(0, ctx.window - (time.monotonic() - started)))
                ctx.evidence(f"agents-run{run}")
                return {"done_events": done}

            measure(ctx, "agents-replay", app, run, 1.0, agents_action)
        if "spinner" in scenarios:
            panes = start_agents(ctx, "idle")
            time.sleep(2)
            for index, pane in enumerate(panes):
                hook(ctx, pane, index, "UserPromptSubmit")
            time.sleep(ctx.settle)
            measure(ctx, "spinner-visible", app, run, 1.0, sleep_window)
            ctx.evidence(f"spinner-run{run}")
            ctx.minimize(True)
            time.sleep(5)
            measure(ctx, "spinner-hidden", app, run, 1.0, sleep_window)
            ctx.minimize(False)
        ctx.stop_app()
    if scenarios & {"editor", "explorer"}:
        ctx.reset("single", seeded=True)
        if ctx.host == "local":
            ctx.sh(files_fixture_script(shlex.quote(str(ctx.home / "work"))))
        for scenario in ("editor", "explorer"):
            if scenario not in scenarios:
                continue
            ctx.seed_config()
            launch_ready(ctx, app)
            time.sleep(ctx.settle)
            measure(ctx, scenario, app, run, 0.5, (lambda: editor_action(ctx)) if scenario == "editor"
                    else (lambda: explorer_action(ctx)))
            ctx.stop_app()


def startup_metrics(ctx: Ctx, app: str) -> dict:
    t0, mapped, content = launch_ready(ctx, app)
    time.sleep(2)
    return {"window_map_ms": round((mapped - t0) * 1000, 1) if mapped else None,
            "first_content_ms": round((content - t0) * 1000, 1) if content else None}


def heavy_action(ctx: Ctx, command: str) -> dict:
    t0 = time.monotonic()
    ctx.send(f"{command}; {SENTINEL}")
    done = None
    while time.monotonic() - t0 < 900:
        if has_green_bar(ctx.grab(), run=300):  # Retina: 2 px per point
            done = time.monotonic()
            break
    time.sleep(3)
    ctx.evidence(f"heavy-{command.split()[0]}")
    return {"wall_s": round(done - t0, 3) if done else None}


def editor_action(ctx: Ctx) -> dict:
    ctx.keystroke("p", modifiers="command down")
    time.sleep(1)
    ctx.keystroke("big.txt")
    time.sleep(1.5)
    ctx.keystroke(key_code=36)  # Return
    time.sleep(6)
    ctx.evidence("editor")
    # Same as Linux: no scroll, because on Linux PageDown did not reach Tauri's
    # editor after Quick Open. If both editors scroll here (check a screenshot),
    # a scroll phase can be added for both.
    return {"opened": "big.txt"}


def explorer_action(ctx: Ctx) -> dict:
    ctx.keystroke("e", modifiers="command down, shift down")
    time.sleep(2)
    ctx.keystroke(key_code=125)  # Down
    time.sleep(0.5)
    ctx.keystroke(key_code=124)  # Right: expand
    time.sleep(4)
    for _ in range(300):
        ctx.keystroke(key_code=125)
        time.sleep(0.02)
    time.sleep(2)
    ctx.evidence("explorer")
    return {"down_presses": 300}


def typing(ctx: Ctx, app: str, run: int) -> None:
    out = ctx.results / ctx.host / "typing"
    out.mkdir(parents=True, exist_ok=True)
    if ctx.meter is None:
        result = {"metrics": {"error": "not measured: needs --meter (ScreenCaptureKit + CGEvent meter)"}}
    else:
        result = {"metrics": meter_typing(ctx, run)}
    result.update(app=app, host=ctx.host, scenario="typing", run=run)
    (out / f"{app}-run{run}.json").write_text(json.dumps(result, indent=2))
    log(f"  {ctx.host}/typing/{app}#{run}: {json.dumps(result['metrics'])[:200]}")


def meter_typing(ctx: Ctx, run: int) -> dict:
    """Drives tests/performance/runtime/macos/pixels.swift (op=key) the way run.py does:
    a real HID key, then the first captured frame whose region changed."""
    # Cursor hidden: the meter times the first frame whose region changes, and a blinking
    # cursor would be that change. The probe's answer (a solid #0000ff cell) is then the only one.
    ctx.send(f"clear; printf '\\033[?25l'; perl {ctx.path('keyprobe.pl')}")
    time.sleep(2)
    fifo_in, fifo_out = ctx.root / "meter-in", ctx.root / "meter-out"
    for path in (fifo_in, fifo_out):
        if path.exists():
            path.unlink()
    os.mkfifo(fifo_in)
    window_id = ctx.window_id()
    if window_id is None:
        return {"error": "not measured: app window not found for the meter"}
    window_id = str(window_id)
    # Started as a one-shot launchd job, as runtime/macos/run.py does: as a plain child of
    # the terminal, TCC would check the terminal's grants instead of the meter's own.
    label = "dev.muxflow.s11-meter-bench"
    job = ctx.root / "meter-bench.plist"
    subprocess.run(["launchctl", "bootout", f"gui/{os.getuid()}/{label}"], capture_output=True)
    with job.open("wb") as fp:
        plistlib.dump({"Label": label, "RunAtLoad": True, "KeepAlive": False,
                       "ProgramArguments": [str(ctx.meter), str(ctx.app_proc.pid), window_id, str(fifo_in), str(fifo_out)],
                       "StandardErrorPath": str(ctx.root / "meter-errors.log")}, fp)
    subprocess.run(["launchctl", "bootstrap", f"gui/{os.getuid()}", str(job)], check=True)
    writer = fifo_in.open("w")
    for _ in range(100):
        if fifo_out.exists():
            break
        time.sleep(0.05)
    reader = fifo_out.open()

    def answer(timeout: float) -> dict:
        # meter-out is a regular file the meter appends to: poll until a full line lands.
        end, buffer = time.monotonic() + timeout, ""
        while time.monotonic() < end:
            buffer += reader.readline()
            if buffer.endswith("\n"):
                return json.loads(buffer)
            time.sleep(0.002)
        return {"error": "meter did not answer"}

    ready = answer(15)
    if not ready.get("ready"):
        subprocess.run(["launchctl", "bootout", f"gui/{os.getuid()}/{label}"], capture_output=True)
        ctx.send("q", enter=False)
        return {"error": f"not measured: meter: {ready.get('error', ready)}"}
    osa(f'tell application "System Events" to set frontmost of (first process whose unix id is {ctx.app_proc.pid}) to true')
    time.sleep(0.5)
    width, height = ready.get("width", 0), ready.get("height", 0)
    rect = [int(width * 0.16), 0, int(width * 0.84), int(height * 0.25)]
    rng = random.Random(1000 + run)
    latencies, misses, errors = [], 0, []
    for _ in range(ctx.typing_keys):
        time.sleep(rng.uniform(0.25, 0.4))
        writer.write(json.dumps({"op": "key", "code": 7, "rect": rect, "timeout": 3}) + "\n")  # 7 = 'x'
        writer.flush()
        reply = answer(15)
        if reply.get("ms") is None:  # timeout, or "focus lost; refusing input"
            misses += 1
            errors.append(reply.get("error", "timeout"))
        else:
            latencies.append(float(reply["ms"]))
    ctx.evidence(f"typing-run{run}")
    writer.write(json.dumps({"op": "quit"}) + "\n")
    writer.flush()
    time.sleep(1)
    subprocess.run(["launchctl", "bootout", f"gui/{os.getuid()}/{label}"], capture_output=True)
    ctx.send("q", enter=False)
    return {"keys": ctx.typing_keys, "missed": misses, "miss_reasons": sorted(set(errors)),
            "latency_ms_median": round(statistics.median(latencies), 1) if latencies else None,
            "latency_ms_p95": round(bl.percentile(latencies, 0.95), 1) if latencies else None,
            "latency_ms_min": round(min(latencies), 1) if latencies else None,
            "latencies_ms": [round(v, 1) for v in latencies]}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", default="/tmp/mb")
    parser.add_argument("--repo", required=True)
    parser.add_argument("--results", required=True)
    parser.add_argument("--apps", default="tauri,gpui")
    parser.add_argument("--hosts", default="local,ssh")
    parser.add_argument("--scenarios", default="startup,idle,heavy,typing,stream,agents,spinner,editor")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--window", type=float, default=60.0)
    parser.add_argument("--settle", type=float, default=15.0)
    parser.add_argument("--seq", type=int, default=3_000_000)
    parser.add_argument("--typing-keys", type=int, default=40)
    parser.add_argument("--recordings")
    parser.add_argument("--meter", help="the built S11 meter executable (pixels.swift)")
    parser.add_argument("--ssh-config")
    parser.add_argument("--ssh-target", default="muxflow-bench")
    args = parser.parse_args()
    if subprocess.run(["pgrep", "-x", "Safari"], capture_output=True).returncode == 0:
        raise SystemExit("quit Safari first: its WebKit XPC processes would be attributed to Tauri")
    ctx = Ctx(args)
    ctx.results.mkdir(parents=True, exist_ok=True)
    ctx.data.mkdir(parents=True, exist_ok=True)
    with open(ctx.data / "colored.txt", "w") as handle:
        subprocess.run(["perl", str(HERE / "workloads/colored.pl"), "200000"], stdout=handle, check=True)
    for name in ("stream.pl", "keyprobe.pl"):
        shutil.copy(HERE / "workloads" / name, ctx.data / name)
    scenarios = set(args.scenarios.split(","))
    if args.recordings and scenarios & {"agents", "spinner"}:
        agents = ctx.data / "agents"
        (agents / "bin").mkdir(parents=True, exist_ok=True)
        for name in ("claude", "codex"):
            for suffix in (".data", ".timing"):
                shutil.copy(Path(args.recordings) / f"{name}{suffix}", agents / f"{name}{suffix}")
            shutil.copy(Path(args.recordings) / "bin" / "agent-replay", agents / "bin" / name)
    meta = {"started": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "platform": "macOS", "runs": args.runs,
            "window_s": args.window, "settle_s": args.settle, "compositor": "WindowServer",
            "gpu": "powermetrics" if ctx.sudo else "not measured", "typing": "meter" if ctx.meter else "not measured",
            "hosts": args.hosts.split(","), "apps": args.apps.split(",")}
    (ctx.results / "meta.json").write_text(json.dumps(meta, indent=2))
    try:
        for host in args.hosts.split(","):
            ctx.host = host
            if host == "ssh":
                # The VM is prepared by the Linux runner (helper, workloads, hooks); the
                # Mac only needs the replay binary for its own architecture there: none,
                # the VM is Linux and already has it.
                ctx.sh("test -x ~/.local/bin/muxflow-host && test -s ~/bench/colored.txt", check=True)
            for run in range(1, args.runs + 1):
                apps = args.apps.split(",")
                for app in (apps if run % 2 else list(reversed(apps))):
                    log(f"{host} {app} run {run}")
                    run_app(ctx, app, run, scenarios)
    finally:
        ctx.stop_app()
        host, ctx.host = ctx.host, "local"
        ctx.stop_host_side()
        if ctx.ssh_config:
            ctx.host = "ssh"
            ctx.sh("tmux kill-session -t w1 2>/dev/null; ~/.local/bin/muxflow-host daemon-stop >/dev/null 2>&1; true")
            subprocess.run(["ssh", "-F", str(ctx.ssh_config), "-o", f"ControlPath={ctx.control_path}", "-O", "exit",
                            ctx.ssh_target], capture_output=True)
        log("stopped the app, the helper daemon, the bench tmux server and the remote sessions")
    return 0


if __name__ == "__main__":
    sys.exit(main())
