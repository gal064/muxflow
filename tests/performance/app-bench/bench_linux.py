#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy>=1.26"]
# ///
"""Tauri vs GPUI desktop benchmark, Linux/Wayland (Hyprland) runner.

See README.md for the method. Run through run-linux.sh, which checks the
builds and the environment first.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import shlex
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import threading
import time
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import benchlib as bl  # noqa: E402

APPS = {
    "tauri": {"binary": "muxflow", "class": "muxflow"},
    "gpui": {"binary": "muxflow-gpui", "class": "dev.muxflow.desktop"},
}
WINDOW = (160, 40, 1600, 1000)  # x, y, width, height on the bench output
MAGENTA = (255, 0, 255)  # shell prompt background, see seed_shell()
GREEN = (0, 255, 0)  # end-of-output sentinel
BLUE = (0, 0, 255)  # keyprobe.pl echo
PROMPT = r"\[\e[48;2;255;0;255m\]  BENCH  \[\e[0m\] "
# A solid 40-cell bar: no workload output forms a 150 px run of saturated green.
SENTINEL = r"printf '\e[48;2;0;255;0m%40s\e[0m\n' ''"
TMUX_SOCKET = "b"  # short: the socket path must fit in sun_path


def log(message: str) -> None:
    print(f"[{time.strftime('%H:%M:%S')}] {message}", flush=True)


# ---------------------------------------------------------------- compositor


class Compositor:
    """A Hyprland instance: the private nested one the runner starts, or the session's."""

    def __init__(self, root: Path, mode: str, session_signature: str | None):
        self.root = root
        self.mode = mode
        self.proc: subprocess.Popen | None = None
        # Private mode must never start from the session's signature: every
        # hyprctl call below would otherwise reconfigure the user's compositor.
        self.session_signature = session_signature
        self.signature = session_signature if mode == "session" else None
        self.wayland_display = os.environ.get("WAYLAND_DISPLAY", "wayland-1")
        self.output = None
        self.pid = None
        self.events: list[tuple[float, str]] = []
        self._events_lock = threading.Lock()
        self.runtime = os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")

    def start(self) -> None:
        if self.mode == "session":
            self.output = json.loads(self.hyprctl("monitors", "-j"))[0]["name"]
            self.pid = int(json.loads(self.hyprctl("instances", "-j"))[0]["pid"])
            self._start_events()
            return
        hypr = self.root / "hypr"
        (hypr / "home").mkdir(parents=True, exist_ok=True)
        config = hypr / "hyprland.lua"
        classes = "|".join(app["class"].replace(".", "\\\\.") for app in APPS.values())
        x, y, w, h = WINDOW
        config.write_text(
            "-- Private compositor for the app benchmark: no autostart, no animations, no blur.\n"
            'hl.monitor({ output = "", mode = "1920x1080@60", position = "0x0", scale = 1 })\n'
            "hl.config({\n"
            '  general = { gaps_in = 0, gaps_out = 0, border_size = 1, layout = "dwindle" },\n'
            "  decoration = { rounding = 0, shadow = { enabled = false }, blur = { enabled = false },\n"
            "                 active_opacity = 1.0, inactive_opacity = 1.0 },\n"
            "  animations = { enabled = false },\n"
            "  misc = { force_default_wallpaper = 0, disable_hyprland_logo = true },\n"
            "  xwayland = { enabled = false },\n"
            '  input = { kb_layout = "us", follow_mouse = 0 },\n'
            "})\n"
            f'hl.window_rule({{ match = {{ class = "^({classes})$" }}, float = true, size = {{ {w}, {h} }},\n'
            f'  move = {{ {x}, {y} }}, suppress_event = "maximize fullscreen" }})\n'
        )
        before = set(os.listdir(f"{self.runtime}/hypr"))
        sockets_before = {n for n in os.listdir(self.runtime) if n.startswith("wayland-") and not n.endswith(".lock")}
        env = {
            "HOME": str(hypr / "home"),
            "PATH": "/usr/local/bin:/usr/bin:/bin",
            "XDG_RUNTIME_DIR": self.runtime,
            "WAYLAND_DISPLAY": os.environ["WAYLAND_DISPLAY"],
            "HYPRLAND_NO_SD_NOTIFY": "1",
            "HYPRLAND_NO_CRASHREPORTER": "1",
        }
        self.proc = subprocess.Popen(
            ["Hyprland", "--config", str(config)],
            env=env,
            stdout=open(hypr / "hyprland.log", "w"),
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        self.pid = self.proc.pid
        for _ in range(100):
            time.sleep(0.1)
            new = set(os.listdir(f"{self.runtime}/hypr")) - before
            for signature in new:
                if (os.path.exists(f"{self.runtime}/hypr/{signature}/.socket.sock")
                        and self._lock_pid(signature) == self.proc.pid):
                    self.signature = signature
            if self.signature:
                break
        if not self.signature:
            raise RuntimeError("private Hyprland did not start")
        time.sleep(1.0)
        sockets = {n for n in os.listdir(self.runtime) if n.startswith("wayland-") and not n.endswith(".lock")}
        new_sockets = sorted(sockets - sockets_before)
        if len(new_sockets) != 1:
            raise RuntimeError(f"cannot tell the private compositor's socket: {new_sockets}")
        self.wayland_display = new_sockets[0]
        if self.wayland_display == os.environ["WAYLAND_DISPLAY"]:
            raise RuntimeError("private compositor resolved to the session's own socket")
        # The nested window in the parent session is never shown to us: disable
        # it and render to one headless output with a fixed mode instead.
        self.hyprctl("output", "create", "headless", "BENCH")
        time.sleep(0.5)
        for monitor in json.loads(self.hyprctl("monitors", "all", "-j")):
            if monitor["name"] != "BENCH":
                self.hyprctl("eval", f"hl.monitor({{output='{monitor['name']}', disabled=true}})")
        self.hyprctl("eval", "hl.monitor({output='BENCH', mode='1920x1080@60', position='0x0', scale=1})")
        time.sleep(0.5)
        # The brief two-outputs-at-0,0 moment leaves a layout warning drawn over the window.
        self.hyprctl("dismissnotify")
        self.output = "BENCH"
        self._start_events()
        log(f"private Hyprland pid {self.pid}, {self.wayland_display}, output {self.output}")

    def stop(self) -> None:
        if self.proc is not None:
            self.proc.terminate()
            try:
                self.proc.wait(10)
            except subprocess.TimeoutExpired:
                self.proc.kill()

    def _lock_pid(self, signature: str) -> int | None:
        try:
            first = Path(f"{self.runtime}/hypr/{signature}/hyprland.lock").read_text().split("\n", 1)[0]
            return int(first.strip())
        except (OSError, ValueError):
            return None

    def hyprctl(self, *args: str) -> str:
        if not self.signature:
            raise RuntimeError("no compositor instance selected")
        if self.mode == "private":
            # Only ever the instance this harness started: its lock file names our pid.
            if (self.proc is None or self.signature == self.session_signature
                    or self._lock_pid(self.signature) != self.proc.pid):
                raise RuntimeError("refusing to send hyprctl commands to a compositor we did not start")
        env = dict(os.environ, HYPRLAND_INSTANCE_SIGNATURE=self.signature)
        return subprocess.run(["hyprctl", *args], env=env, capture_output=True, text=True).stdout

    def clients(self) -> list[dict]:
        try:
            return json.loads(self.hyprctl("clients", "-j"))
        except json.JSONDecodeError:
            return []

    def _start_events(self) -> None:
        path = f"{self.runtime}/hypr/{self.signature}/.socket2.sock"

        def reader() -> None:
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            sock.connect(path)
            buffer = b""
            while True:
                data = sock.recv(65536)
                if not data:
                    return
                now = time.monotonic()
                buffer += data
                *lines, buffer = buffer.split(b"\n")
                with self._events_lock:
                    for line in lines:
                        self.events.append((now, line.decode(errors="replace")))

        threading.Thread(target=reader, daemon=True).start()

    def open_window_after(self, t0: float, window_class: str) -> float | None:
        with self._events_lock:
            for stamp, line in self.events:
                if stamp >= t0 and line.startswith("openwindow>>"):
                    parts = line[len("openwindow>>") :].split(",", 3)
                    if len(parts) >= 3 and parts[2] == window_class:
                        return stamp
        return None

    def grab(self, region: tuple[int, int, int, int] = WINDOW) -> np.ndarray:
        x, y, w, h = region
        env = dict(os.environ, WAYLAND_DISPLAY=self.wayland_display)
        raw = subprocess.run(
            ["grim", "-t", "ppm", "-g", f"{x},{y} {w}x{h}", "-"], env=env, capture_output=True, check=True
        ).stdout
        # P6 header: magic, width, height, maxval, each followed by whitespace.
        fields, index = [], 0
        while len(fields) < 4:
            while raw[index : index + 1].isspace():
                index += 1
            start = index
            while not raw[index : index + 1].isspace():
                index += 1
            fields.append(raw[start:index])
        width, height = int(fields[1]), int(fields[2])
        pixels = np.frombuffer(raw, dtype=np.uint8, offset=index + 1, count=width * height * 3)
        return pixels.reshape(height, width, 3)

    def save_png(self, path: Path, region: tuple[int, int, int, int] = WINDOW) -> None:
        x, y, w, h = region
        env = dict(os.environ, WAYLAND_DISPLAY=self.wayland_display)
        subprocess.run(["grim", "-g", f"{x},{y} {w}x{h}", str(path)], env=env, check=False)

    def show_workspace(self, workspace: int) -> None:
        self.hyprctl("dispatch", f"hl.dsp.focus({{workspace='{workspace}'}})")

    def type_keys(self, args: list[str]) -> subprocess.Popen:
        # Never inject keys into the session compositor unless asked to: a locked
        # session would receive them in its password field.
        if self.mode == "private" and self.wayland_display == os.environ.get("WAYLAND_DISPLAY"):
            raise RuntimeError("refusing to type into the session compositor")
        env = dict(os.environ, WAYLAND_DISPLAY=self.wayland_display)
        return subprocess.Popen(["wtype", *args], env=env)


def count_color(image: np.ndarray, rgb: tuple[int, int, int]) -> int:
    """Pixels of the marker's hue. Hue, not exact value: a modal backdrop or an
    inactive-pane dim darkens the marker but keeps its hue, and neither app's
    chrome uses these saturated primaries."""
    r, g, b = (image[..., i].astype(np.int16) for i in range(3))
    if rgb == MAGENTA:
        mask = (r > 100) & (b > 100) & (np.abs(r - b) < 50) & (g * 2 < np.minimum(r, b))
    elif rgb == GREEN:
        mask = (g > 100) & (r * 2 < g) & (b * 2 < g)
    elif rgb == BLUE:
        mask = (b > 100) & (r * 5 < b * 2) & (g * 5 < b * 2)
    else:
        raise ValueError(rgb)
    return int(np.count_nonzero(mask))


def has_green_bar(image: np.ndarray, run: int = 150) -> bool:
    """True when some row holds `run` consecutive saturated-green pixels (the sentinel)."""
    r, g, b = (image[..., i].astype(np.int16) for i in range(3))
    mask = (g > 200) & (r < 70) & (b < 70)
    rows = mask[mask.sum(axis=1) >= run]
    if rows.size == 0:
        return False
    sums = np.cumsum(np.pad(rows, ((0, 0), (1, 0))).astype(np.int32), axis=1)
    return bool(np.any(sums[:, run:] - sums[:, :-run] == run))


# ---------------------------------------------------------------- terminal side


def layout_script(layout: str, cwd: str, tmux: str) -> str:
    """Shell that builds a bench layout on a tmux server.

    single: one window, one pane. large: 8 windows of 2 side-by-side panes.
    agents: one window of 6 tiled panes (3 columns x 2 rows).
    """
    lines = [f"set -e", f"{tmux} new-session -d -s w1 -c {cwd} -x 200 -y 50"]
    if layout == "large":
        lines += [f"{tmux} new-window -d -t w1 -c {cwd}"] * 7
        lines.append(f"for w in $({tmux} list-windows -t w1 -F '#{{window_id}}'); do "
                     f"{tmux} split-window -d -h -t \"$w\" -c {cwd}; done")
    elif layout == "agents":
        for _ in range(5):
            lines.append(f"{tmux} split-window -d -t w1 -c {cwd}")
            lines.append(f"{tmux} select-layout -t w1 tiled")
    return "\n".join(lines) + "\n"


class LocalTerminal:
    """The app's local tmux server, isolated by TMUX_TMPDIR + socket name."""

    def __init__(self, ctx: "Context"):
        self.ctx = ctx

    def env(self) -> dict:
        return self.ctx.base_env()

    def tmux(self, *args: str, check: bool = True) -> str:
        result = subprocess.run(
            ["tmux", "-L", TMUX_SOCKET, *args], env=self.env(), capture_output=True, text=True
        )
        if check and result.returncode != 0:
            raise RuntimeError(f"tmux {args}: {result.stderr.strip()}")
        return result.stdout

    def server_pid(self) -> int | None:
        try:
            return int(self.tmux("display-message", "-p", "#{pid}", check=False).strip())
        except ValueError:
            return None

    def create_layout(self, layout: str) -> None:
        work = self.ctx.home / "work"
        script = layout_script(layout, shlex.quote(str(work)), f"tmux -L {TMUX_SOCKET}")
        subprocess.run(["bash", "-c", script], env=self.env(), check=True)

    def send(self, text: str, enter: bool = True, pane: str | None = None) -> None:
        target = pane or self.panes()[0]
        self.tmux("send-keys", "-t", target, "-l", text)
        if enter:
            self.tmux("send-keys", "-t", target, "Enter")

    def panes(self) -> list[str]:
        return self.tmux("list-panes", "-s", "-t", "w1", "-F", "#{pane_id}").split()

    def run(self, script: str) -> str:
        return subprocess.run(["bash", "-c", script], env=self.env(), capture_output=True, text=True).stdout

    def kill_server(self) -> None:
        self.tmux("kill-server", check=False)

    def path(self, name: str) -> str:
        return str(self.ctx.data / name)


class RemoteTerminal:
    """The SSH host's tmux server and helper, driven through our own ssh master."""

    def __init__(self, ctx: "Context"):
        self.ctx = ctx

    def ssh(self, command: str, check: bool = True, timeout: float = 120) -> str:
        args = ["ssh", "-F", str(self.ctx.ssh_config), "-o", "BatchMode=yes",
                "-o", "ControlMaster=auto", "-o", f"ControlPath={self.ctx.control_path}",
                "-o", "ControlPersist=900", self.ctx.ssh_target, command]
        result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
        if check and result.returncode != 0:
            raise RuntimeError(f"ssh {command!r}: {result.stderr.strip()}")
        return result.stdout

    def scp(self, sources: list[Path], destination: str) -> None:
        subprocess.run(
            ["scp", "-q", "-F", str(self.ctx.ssh_config), "-o", f"ControlPath={self.ctx.control_path}",
             *map(str, sources), f"{self.ctx.ssh_target}:{destination}"],
            check=True,
        )

    def prepare(self) -> None:
        self.ssh("mkdir -p ~/bench ~/.local/bin")
        self.scp([HERE / "workloads" / name for name in
                  ("remote-sampler.pl", "stream.pl", "colored.pl", "keyprobe.pl")], "bench/")
        bashrc = f"PS1={shlex.quote(PROMPT)}\nHISTFILE=/dev/null\n"
        self.ssh(f"printf %s {shlex.quote(bashrc)} > ~/bench/bashrc; "
                 "grep -q 'bench/bashrc' ~/.bashrc || echo '. ~/bench/bashrc  # app-bench' >> ~/.bashrc; "
                 "test -s ~/bench/colored.txt || perl ~/bench/colored.pl 200000 > ~/bench/colored.txt")
        agents = self.ctx.data / "agents"
        if agents.exists():
            self.ssh("mkdir -p ~/bench/agents/bin ~/.claude ~/.codex")
            self.scp(sorted(p for p in agents.iterdir() if p.is_file()), "bench/agents/")
            self.scp(sorted((agents / "bin").iterdir()), "bench/agents/bin/")
            self.ssh("chmod 755 ~/bench/agents/bin/*")
        helper = self.ssh("sha256sum ~/.local/bin/muxflow-host 2>/dev/null | cut -d' ' -f1", check=False).strip()
        local = subprocess.run(["sha256sum", str(self.ctx.host_binary)], capture_output=True, text=True).stdout.split()[0]
        if helper != local:
            log("installing the local muxflow-host build on the SSH host (~/.local/bin, as the app's installer does)")
            self.scp([self.ctx.host_binary], ".local/bin/muxflow-host.bench-new")
            self.ssh("chmod 755 ~/.local/bin/muxflow-host.bench-new && mv ~/.local/bin/muxflow-host.bench-new ~/.local/bin/muxflow-host")
        if agents.exists():
            # The bench account's own agent configs: the helper's hooks, as the app's setup would add.
            self.ssh("~/.local/bin/muxflow-host hook install --yes >/dev/null")
        self.ssh(f"test -s ~/big.txt || ({files_fixture_script('~')})")

    def reset(self, layout: str | None) -> None:
        # Only the bench sessions, the bench user's helper daemon and our sampler.
        self.ssh("tmux kill-session -t w1 2>/dev/null; ~/.local/bin/muxflow-host daemon-stop >/dev/null 2>&1; "
                 "pkill -u $(id -u) -f remote-sampler.pl; rm -f ~/bench/stop; true", check=False)
        if layout:
            self.create_layout(layout)

    def create_layout(self, layout: str) -> None:
        self.ssh(layout_script(layout, "~", "tmux"))

    def send(self, text: str, enter: bool = True, pane: str | None = None) -> None:
        target = shlex.quote(pane) if pane else "$(tmux list-panes -s -t w1 -F '#{pane_id}' | head -1)"
        command = f"tmux send-keys -t {target} -l {shlex.quote(text)}"
        if enter:
            command += f" && tmux send-keys -t {target} Enter"
        self.ssh(command)

    def panes(self) -> list[str]:
        return self.ssh("tmux list-panes -s -t w1 -F '#{pane_id}'").split()

    def run(self, script: str) -> str:
        return self.ssh(script)

    def path(self, name: str) -> str:
        return {"colored.txt": "~/bench/colored.txt", "stream.pl": "~/bench/stream.pl",
                "keyprobe.pl": "~/bench/keyprobe.pl"}[name]

    # remote sampler
    def sampler_start(self, tag: str, interval: float) -> None:
        self.ssh(f"rm -f ~/bench/stop; nohup perl ~/bench/remote-sampler.pl ~/bench/s-{tag}.txt "
                 f"~/bench/stop {interval} >/dev/null 2>&1 &")

    def sampler_stop(self, tag: str) -> str:
        return self.ssh(f"touch ~/bench/stop; sleep 1.2; cat ~/bench/s-{tag}.txt; rm -f ~/bench/s-{tag}.txt ~/bench/stop")


def remote_ticks(text: str) -> list[dict]:
    """Parses remote-sampler.pl output into sampler ticks with remote categories."""
    blocks: list[tuple[float, dict[int, bl.Proc]]] = []
    for line in text.splitlines():
        if line.startswith("T "):
            blocks.append((float(line.split()[1]), {}))
        elif blocks and line.strip():
            pid, ppid, comm, self_ticks, child_ticks, start, rss, pss = line.split()
            blocks[-1][1][int(pid)] = bl.Proc(int(pid), int(ppid), comm, int(self_ticks), int(child_ticks),
                                              int(start), int(rss), int(pss) if int(pss) >= 0 else None)
    accountant = bl.CpuAccountant(window_start_ticks=0)
    ticks = []
    for stamp, table in blocks:
        children = bl.children_map(table)
        cats: dict[int, str] = {}
        for pid, proc in table.items():
            if proc.comm.startswith("tmux:_server"):
                cats[pid] = "tmux"
                for child in bl.descendants(pid, children)[1:]:
                    cats[child] = "tmux" if table[child].comm.startswith("tmux") else "panes"
        for pid, proc in table.items():
            if pid in cats:
                continue
            if proc.comm.startswith("muxflow-host"):
                cats[pid] = "host"
            elif proc.comm.startswith("tmux"):
                cats[pid] = "tmux"
            elif proc.comm.startswith("sshd"):
                cats[pid] = "sshd"
        cpu = accountant.update(table, cats)
        pss: dict[str, int] = {}
        rss: dict[str, int] = {}
        for pid, cat in cats.items():
            if cat in ("host", "tmux"):
                rss[cat] = rss.get(cat, 0) + table[pid].rss_kb
                pss[cat] = pss.get(cat, 0) + (table[pid].pss_kb or 0)
        ticks.append({"t": stamp, "cpu": dict(cpu), "pss": pss, "rss": rss})
    return ticks


# ---------------------------------------------------------------- sampler


class Sampler(threading.Thread):
    """Samples the app tree, its host helpers, tmux, ssh and the compositor."""

    MEMORY_CATEGORIES = ("app", "host", "tmux", "ssh")
    GPU_CATEGORIES = ("app", "compositor")

    def __init__(self, ctx: "Context", app_pid: int, interval: float):
        super().__init__(daemon=True)
        self.ctx = ctx
        self.app_pid = app_pid
        self.interval = interval
        self.ticks: list[dict] = []
        self.stop_event = threading.Event()
        self.cmdlines: dict[tuple[int, int], str] = {}
        self.tmux_pid = ctx.terminal_server_pid()
        self.window_start = bl.boot_ticks_now()

    def cmdline(self, proc: bl.Proc) -> str:
        key = (proc.pid, proc.start)
        if key not in self.cmdlines:
            try:
                with open(f"/proc/{proc.pid}/cmdline", "rb") as handle:
                    self.cmdlines[key] = handle.read().replace(b"\0", b" ").decode(errors="replace")
            except OSError:
                self.cmdlines[key] = ""
        return self.cmdlines[key]

    def classify(self, table: dict[int, bl.Proc]) -> dict[int, str]:
        children = bl.children_map(table)
        cats: dict[int, str] = {}

        def add_tree(root: int, default: str) -> None:
            for pid in bl.descendants(root, children):
                if pid in cats or pid not in table:
                    continue
                comm = table[pid].comm
                if comm.startswith("muxflow-host"):
                    cats[pid] = "host"
                elif comm == "ssh":
                    cats[pid] = "ssh"
                elif comm.startswith("tmux"):
                    cats[pid] = "tmux"
                else:
                    cats[pid] = default

        if self.tmux_pid and self.tmux_pid in table:
            cats[self.tmux_pid] = "tmux"
            for pid in bl.descendants(self.tmux_pid, children)[1:]:
                cats[pid] = "tmux" if table[pid].comm.startswith("tmux") else "panes"
        if self.app_pid in table:
            add_tree(self.app_pid, "app")
        runtime_marker = str(self.ctx.runtime) + "/"
        for pid, proc in table.items():
            if pid in cats:
                continue
            if proc.comm.startswith("muxflow-host") and runtime_marker in self.cmdline(proc):
                add_tree(pid, "host")
            elif proc.comm == "ssh" and "/muxflow-" in self.cmdline(proc) and "/ssh-" in self.cmdline(proc):
                cats[pid] = "ssh"  # the app's control master, reparented after it daemonized
        if self.ctx.compositor.pid in table:
            cats[self.ctx.compositor.pid] = "compositor"
        return cats

    def run(self) -> None:
        cpu_accountant = bl.CpuAccountant(window_start_ticks=self.window_start)
        gpu_accountant = bl.GpuAccountant()
        next_due = time.monotonic()
        while True:
            stamp = time.monotonic()
            table = bl.proc_table()
            cats = self.classify(table)
            cpu = cpu_accountant.update(table, cats)
            pss: dict[str, int] = {}
            rss: dict[str, int] = {}
            procs = []
            gpu_samples: dict[tuple[int, int, str], dict[str, int]] = {}
            for pid, cat in cats.items():
                if cat in self.MEMORY_CATEGORIES:
                    rollup = bl.smaps_rollup(pid)
                    if rollup:
                        pss[cat] = pss.get(cat, 0) + rollup[0]
                        rss[cat] = rss.get(cat, 0) + rollup[1]
                        procs.append([pid, table[pid].comm, cat, rollup[0], table[pid].self_ticks])
                if cat in self.GPU_CATEGORIES:
                    for client, values in bl.drm_clients(pid).items():
                        gpu_samples[(pid, table[pid].start, client)] = values
            gpu_ns, vram = gpu_accountant.update(gpu_samples, cats)
            self.ticks.append({"t": stamp, "cpu": dict(cpu), "gpu_ns": dict(gpu_ns), "pss": pss,
                               "rss": rss, "vram": dict(vram), "procs": procs})
            if self.stop_event.is_set():
                return
            next_due += self.interval
            self.stop_event.wait(max(0.0, next_due - time.monotonic()))

    def finish(self) -> list[dict]:
        self.stop_event.set()
        self.join(30)
        return self.ticks


# ---------------------------------------------------------------- context


class Context:
    def __init__(self, args: argparse.Namespace):
        self.root = Path(args.root).resolve()
        self.repo = Path(args.repo).resolve()
        self.results = Path(args.results).resolve()
        self.home = self.root / "h"
        self.runtime = self.root / "r"
        self.tmux_tmpdir = self.root / "t"
        self.data = self.root / "data"
        self.ssh_config = Path(args.ssh_config).resolve() if args.ssh_config else None
        self.ssh_target = args.ssh_target
        self.control_path = f"{os.environ.get('XDG_RUNTIME_DIR', '/tmp')}/app-bench-cm"
        self.target_dir = self.repo / "target" / "release"
        self.host_binary = self.target_dir / "muxflow-host"
        self.compositor = Compositor(self.root, args.compositor, os.environ.get("HYPRLAND_INSTANCE_SIGNATURE"))
        self.host = "local"
        self.local = LocalTerminal(self)
        self.remote = RemoteTerminal(self) if self.ssh_config else None
        self.app_proc: subprocess.Popen | None = None
        self.app_name = ""
        self.settle = args.settle
        self.window = args.window
        self.typing_keys = args.typing_keys
        self.seq_count = args.seq
        sock = self.tmux_tmpdir / f"tmux-{os.getuid()}" / TMUX_SOCKET
        host_sock = self.runtime / "host.sock"
        for path in (sock, host_sock):
            if len(str(path)) > 107:
                raise SystemExit(f"socket path too long ({len(str(path))} > 107): {path}; use a shorter --root")

    def base_env(self) -> dict:
        return {
            "HOME": str(self.home),
            "USER": os.environ.get("USER", "bench"),
            "LOGNAME": os.environ.get("USER", "bench"),
            "LANG": "en_US.UTF-8",
            "SHELL": "/bin/bash",
            "PATH": "/usr/local/bin:/usr/bin:/bin",
            "XDG_RUNTIME_DIR": os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"),
            "ADE_HOST_RUNTIME_DIR": str(self.runtime),
            "TMUX_TMPDIR": str(self.tmux_tmpdir),
            "ADE_TMUX_SOCKET_NAME": TMUX_SOCKET,
        }

    def app_env(self) -> dict:
        env = self.base_env()
        env.update({
            "WAYLAND_DISPLAY": self.compositor.wayland_display,
            "XDG_SESSION_TYPE": "wayland",
        })
        if "DBUS_SESSION_BUS_ADDRESS" in os.environ:
            env["DBUS_SESSION_BUS_ADDRESS"] = os.environ["DBUS_SESSION_BUS_ADDRESS"]
        return env

    @property
    def terminal(self):
        return self.remote if self.host == "ssh" else self.local

    def terminal_server_pid(self) -> int | None:
        return self.local.server_pid() if self.host == "local" else None

    # state -----------------------------------------------------------------

    def stop_host_side(self) -> None:
        subprocess.run([str(self.host_binary), "daemon-stop"], env=self.base_env(), capture_output=True)
        self.local.kill_server()
        # Anything still holding our runtime dir (a daemon that ignored the stop).
        for pid, proc in bl.proc_table().items():
            if proc.comm.startswith("muxflow-host"):
                try:
                    cmdline = Path(f"/proc/{pid}/cmdline").read_bytes()
                except OSError:
                    continue
                if str(self.runtime).encode() in cmdline:
                    os.kill(pid, signal.SIGKILL)

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
            self.remote.reset(layout)
        elif layout:
            self.local.create_layout(layout)

    def seed_config(self) -> None:
        config = self.home / ".config" / "dev.muxflow.desktop"
        config.mkdir(parents=True, exist_ok=True)
        x, y, w, h = WINDOW
        state = {
            "schemaVersion": 1, "appTabs": [], "workspaceUi": [],
            "shell": {
                "panelSurface": "files", "enabledPanelExtensions": None, "sidebarCollapsed": False,
                "sidebarWidth": None, "panelOpen": False, "panelWidth": None, "dockFraction": None,
                "agentSort": "workspace", "agentsSectionRatio": None, "agentStateGlyphs": False,
                "compactWorkspaces": None, "tabsInTitleBar": None, "focusFollowsMouse": False,
                "pinnedOnly": False, "terminalScreenReader": False, "copyOnSelect": None,
                "cleanWrappedCommands": None, "terminalApplicationClipboard": None, "terminalFontSize": None,
                "theme": None, "uiScale": None, "markdownAppearance": None, "agentSounds": None,
                "newWorkspaceHostProfileId": None, "defaultMarkdownView": None,
                "windowGeometry": {"x": x, "y": y, "width": w, "height": h, "maximized": False,
                                   "scaleFactorMilli": 1000},
            },
            "commands": {"shortcutOverrides": {}},
            "hostSetup": {"local": "declined", "bench": "declined"},
            "workspaceDefaults": {},
        }
        (config / "app-state.json").write_text(json.dumps(state, indent=2))
        if self.host == "ssh":
            self.seed_profiles()

    def seed_profiles(self) -> None:
        config = self.home / ".config" / "dev.muxflow.desktop"
        config.mkdir(parents=True, exist_ok=True)
        profiles = {
            "schemaVersion": 1,
            "profiles": [
                {"id": "local", "label": "Local", "connection": {"mode": "local"}, "shown": False},
                {"id": "bench", "label": "Bench", "connection": {
                    "mode": "ssh", "profileId": "bench", "target": self.ssh_target,
                    "configPath": str(self.ssh_config)}, "shown": True},
            ],
            "lastProfileId": "bench",
        }
        path = config / "profiles.json"
        path.write_text(json.dumps(profiles, indent=2))
        os.chmod(path, 0o600)

    # app -------------------------------------------------------------------

    def launch(self, app: str) -> float:
        binary = self.target_dir / APPS[app]["binary"]
        self.app_name = app
        log_path = self.results / "logs" / f"{app}-{self.host}.log"
        log_path.parent.mkdir(parents=True, exist_ok=True)
        t0 = time.monotonic()
        self.app_proc = subprocess.Popen(
            [str(binary)], env=self.app_env(), cwd=str(self.home),
            stdout=open(log_path, "a"), stderr=subprocess.STDOUT, start_new_session=True,
        )
        return t0

    def stop_app(self) -> None:
        if self.app_proc is None:
            return
        pid = self.app_proc.pid
        self.app_proc.terminate()
        try:
            self.app_proc.wait(10)
        except subprocess.TimeoutExpired:
            self.app_proc.kill()
            self.app_proc.wait(5)
        # Leftovers in the app's process group (WebKit helpers, bridges), but never
        # the host daemon: a real quit leaves it running for the next launch.
        time.sleep(0.5)
        for other, proc in bl.proc_table().items():
            try:
                if os.getpgid(other) != pid:
                    continue
            except OSError:
                continue
            cmdline = Path(f"/proc/{other}/cmdline").read_bytes() if os.path.exists(f"/proc/{other}") else b""
            if b" daemon" in cmdline.replace(b"\0", b" "):
                continue
            try:
                os.kill(other, signal.SIGKILL)
            except OSError:
                pass
        self.app_proc = None

    def wait_content(self, t0: float, color=MAGENTA, threshold: int = 300, timeout: float = 60,
                     region=WINDOW) -> float | None:
        deadline = t0 + timeout
        while time.monotonic() < deadline:
            try:
                image = self.compositor.grab(region)
            except subprocess.CalledProcessError:
                time.sleep(0.02)
                continue
            stamp = time.monotonic()
            if count_color(image, color) >= threshold:
                return stamp
        return None


# ---------------------------------------------------------------- scenarios


def save(ctx: Context, scenario: str, app: str, run: int, result: dict, ticks: list | None = None,
         remote: list | None = None) -> dict:
    out = ctx.results / ctx.host / scenario
    out.mkdir(parents=True, exist_ok=True)
    result.update({"app": app, "host": ctx.host, "scenario": scenario, "run": run})
    (out / f"{app}-run{run}.json").write_text(json.dumps(result, indent=2))
    if ticks is not None:
        bl.write_jsonl(str(out / f"{app}-run{run}.ticks.jsonl"), ticks)
    if remote is not None:
        bl.write_jsonl(str(out / f"{app}-run{run}.remote.jsonl"), remote)
    brief = {k: result.get(k) for k in ("metrics",) if k in result}
    cpu = result.get("cpu_pct", {})
    log(f"  {ctx.host}/{scenario}/{app}#{run}: {json.dumps(brief)} cpu%={json.dumps(cpu)}")
    return result


def sample_window(ctx: Context, seconds: float, interval: float, tag: str, action=None) -> tuple[dict, list, list | None]:
    sampler = Sampler(ctx, ctx.app_proc.pid if ctx.app_proc else -1, interval)
    if ctx.host == "ssh":
        ctx.remote.sampler_start(tag, 1.0)
    sampler.start()
    extra = {}
    if action is not None:
        extra = action() or {}
    else:
        time.sleep(seconds)
    ticks = sampler.finish()
    remote = None
    result = bl.summarize_ticks(ticks)
    if ctx.host == "ssh":
        remote = remote_ticks(ctx.remote.sampler_stop(tag))
        result["remote"] = bl.summarize_ticks(remote)
    if extra:
        result["metrics"] = extra
    return result, ticks, remote


def startup(ctx: Context, app: str, run: int, kind: str) -> bool:
    t0 = ctx.launch(app)
    sampler = Sampler(ctx, ctx.app_proc.pid, 0.25)
    sampler.start()
    content = ctx.wait_content(t0)
    mapped = ctx.compositor.open_window_after(t0, APPS[app]["class"])
    if mapped is None:
        for client in ctx.compositor.clients():
            if client.get("pid") == ctx.app_proc.pid:
                mapped = time.monotonic()  # fallback: late, only when the event was missed
    time.sleep(2.0)
    ticks = sampler.finish()
    result = bl.summarize_ticks(ticks)
    result["metrics"] = {
        "window_map_ms": round((mapped - t0) * 1000, 1) if mapped else None,
        "first_content_ms": round((content - t0) * 1000, 1) if content else None,
    }
    geometry = [c for c in ctx.compositor.clients() if c.get("pid") == ctx.app_proc.pid]
    result["window"] = {"at": geometry[0]["at"], "size": geometry[0]["size"]} if geometry else None
    if mapped is None:
        result["events_debug"] = [f"{stamp - t0:.3f} {line}" for stamp, line in ctx.compositor.events[-30:]]
    if content is None:
        (ctx.results / ctx.host).mkdir(parents=True, exist_ok=True)
        ctx.compositor.save_png(ctx.results / ctx.host / f"startup-{kind}-{app}-run{run}-timeout.png")
    save(ctx, f"startup-{kind}", app, run, result, ticks)
    return content is not None


def idle(ctx: Context, app: str, run: int, scenario: str) -> None:
    time.sleep(ctx.settle)
    result, ticks, remote = sample_window(ctx, ctx.window, 1.0, f"{scenario}-{app}-{run}")
    save(ctx, scenario, app, run, result, ticks, remote)


def heavy(ctx: Context, app: str, run: int, kind: str) -> None:
    terminal = ctx.terminal
    terminal.send("clear")
    time.sleep(1.5)
    if kind == "seq":
        command = f"seq 1 {ctx.seq_count}; {SENTINEL}"
    else:
        command = f"cat {terminal.path('colored.txt')}; {SENTINEL}"

    def action() -> dict:
        t0 = time.monotonic()
        terminal.send(command)
        sent = time.monotonic()
        done = None
        while time.monotonic() - t0 < 900:
            image = ctx.compositor.grab()
            if has_green_bar(image):
                done = time.monotonic()
                break
            time.sleep(0.05)
        time.sleep(3.0)  # trailing work after the last frame
        return {"wall_s": round(done - t0, 3) if done else None, "send_ms": round((sent - t0) * 1000, 1)}

    result, ticks, remote = sample_window(ctx, 0, 0.5, f"heavy-{kind}-{app}-{run}", action)
    if result.get("metrics", {}).get("wall_s") is None:
        ctx.compositor.save_png(ctx.results / ctx.host / f"heavy-{kind}-{app}-run{run}-timeout.png")
    save(ctx, f"heavy-{kind}", app, run, result, ticks, remote)
    terminal.send("clear")
    time.sleep(2.0)


def stream(ctx: Context, app: str, run: int) -> None:
    time.sleep(ctx.settle)
    terminal = ctx.terminal

    def action() -> dict:
        terminal.send(f"clear; perl {terminal.path('stream.pl')} {ctx.window} 60")
        time.sleep(ctx.window + 2)
        return {}

    result, ticks, remote = sample_window(ctx, 0, 1.0, f"stream-{app}-{run}", action)
    result.pop("metrics", None)
    save(ctx, "stream", app, run, result, ticks, remote)


def typing(ctx: Context, app: str, run: int) -> None:
    terminal = ctx.terminal
    terminal.send(f"clear; perl {terminal.path('keyprobe.pl')}")
    time.sleep(2.0)
    x, y, w, h = WINDOW
    region = (x + 250, y, w - 250, 260)  # terminal area, right of the sidebar
    base = count_color(ctx.compositor.grab(region), BLUE)
    # Calibration and focus check: three keystrokes must add three blue cells.
    for _ in range(3):
        ctx.compositor.type_keys(["x"]).wait()
        time.sleep(0.2)
    time.sleep(2.0)
    cell = (count_color(ctx.compositor.grab(region), BLUE) - base) / 3
    if not 40 <= cell <= 600:
        (ctx.results / ctx.host).mkdir(parents=True, exist_ok=True)
        ctx.compositor.save_png(ctx.results / ctx.host / f"typing-{app}-run{run}-calibration.png")
        save(ctx, "typing", app, run, {"metrics": {"error": f"calibration failed: {cell:.0f} px per key"}})
        terminal.send("q", enter=False)
        return
    count = count_color(ctx.compositor.grab(region), BLUE)
    rng = random.Random(1000 + run)
    latencies, misses, grab_times = [], 0, []
    for _ in range(ctx.typing_keys):
        time.sleep(rng.uniform(0.25, 0.4))
        before = count
        sent = time.monotonic()
        typer = ctx.compositor.type_keys(["x"])
        seen = None
        while time.monotonic() - sent < 3.0:
            start = time.monotonic()
            count = count_color(ctx.compositor.grab(region), BLUE)
            grab_times.append(time.monotonic() - start)
            if count >= before + cell * 0.5:
                seen = time.monotonic()
                break
        typer.wait()
        if seen is None:
            misses += 1
        else:
            latencies.append((seen - sent) * 1000)
    grab_ms = statistics.median(grab_times) * 1000 if grab_times else None
    final = count
    sends = range(ctx.typing_keys)
    result = {"metrics": {
        "keys": len(sends), "missed": misses, "cell_px": cell, "blue_px_final": final,
        "latency_ms_median": round(statistics.median(latencies), 1) if latencies else None,
        "latency_ms_p95": round(bl.percentile(latencies, 0.95), 1) if latencies else None,
        "latency_ms_min": round(min(latencies), 1) if latencies else None,
        "capture_interval_ms": round(grab_ms, 1) if grab_ms else None,
        "latencies_ms": [round(v, 1) for v in latencies],
    }}
    terminal.send("q", enter=False)
    time.sleep(0.5)
    terminal.send("clear")
    save(ctx, "typing", app, run, result)


AGENT_NAMES = ["claude"] * 3 + ["codex"] * 3
ADAPTERS = {"claude": "claude-code", "codex": "codex"}


def tmux_line(ctx: Context, args: str) -> str:
    if ctx.host == "ssh":
        return ctx.remote.ssh(f"tmux {args}")
    return ctx.local.run(f"tmux -L {TMUX_SOCKET} {args}")


def agent_paths(ctx: Context) -> tuple[str, str]:
    if ctx.host == "ssh":
        return "~/bench/agents", "~/.local/bin/muxflow-host"
    return str(ctx.data / "agents"), str(ctx.host_binary)


def start_agents(ctx: Context, mode: str) -> list[str]:
    """Respawns the six agent panes with the replay binary (as claude/codex)."""
    root, _ = agent_paths(ctx)
    panes = ctx.terminal.panes()[:6]
    for index, pane in enumerate(panes):
        name = AGENT_NAMES[index]
        extra = "--idle" if mode == "idle" else f"--offset {5 + 17 * index}"
        tmux_line(ctx, f"respawn-pane -k -t {pane} {shlex.quote(f'exec {root}/bin/{name} {root}/{name} {extra}')}")
    return panes


def agent_hook(ctx: Context, pane: str, index: int, event: str) -> None:
    """Fires the agent's real hook command from the pane's own tmux server, as an
    agent's hook would: under the tmux server, not under this harness."""
    _, host = agent_paths(ctx)
    payload = json.dumps({"hook_event_name": event, "session_id": f"app-bench-{index}"})
    inner = (f"printf '%s' {shlex.quote(payload)} | TMUX='#{{socket_path}},#{{pid}},#{{session_id}}' "
             f"TMUX_PANE='#{{pane_id}}' {host} hook ingest --adapter {ADAPTERS[AGENT_NAMES[index]]}")
    tmux_line(ctx, f"run-shell -t {pane} {shlex.quote(inner)}")


def install_local_hooks(ctx: Context) -> None:
    for name in (".claude", ".codex"):
        (ctx.home / name).mkdir(exist_ok=True)
    subprocess.run([str(ctx.host_binary), "hook", "install", "--home", str(ctx.home), "--yes"],
                   env=ctx.base_env(), capture_output=True, check=True)


def block_agents(ctx: Context, app: str, run: int, scenarios: set[str]) -> None:
    if not scenarios & {"agents", "spinner", "still"}:
        return
    log(f"{ctx.host} {app} run {run}: agents block")
    ctx.reset("agents", seeded=True)
    if ctx.host == "local":
        install_local_hooks(ctx)
    t0 = ctx.launch(app)
    if ctx.wait_content(t0) is None:
        log("  no content within 60 s")
    evidence = ctx.results / ctx.host / "evidence"
    evidence.mkdir(parents=True, exist_ok=True)

    if "agents" in scenarios:
        panes = start_agents(ctx, "replay")
        time.sleep(1.0)
        for index, pane in enumerate(panes):
            agent_hook(ctx, pane, index, "UserPromptSubmit")
        time.sleep(ctx.settle)
        ctx.compositor.save_png(evidence / f"agents-replay-{app}-run{run}.png")

        def action() -> dict:
            started = time.monotonic()
            done = 0
            while time.monotonic() - started < ctx.window:
                time.sleep(min(7.0, max(0.0, ctx.window - (time.monotonic() - started))))
                if time.monotonic() - started > ctx.window - 3:
                    break
                index = done % len(panes)
                agent_hook(ctx, panes[index], index, "Stop")  # the row shows done
                time.sleep(3.0)
                agent_hook(ctx, panes[index], index, "UserPromptSubmit")  # and works again
                done += 1
            remaining = ctx.window - (time.monotonic() - started)
            if remaining > 0:
                time.sleep(remaining)
            return {"done_events": done}

        result, ticks, remote = sample_window(ctx, 0, 1.0, f"agents-{app}-{run}", action)
        save(ctx, "agents-replay", app, run, result, ticks, remote)

    if "spinner" in scenarios:
        panes = start_agents(ctx, "idle")
        time.sleep(2.0)
        for index, pane in enumerate(panes):
            agent_hook(ctx, pane, index, "UserPromptSubmit")
        time.sleep(ctx.settle)
        ctx.compositor.save_png(evidence / f"spinner-{app}-run{run}.png")
        result, ticks, remote = sample_window(ctx, ctx.window, 1.0, f"spin-v-{app}-{run}")
        save(ctx, "spinner-visible", app, run, result, ticks, remote)
        ctx.compositor.show_workspace(2)
        time.sleep(5.0)
        result, ticks, remote = sample_window(ctx, ctx.window, 1.0, f"spin-h-{app}-{run}")
        save(ctx, "spinner-hidden", app, run, result, ticks, remote)
        ctx.compositor.show_workspace(1)
    if "still" in scenarios:
        # The spinner's baseline: the same six rows and silent panes, driven
        # working and then done through the same hooks, so only the motion goes.
        panes = start_agents(ctx, "idle")
        time.sleep(2.0)
        for index, pane in enumerate(panes):
            agent_hook(ctx, pane, index, "UserPromptSubmit")
        time.sleep(1.0)
        for index, pane in enumerate(panes):
            agent_hook(ctx, pane, index, "Stop")
        time.sleep(ctx.settle)
        ctx.compositor.save_png(evidence / f"still-{app}-run{run}.png")
        result, ticks, remote = sample_window(ctx, ctx.window, 1.0, f"still-{app}-{run}")
        save(ctx, "spinner-still", app, run, result, ticks, remote)
    ctx.stop_app()


def keys(ctx: Context, args: list[str]) -> None:
    ctx.compositor.type_keys(args).wait()


def files_fixture_script(root: str) -> str:
    """A 5 MB text file and a 10,000-entry directory in the workspace's directory."""
    return (f"cd {root} && mkdir -p 0-many && "
            f"(cd 0-many && seq -f 'entry-%05g.txt' 1 10000 | xargs touch) && "
            f"perl -e 'for $i (1..80000) {{ printf \"%06d the quick brown fox jumps over the lazy dog %s\\n\", "
            f"$i, \"x\" x 8 }}' > big.txt && wc -c big.txt")


def block_files(ctx: Context, app: str, run: int, scenarios: set[str]) -> None:
    """Editor (open a 5 MB file, page through it) and Explorer (expand 10k entries, scroll)."""
    if not scenarios & {"editor", "explorer"}:
        return
    log(f"{ctx.host} {app} run {run}: editor/explorer block")
    evidence = ctx.results / ctx.host / "evidence"
    evidence.mkdir(parents=True, exist_ok=True)
    ctx.reset("single", seeded=True)
    if ctx.host == "local":
        ctx.local.run(files_fixture_script(shlex.quote(str(ctx.home / "work"))))
    for scenario in ("editor", "explorer"):
        if scenario not in scenarios:
            continue
        ctx.seed_config()  # forget tabs the previous launch restored
        t0 = ctx.launch(app)
        ctx.wait_content(t0)
        time.sleep(ctx.settle)

        def editor() -> dict:
            keys(ctx, ["-M", "ctrl", "p", "-m", "ctrl"])
            time.sleep(1.0)
            keys(ctx, ["big.txt"])
            time.sleep(1.5)
            ctx.compositor.save_png(evidence / f"editor-quickopen-{app}-run{run}.png")
            keys(ctx, ["-k", "Return"])
            time.sleep(6.0)
            ctx.compositor.save_png(evidence / f"editor-open-{app}-run{run}.png")
            # No scrolling: PageDown reaches GPUI's editor but not Tauri's (its
            # editor does not take keyboard focus on open), and there is no
            # pointer injection on this compositor, so a scroll would not be
            # the same action in both apps.
            return {"opened": "big.txt"}

        def explorer() -> dict:  # not run by default: keys do not reach either app's tree
            keys(ctx, ["-M", "ctrl", "-M", "shift", "e", "-m", "shift", "-m", "ctrl"])
            time.sleep(2.0)
            keys(ctx, ["-k", "Down"])
            time.sleep(0.5)
            keys(ctx, ["-k", "Right"])
            time.sleep(4.0)
            ctx.compositor.save_png(evidence / f"explorer-expanded-{app}-run{run}.png")
            args = []
            for _ in range(300):
                args += ["-k", "Down", "-s", "20"]
            keys(ctx, args)
            time.sleep(2.0)
            ctx.compositor.save_png(evidence / f"explorer-scrolled-{app}-run{run}.png")
            return {"down_presses": 300}

        result, ticks, remote = sample_window(ctx, 0, 0.5, f"{scenario}-{app}-{run}",
                                              editor if scenario == "editor" else explorer)
        save(ctx, scenario, app, run, result, ticks, remote)
        ctx.stop_app()


def compositor_baseline(ctx: Context) -> None:
    ctx.host = "local"
    result, ticks, _ = sample_window(ctx, ctx.window, 1.0, "baseline")
    save(ctx, "compositor-baseline", "none", 1, result, ticks)


def block_single(ctx: Context, app: str, run: int, scenarios: set[str]) -> None:
    """Cold start, then warm start and the single-pane scenarios on the warm app."""
    if not scenarios & {"startup", "idle", "heavy", "typing"}:
        return
    log(f"{ctx.host} {app} run {run}: single-pane block")
    if "startup" in scenarios:
        ctx.reset("single", seeded=ctx.host == "ssh" and False)
        if ctx.host == "ssh":
            ctx.seed_profiles()
        startup(ctx, app, run, "cold")
        ctx.stop_app()
    else:
        ctx.reset("single", seeded=False)
    # Warm: the daemon and tmux server from the previous launch are still up.
    ctx.seed_config()
    if "startup" in scenarios:
        startup(ctx, app, run, "warm")
    else:
        t0 = ctx.launch(app)
        ctx.wait_content(t0)
    if "idle" in scenarios:
        idle(ctx, app, run, "idle-1x1")
    if "heavy" in scenarios:
        heavy(ctx, app, run, "seq")
        heavy(ctx, app, run, "cat")
    if "typing" in scenarios:
        typing(ctx, app, run)
    ctx.stop_app()


def block_large(ctx: Context, app: str, run: int, scenarios: set[str]) -> None:
    if not scenarios & {"idle", "stream"}:
        return
    log(f"{ctx.host} {app} run {run}: 8 tabs x 2 panes block")
    ctx.reset("large", seeded=True)
    t0 = ctx.launch(app)
    if ctx.wait_content(t0) is None:
        log("  no content within 60 s")
    if "idle" in scenarios:
        idle(ctx, app, run, "idle-8x2")
    if "stream" in scenarios:
        stream(ctx, app, run)
    ctx.stop_app()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, help="scratch root (HOME, runtime, tmux dirs live here)")
    parser.add_argument("--repo", required=True)
    parser.add_argument("--results", required=True)
    parser.add_argument("--apps", default="tauri,gpui")
    parser.add_argument("--hosts", default="local,ssh")
    parser.add_argument("--scenarios", default="startup,idle,heavy,typing,stream,agents,spinner,editor,baseline")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--window", type=float, default=60.0, help="seconds per steady-state window")
    parser.add_argument("--settle", type=float, default=15.0)
    parser.add_argument("--seq", type=int, default=3_000_000)
    parser.add_argument("--typing-keys", type=int, default=40)
    parser.add_argument("--recordings", help="dir with claude/codex .data+.timing and bin/agent-replay")
    parser.add_argument("--ssh-config")
    parser.add_argument("--ssh-target", default="muxflow-bench")
    parser.add_argument("--compositor", choices=("private", "session"), default="private")
    args = parser.parse_args()

    ctx = Context(args)
    ctx.results.mkdir(parents=True, exist_ok=True)
    ctx.data.mkdir(parents=True, exist_ok=True)
    colored = ctx.data / "colored.txt"
    if not colored.exists():
        with open(colored, "w") as handle:
            subprocess.run(["perl", str(HERE / "workloads" / "colored.pl"), "200000"], stdout=handle, check=True)
    for name in ("stream.pl", "keyprobe.pl"):
        shutil.copy(HERE / "workloads" / name, ctx.data / name)
    if not args.recordings and set(args.scenarios.split(",")) & {"agents", "spinner", "still"}:
        log("no --recordings: skipping the agents and spinner scenarios")
        args.scenarios = ",".join(s for s in args.scenarios.split(",") if s not in ("agents", "spinner", "still"))
    if set(args.scenarios.split(",")) & {"agents", "spinner", "still"}:
        # Recordings stay outside the repository: they hold local paths and names.
        recordings = Path(args.recordings).resolve()
        agents = ctx.data / "agents"
        (agents / "bin").mkdir(parents=True, exist_ok=True)
        for name in ("claude", "codex"):
            for suffix in (".data", ".timing"):
                shutil.copy(recordings / f"{name}{suffix}", agents / f"{name}{suffix}")
            shutil.copy(recordings / "bin" / "agent-replay", agents / "bin" / name)
            os.chmod(agents / "bin" / name, 0o755)
    apps = args.apps.split(",")
    hosts = args.hosts.split(",")
    scenarios = set(args.scenarios.split(","))
    meta = {
        "started": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
        "apps": apps, "hosts": hosts, "scenarios": sorted(scenarios), "runs": args.runs,
        "window_s": args.window, "settle_s": args.settle, "seq": args.seq, "compositor": args.compositor,
        "binaries": {app: {"path": str(ctx.target_dir / APPS[app]["binary"]),
                           "sha256": subprocess.run(["sha256sum", str(ctx.target_dir / APPS[app]["binary"])],
                                                    capture_output=True, text=True).stdout.split()[0]}
                     for app in apps},
        "host_sha256": subprocess.run(["sha256sum", str(ctx.host_binary)], capture_output=True, text=True).stdout.split()[0],
    }
    meta_path = ctx.results / "meta.json"
    if meta_path.exists():  # a second pass (e.g. ssh after local) into the same results
        previous = json.loads(meta_path.read_text())
        meta["hosts"] = sorted(set(previous.get("hosts", [])) | set(hosts))
        meta["passes"] = previous.get("passes", [previous.get("started")]) + [meta["started"]]
        meta["started"] = previous.get("started")
    meta_path.write_text(json.dumps(meta, indent=2))

    def interrupted(*_):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, interrupted)
    try:
        ctx.compositor.start()
        if "baseline" in scenarios:
            compositor_baseline(ctx)
        for host in hosts:
            ctx.host = host
            if host == "ssh":
                if ctx.remote is None:
                    raise SystemExit("--ssh-config is required for the ssh host")
                ctx.remote.prepare()
            for run in range(1, args.runs + 1):
                order = apps if run % 2 else list(reversed(apps))  # alternate who goes first
                for app in order:
                    block_single(ctx, app, run, scenarios)
                for app in order:
                    block_large(ctx, app, run, scenarios)
                for app in order:
                    block_agents(ctx, app, run, scenarios)
                for app in order:
                    block_files(ctx, app, run, scenarios)
            if host == "ssh":
                ctx.remote.reset(None)
    finally:
        ctx.stop_app()
        ctx.host = "local"
        ctx.stop_host_side()
        if ctx.remote is not None:
            ctx.remote.reset(None)
            subprocess.run(["ssh", "-F", str(ctx.ssh_config), "-o", f"ControlPath={ctx.control_path}",
                            "-O", "exit", ctx.ssh_target], capture_output=True)
        ctx.compositor.stop()
        log("stopped the app, its helper daemon, the bench tmux server, the remote sessions and the compositor")
    return 0


if __name__ == "__main__":
    sys.exit(main())
