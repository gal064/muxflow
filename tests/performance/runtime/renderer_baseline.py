#!/usr/bin/env python3
"""Black-box key-to-pixels probe for the terminal renderer (Phase 0, T0.7).

Treats the app as opaque, so the same instrument measures the Tauri app and the
GPUI spike on the same X display: a real key goes in through XTEST, and the time
is taken until `GetImage` shows different pixels in the region where the echo
lands. No shipping code is touched. Raw X11 over the Unix socket (stdlib only).

Driven by run-renderer-baseline.sh, which owns the display, the isolation and
the cleanup. Failures and timeouts are reported as results, not hidden, and the exit
status is 0 unless a scenario could not run at all.
"""
import argparse
import json
import math
import os
import random
import re
import signal
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import zlib

class Terminated(BaseException):
    """SIGTERM: unwinds through App.__exit__ so everything the run started is stopped, and ends the run."""


# ---------------------------------------------------------------- X11 ----


class X11:
    def __init__(self):
        display = os.environ.get("DISPLAY", "")
        m = re.fullmatch(r":(\d+)(?:\.\d+)?", display)
        if not m:
            raise SystemExit(f"unsupported DISPLAY {display!r}; a local X display is required")
        self.number = int(m.group(1))
        self.errors = []
        self.seq = 0  # requests sent since setup, to tell an error answering a reply from an earlier one
        self.sock = None
        for name, data in [(b"", b"")] + self._cookies():
            self.sock = socket.socket(socket.AF_UNIX)
            self.sock.connect(f"/tmp/.X11-unix/X{self.number}")
            self.sock.settimeout(30)
            if self._setup(name, data):
                break
            self.sock.close()
            self.sock = None
        if self.sock is None:
            raise SystemExit("the X server refused the connection (no usable authorization)")
        self.xtest = self._extension(b"XTEST")
        if self.xtest is None:
            raise SystemExit("the X server has no XTEST extension, so keys cannot be injected")
        self._load_keymap()

    def _cookies(self):
        path = os.environ.get("XAUTHORITY") or os.path.expanduser("~/.Xauthority")
        out = []
        try:
            blob = open(path, "rb").read()
        except OSError:
            return out
        i = 0
        try:
            while i < len(blob):
                fields = []
                i += 2  # the address family
                for _ in range(4):
                    n = struct.unpack(">H", blob[i:i + 2])[0]
                    fields.append(blob[i + 2:i + 2 + n])
                    i += 2 + n
                number, name, data = fields[1], fields[2], fields[3]
                if number in (b"", str(self.number).encode()) and name == b"MIT-MAGIC-COOKIE-1":
                    out.append((name, data))
        except struct.error:
            pass
        return out

    def _send(self, data, requests=1):
        self.seq += requests
        self.sock.sendall(data)

    def _read(self, n):
        buf = bytearray()
        while len(buf) < n:
            try:
                chunk = self.sock.recv(n - len(buf))
            except socket.timeout:
                raise SystemExit("the X server did not answer within 30 s")
            if not chunk:
                raise SystemExit("the X connection closed")
            buf += chunk
        return bytes(buf)

    def _setup(self, name, data):
        pad = lambda b: b"\0" * (-len(b) % 4)
        self.sock.sendall(struct.pack("<BxHHHHxx", ord("l"), 11, 0, len(name), len(data)) + name + pad(name) + data + pad(data))
        head = self._read(8)
        body = self._read(struct.unpack("<H", head[6:8])[0] * 4)
        if head[0] != 1:
            return False
        vendor = struct.unpack("<H", body[16:18])[0]
        formats = body[21]
        self.min_kc, self.max_kc = body[26], body[27]
        screen = 32 + vendor + (-vendor % 4) + 8 * formats
        self.root = struct.unpack("<I", body[screen:screen + 4])[0]
        self.screen_w, self.screen_h = struct.unpack("<HH", body[screen + 20:screen + 24])
        return True

    def _reply(self):
        while True:
            head = self._read(32)
            if head[0] == 1:
                extra = struct.unpack("<I", head[4:8])[0] * 4
                return head, (self._read(extra) if extra else b"")
            if head[0] == 0 and struct.unpack("<H", head[2:4])[0] == self.seq & 0xFFFF:
                raise SystemExit(f"X error {head[1]} answering the request the probe was waiting on (BadMatch/BadDrawable?)")
            self.errors.append(head[1])  # an error from a fire-and-forget request, or an event

    def _extension(self, name):
        pad = b"\0" * (-len(name) % 4)
        self._send(struct.pack("<BxHHxx", 98, 2 + (len(name) + 3) // 4, len(name)) + name + pad)
        head, _ = self._reply()
        return head[9] if head[8] else None

    def _load_keymap(self):
        count = self.max_kc - self.min_kc + 1
        self._send(struct.pack("<BxHBBxx", 101, 2, self.min_kc, count))
        head, body = self._reply()
        per = head[1]
        self.keymap = {}
        for i in range(count):
            for col, sym in enumerate(struct.unpack(f"<{per}I", body[i * per * 4:(i + 1) * per * 4])):
                if sym and col < 2:
                    self.keymap.setdefault((sym, col), self.min_kc + i)

    def keycode(self, name):
        specials = {"Return": 0xFF0D, "Escape": 0xFF1B, "Control_L": 0xFFE3, "Alt_L": 0xFFE9, "BackSpace": 0xFF08,
                    "Left": 0xFF51, "Up": 0xFF52, "Right": 0xFF53, "Down": 0xFF54}
        sym = specials.get(name, ord(name) if len(name) == 1 else None)
        code = self.keymap.get((sym, 0))
        if code is None:
            raise SystemExit(f"no keycode for {name!r} in the keyboard layout (a US layout is assumed)")
        return code

    def _fake(self, kind, detail, x=0, y=0):
        return struct.pack("<BBHBBxxIIxxxxxxxxhhxxxxxxxB", self.xtest, 2, 9, kind, detail, 0, self.root, x, y, 0)

    def key(self, name, mods=()):
        """Press and release one key, with modifier keys held, in a single write. Returns the time just before it."""
        mods = ["Control_L" if m is True else m for m in ([mods] if isinstance(mods, str) else mods or ())]
        code = self.keycode(name)
        msgs = b"".join(self._fake(2, self.keycode(m)) for m in mods)
        msgs += self._fake(2, code) + self._fake(3, code)
        msgs += b"".join(self._fake(3, self.keycode(m)) for m in reversed(mods))
        t0 = time.perf_counter()
        self._send(msgs, len(msgs) // 36)
        return t0

    def geometry(self, win):
        self._send(struct.pack("<BxHI", 14, 2, win))
        head, _ = self._reply()
        return struct.unpack("<hhHH", head[12:20])

    def resize(self, win, w, h):
        msg = struct.pack("<BxHIHxxII", 12, 5, win, 0x0C, w, h)
        t0 = time.perf_counter()
        self._send(msg)
        return t0

    def focus(self, win):
        self._send(struct.pack("<BBHII", 42, 1, 3, win, 0))

    def grab(self, win, x, y, w, h):
        self._send(struct.pack("<BBHIhhHHI", 73, 2, 5, win, x, y, w, h, 0xFFFFFFFF))
        head, body = self._reply()
        return body

    def sync(self):
        self.geometry(self.root)


def find_window(x, pid, timeout=90):
    """The biggest visible window of a process, by xdotool's _NET_WM_PID search."""
    end = time.time() + timeout
    while time.time() < end:
        out = subprocess.run(["xdotool", "search", "--onlyvisible", "--pid", str(pid)], capture_output=True, text=True).stdout.split()
        best = None
        for w in out:
            try:
                _, _, gw, gh = x.geometry(int(w))
            except SystemExit:
                continue
            if gw >= 200 and gh >= 200 and (best is None or gw * gh > best[1]):
                best = (int(w), gw * gh)
        if best:
            return best[0]
        time.sleep(0.5)
    raise SystemExit("the app's window never appeared")


def save_png(path, bgrx, w, h):
    rows = []
    for y in range(h):
        row = bgrx[y * w * 4:(y + 1) * w * 4]
        rgb = bytearray(w * 3)
        rgb[0::3], rgb[1::3], rgb[2::3] = row[2::4], row[1::4], row[0::4]
        rows.append(b"\0" + bytes(rgb))
    raw = zlib.compress(b"".join(rows), 3)
    chunk = lambda t, d: struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", raw) + chunk(b"IEND", b""))


def color_bbox(img, w, h, rgb, minimum=200):
    """Bounding box of the pixels exactly equal to rgb, or None."""
    target = rgb[2] | rgb[1] << 8 | rgb[0] << 16
    view = memoryview(img).cast("I")
    x0 = y0 = 1 << 30
    x1 = y1 = -1
    count = 0
    for y in range(h):
        row = view[y * w:(y + 1) * w]
        hits = [i for i, v in enumerate(row) if v & 0xFFFFFF == target]
        if hits:
            count += len(hits)
            x0, x1 = min(x0, hits[0]), max(x1, hits[-1])
            y0, y1 = min(y0, y), max(y1, y)
    return (x0, y0, x1, y1) if count >= minimum else None


def first_color(strip, rgb):
    """Index of the first pixel in a 1-pixel-high or -wide strip equal to rgb, or -1."""
    view = memoryview(strip).cast("I")
    t = rgb[2] | rgb[1] << 8 | rgb[0] << 16
    for i, v in enumerate(view):
        if v & 0xFFFFFF == t:
            return i
    return -1


def pixel_is(px, rgb):
    return len(px) >= 3 and px[2] == rgb[0] and px[1] == rgb[1] and px[0] == rgb[2]


def diff_bbox(a, b, w, h):
    """Bounding box of the pixels that differ between two same-size images."""
    va, vb = memoryview(a).cast("I"), memoryview(b).cast("I")
    x0 = y0 = 1 << 30
    x1 = y1 = -1
    for y in range(h):
        ra, rb = va[y * w:(y + 1) * w], vb[y * w:(y + 1) * w]
        if ra == rb:
            continue
        hits = [i for i in range(w) if ra[i] & 0xFFFFFF != rb[i] & 0xFFFFFF]
        if hits:
            x0, x1 = min(x0, hits[0]), max(x1, hits[-1])
            y0, y1 = min(y0, y), max(y1, y)
    return (x0, y0, x1, y1) if x1 >= 0 else None


def is_echo(base, cur):
    """A typed letter repaints pixels into several colors (ink and its antialiasing). A blinking
    cursor flips them all to one color, so a single-color change is not an echo."""
    vb, vc = memoryview(base).cast("I"), memoryview(cur).cast("I")
    colors = set()
    for b, c in zip(vb, vc):
        if b != c:
            colors.add(c & 0xFFFFFF)
            if len(colors) >= 3:
                return True
    return False


# ------------------------------------------------------------- stats ----


def dist(values_s):
    """Nearest-rank p50/p95/p99 in ms, as the T0.4 report computes them."""
    if not values_s:
        return None
    v = sorted(values_s)
    pick = lambda q: v[max(0, min(len(v) - 1, math.ceil(q * len(v)) - 1))] * 1000
    return {"n": len(v), "p50": round(pick(0.50), 2), "p95": round(pick(0.95), 2), "p99": round(pick(0.99), 2), "max": round(v[-1] * 1000, 2)}


# ------------------------------------------------------------ process ----


class Procs:
    """Every process started under this run, found by the marker in its environment."""

    def __init__(self, marker):
        self.marker = f"MUXFLOW_RB_RUN={marker}".encode()

    def pids(self):
        out = []
        me = os.getpid()
        for name in os.listdir("/proc"):
            if not name.isdigit() or int(name) == me:
                continue
            try:
                with open(f"/proc/{name}/environ", "rb") as f:
                    if self.marker in f.read().split(b"\0"):
                        out.append(int(name))
            except OSError:
                pass
        return out

    @staticmethod
    def info(pid):
        try:
            comm = open(f"/proc/{pid}/comm").read().strip()
            rss = 0
            for line in open(f"/proc/{pid}/status"):
                if line.startswith("VmRSS:"):
                    rss = int(line.split()[1])
            stat = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
            cpu = (int(stat[11]) + int(stat[12])) / os.sysconf("SC_CLK_TCK")
            return comm, rss, cpu
        except (OSError, IndexError, ValueError):
            return None

    def snapshot(self, app_pid):
        """RSS (kB) and CPU seconds of the app process itself, its WebKit processes and the host helper.
        Panes, tmux and the flooders are not the app and are left out for both apps."""
        groups = {}
        for pid in self.pids():
            info = self.info(pid)
            if not info:
                continue
            comm, rss, cpu = info
            role = "app" if pid == app_pid else "webview" if comm.startswith("WebKit") else "helper" if comm.startswith("muxflow-host") else None
            if role is None:
                continue
            g = groups.setdefault(role, {"rss_kb": 0, "cpu_s": 0.0, "procs": 0})
            g["rss_kb"] += rss
            g["cpu_s"] += cpu
            g["procs"] += 1
        return groups

    def kill(self):
        for sig in (signal.SIGTERM, signal.SIGKILL):
            pids = self.pids()
            for pid in pids:
                try:
                    os.kill(pid, sig)
                except OSError:
                    pass
            end = time.time() + 3
            while time.time() < end and self.pids():
                time.sleep(0.1)
            if not self.pids():
                return


# --------------------------------------------------------------- tmux ----


class Tmux:
    """A private tmux server: its own TMUX_TMPDIR and socket, never the owner's."""

    def __init__(self, tmpdir, socket_name):
        assert socket_name.startswith("muxflow-qa-")
        self.env = {**os.environ, "TMUX_TMPDIR": tmpdir}
        self.env.pop("TMUX", None)
        self.base = ["tmux", "-L", socket_name, "-f", "/dev/null"]
        self.control = None

    def run(self, *args, check=True):
        r = subprocess.run(self.base + list(args), env=self.env, capture_output=True, text=True)
        if check and r.returncode:
            raise SystemExit(f"tmux {' '.join(args)}: {r.stderr.strip()}")
        return r.stdout

    def open_control(self, session):
        """A quiet control client, so timed commands are one write (no process spawn)."""
        self.control = subprocess.Popen(self.base + ["-C", "attach-session", "-f", "no-output,ignore-size,no-detach-on-destroy", "-t", session],
                                        env=self.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
        threading.Thread(target=lambda: [None for _ in iter(lambda: self.control.stdout.read(65536), b"")], daemon=True).start()

    def fast(self, line):
        t0 = time.perf_counter()
        self.control.stdin.write((line + "\n").encode())
        self.control.stdin.flush()
        return t0

    def scaffold(self, session, panes):
        """The same layout the GPUI spike builds for itself: window 0 with N tiled panes, then window 1."""
        self.run("kill-server", check=False)
        for _ in range(100):
            r = subprocess.run(self.base + ["new-session", "-d", "-s", session, "-x", "160", "-y", "44", "-e", "HISTFILE=/dev/null", "-e", "LANG=C.UTF-8", "bash --norc --noprofile"],
                               env=self.env, capture_output=True, text=True)
            if not r.returncode:
                break
            time.sleep(0.1)
        for opt in (["set-option", "-g", "status", "off"], ["set-environment", "-g", "HISTFILE", "/dev/null"], ["set-environment", "-g", "LANG", "C.UTF-8"],
                    ["set-environment", "-g", "PS1", "$ "], ["set-option", "-g", "history-limit", "2000"]):
            self.run(*opt)
        for _ in range(1, panes):
            self.run("split-window", "-d", "-t", f"{session}:0", "bash --norc --noprofile")
            self.run("select-layout", "-t", f"{session}:0", "tiled")
        self.run("new-window", "-d", "-t", f"{session}:1", "bash --norc --noprofile")

    def panes(self, window):
        out = self.run("list-panes", "-t", window, "-F", "#{pane_id} #{pane_pid}")
        return [tuple(line.split()) for line in out.splitlines()]

    def send(self, pane, text):
        self.run("send-keys", "-t", pane, text, "Enter")

    def kill(self):
        if self.control:
            self.control.kill()
        self.run("kill-server", check=False)


# ---------------------------------------------------------------- app ----


class App:
    def __init__(self, args, panes):
        self.args, self.panes_n = args, panes
        self.run_dir = tempfile.mkdtemp(prefix="muxflow-rb.", dir=args.work_dir)
        os.chmod(self.run_dir, 0o700)
        self.procs = Procs(self.run_dir)
        self.session = "rb" if args.app == "tauri" else "muxflow-spike"
        self.tmux = Tmux(os.path.join(self.run_dir, "tmux"), args.socket)
        self.proc = None
        self.x = X11()
        self.win = None

    def env(self):
        r = self.run_dir
        e = dict(os.environ)
        for d in ("home", "config", "data", "cache", "xdg", "host", "tmux"):
            os.makedirs(os.path.join(r, d), exist_ok=True)
            os.chmod(os.path.join(r, d), 0o700)
        e.update(MUXFLOW_RB_RUN=r, HOME=f"{r}/home", XDG_CONFIG_HOME=f"{r}/config", XDG_DATA_HOME=f"{r}/data", XDG_CACHE_HOME=f"{r}/cache",
                 ADE_HOST_RUNTIME_DIR=f"{r}/host", TMUX_TMPDIR=f"{r}/tmux", ADE_TMUX_SOCKET_NAME=self.args.socket,
                 ADE_HOST_HELPER_PATH=self.args.helper, GDK_BACKEND="x11")
        # WebKitGTK's DMABUF renderer must stay on: with WEBKIT_DISABLE_DMABUF_RENDERER=1 an idle page showed
        # new output ~630 ms late on Xvfb (see docs/gpui-phase0-report.md), a fallback-path artifact and not the renderer.
        e.pop("WEBKIT_DISABLE_DMABUF_RENDERER", None)
        if os.environ.get("SPIKE_USE_CURRENT_DISPLAY") == "1":
            e.update(XDG_RUNTIME_DIR=f"{r}/xdg", DBUS_SESSION_BUS_ADDRESS="disabled:")
            e.pop("WAYLAND_DISPLAY", None)
            if not os.environ.get("RB_SOFTWARE_GL"):
                e.pop("LIBGL_ALWAYS_SOFTWARE", None)  # a real display: let WebKit use the GPU
        else:
            e["LIBGL_ALWAYS_SOFTWARE"] = "1"  # the private Xvfb has no GPU path: software GL, as lavapipe is for GPUI
        e.pop("TMUX", None)
        return e

    def start(self):
        env = self.env()
        os.makedirs(f"{self.run_dir}/config/dev.muxflow.desktop", exist_ok=True)
        with open(f"{self.run_dir}/config/dev.muxflow.desktop/profiles.json", "w") as f:
            json.dump({"schemaVersion": 1, "profiles": [{"id": "local", "label": "Local", "connection": {"mode": "local"}}], "lastProfileId": "local"}, f)
        log = open(f"{self.run_dir}/app.log", "wb")
        if self.args.app == "tauri":
            self.tmux.scaffold(self.session, self.panes_n)
            self.proc = subprocess.Popen([self.args.bin], env=env, stdout=log, stderr=log, start_new_session=True)
        else:
            self.proc = subprocess.Popen([self.args.bin, "run", "--panes", str(self.panes_n), "--socket", self.args.socket], env=env, stdout=log, stderr=log, start_new_session=True)
        self.win = find_window(self.x, self.proc.pid)
        deadline = time.time() + 60
        while time.time() < deadline:  # the app is attached once tmux lists its clients
            out = self.tmux.run("list-clients", "-F", "#{client_name}", check=False)
            if out.strip() and self.session in self.tmux.run("list-sessions", "-F", "#{session_name}", check=False):
                break
            time.sleep(0.5)
        else:
            raise SystemExit("the app never attached to tmux")
        time.sleep(5)
        self.x.focus(self.win)
        if self.args.app == "tauri":
            self.x.key("Escape")  # the one-time agent-hooks dialog
            time.sleep(1)
        for pane, _ in self.tmux.panes(f"{self.session}:0") + self.tmux.panes(f"{self.session}:1"):
            self.tmux.run("send-keys", "-t", pane, "clear", "Enter")
        time.sleep(1.5)
        return self

    def snapshot(self):
        return self.procs.snapshot(self.proc.pid)

    def ensure_control(self):
        """The timed-command client, opened only where a scenario needs it: an extra control client
        slows flood delivery (T0.4's REPORT.md), so the other scenarios run without one."""
        if self.tmux.control is None:
            self.tmux.open_control(self.session)

    def stop(self):
        try:
            self.tmux.kill()
            if self.args.app == "tauri":
                subprocess.run([self.args.helper, "daemon-stop"], env=self.env(), capture_output=True, timeout=20)
        except Exception:
            pass
        finally:
            self.procs.kill()
            subprocess.run(["rm", "-rf", self.run_dir])

    def __enter__(self):
        try:
            return self.start()
        except BaseException:
            self.stop()
            raise

    def __exit__(self, *exc):
        self.stop()

    # -- geometry -----------------------------------------------------
    def size(self):
        _, _, w, h = self.x.geometry(self.win)
        return w, h

    def shot(self, name=None):
        w, h = self.size()
        img = self.x.grab(self.win, 0, 0, w, h)
        if name and self.args.frames:
            os.makedirs(self.args.frames, exist_ok=True)
            save_png(os.path.join(self.args.frames, f"{self.args.app}-{name}.png"), img, w, h)
        return img, w, h

    def focus_pane(self, pane):
        self.tmux.run("select-pane", "-t", pane)
        time.sleep(0.8)

    # -- scenario helpers ---------------------------------------------
    def start_flood(self, panes):
        for pane, _ in panes:
            self.tmux.send(pane, f"exec {self.args.flood_bin} flood-writer")

    def start_painter(self, pane, rgb):
        self.tmux.send(pane, f"exec {self.args.python} {os.path.abspath(__file__)} pane-paint --rgb {rgb[0]},{rgb[1]},{rgb[2]}")

    def fill(self, panes):
        for pane, _ in panes:
            self.tmux.send(pane, f"{self.args.flood_bin} flood-writer --lines 400")
        time.sleep(2.5 if self.args.quick else 4)

    def band(self, keys=18):
        """Where typing lands: the pixels that change when `keys` characters are typed at the prompt."""
        self.x.focus(self.win)
        before, w, h = self.shot()
        for i in range(keys):
            self.x.key("abcdefghijklmnopqrstuvwxyz"[i % 26])
            time.sleep(0.04)
        time.sleep(0.8)
        after, _, _ = self.shot("calibrate")
        box = diff_bbox(before, after, w, h)
        self.x.key("u", mods="Control_L")
        time.sleep(0.6)
        if box is None:
            return None, "typed keys changed no pixels: the key never reached the app, or the app did not repaint"
        if (box[2] - box[0]) * (box[3] - box[1]) > 0.25 * w * h:
            return None, f"typing changed a quarter of the window ({box}): something else is animating"
        return (max(0, box[0] - 2), max(0, box[1] - 2), min(w, box[2] + 3) - max(0, box[0] - 2), min(h, box[3] + 3) - max(0, box[1] - 2)), None

    def idle_changes(self, rect, seconds=2.0):
        """Spontaneous pixel changes in a region with nobody typing: cursor blink, clocks."""
        base = self.x.grab(self.win, *rect)
        changes = 0
        end = time.time() + seconds
        while time.time() < end:
            cur = self.x.grab(self.win, *rect)
            if cur != base:
                changes += 1
                base = cur
            time.sleep(0.005)
        return changes


# ---------------------------------------------------------- scenarios ----

LETTERS = "abcdefghijklmnopqrstuvwxyz"


def recover(reached, limit=30):
    """After a timed-out iteration the UI is still in the old state, and the next iteration's target
    state may already hold. Wait for the timed-out target to arrive so the next sample starts from a
    known state; False means it never did and the scenario must stop."""
    end = time.time() + limit
    while time.time() < end:
        if reached():
            return True
    return False


def quiesce(x, win, rect, quiet=0.3, limit=6.0):
    """Wait until a region has not changed for `quiet` seconds, so a late echo or an erase that is
    still repainting is not credited to the next key."""
    end = time.time() + limit
    last, since = x.grab(win, *rect), time.time()
    while time.time() < end:
        cur = x.grab(win, *rect)
        if cur != last:
            last, since = cur, time.time()
        elif time.time() - since >= quiet:
            return True
    return False


def type_samples(app, rect, until, gap_ms, key_timeout, clear_every=18):
    """Closed loop, one key at a time: key, then poll until the region changes."""
    x, win = app.x, app.win
    samples, timeouts, i, since = [], 0, 0, 0
    rejected = 0
    rng = random.Random(0x7E57)
    while not until(len(samples) + timeouts):
        if since >= clear_every:
            x.focus(win)  # XTEST goes to the focused window: keep it ours
            x.key("u", mods="Control_L")
            time.sleep(0.08)
            quiesce(x, win, rect)
            since = 0
        time.sleep(rng.uniform(*gap_ms) / 1000)
        base = x.grab(win, *rect)
        time.sleep(0.002)
        if x.grab(win, *rect) != base:  # still repainting from the last key
            time.sleep(0.05)
            base = x.grab(win, *rect)
        t0 = x.key(LETTERS[i % 26])
        i += 1
        since += 1
        seen = None
        while time.perf_counter() - t0 < key_timeout:
            cur = x.grab(win, *rect)
            if cur != base:
                if is_echo(base, cur):
                    seen = time.perf_counter()
                    break
                rejected += 1  # cursor blink: keep waiting, against the new picture
                base = cur
        if seen is None:
            timeouts += 1
            quiesce(x, win, rect, limit=key_timeout)  # the late echo must not land in the next sample
        else:
            samples.append(seen - t0)
    return samples, timeouts, rejected


def typing_result(app, rect, label, until, gap_ms, key_timeout):
    noise = app.idle_changes(rect)
    start = time.time()
    samples, timeouts, rejected = type_samples(app, rect, until, gap_ms, key_timeout)
    out = {"scenario": label, "rect": rect, "idle_noise_changes_in_2s": noise, "blink_changes_ignored": rejected, "timeouts": timeouts, "elapsed_s": round(time.time() - start, 1), "key_to_pixels": dist(samples), "raw_ms": [round(s * 1000, 2) for s in samples]}
    return out


def sc_key(args, name, panes, fill, flood_n, seconds_default):
    with App(args, panes) as app:
        ps = app.tmux.panes(f"{app.session}:0")
        target = ps[-1] if flood_n else ps[0]
        if fill:
            app.fill(ps)
        app.focus_pane(target[0])
        rect, err = app.band()
        if err:
            return {"scenario": name, "error": err}
        procs0 = app.snapshot()
        if flood_n:
            app.start_flood(ps[:flood_n])
            time.sleep(3)
        gap = (10, 40) if flood_n else (20, 80)
        if flood_n:
            until = lambda n, end=time.time() + (8 if args.quick else 60): time.time() >= end
        else:
            count = 30 if args.quick else 600
            until = lambda n: n >= count
        out = typing_result(app, rect, name, until, gap, 3 if not flood_n else 3)
        out.update(panes=panes, flooding=flood_n, filled=fill, rss_start=procs0, rss_end=app.snapshot())
        app.shot(f"{name}-end")
        return out


def sc_flood_all(args):
    with App(args, 8) as app:
        ps = app.tmux.panes(f"{app.session}:0")
        app.start_flood(ps)
        time.sleep(3)
        seconds = 8 if args.quick else 60
        pids = [int(p) for _, p in ps]
        wchar = lambda: sum(int(l.split()[1]) for pid in pids for l in open(f"/proc/{pid}/io") if l.startswith("wchar:"))
        w0, t0, snap0 = wchar(), time.time(), app.snapshot()
        series = []
        while time.time() - t0 < seconds:
            time.sleep(1)
            series.append(app.snapshot())
        elapsed = time.time() - t0
        w1, snap1 = wchar(), app.snapshot()
        app.shot("flood-8of8-end")
        cpu = {r: round((snap1[r]["cpu_s"] - snap0[r]["cpu_s"]) / elapsed * 100, 1) for r in snap1 if r in snap0}
        peak = {r: max(s.get(r, {}).get("rss_kb", 0) for s in series) for r in snap1}
        return {"scenario": "flood-8of8", "seconds": round(elapsed, 1), "flooder_write_mb_s": round((w1 - w0) / 1e6 / elapsed, 2),
                "rss_start_kb": {r: v["rss_kb"] for r, v in snap0.items()}, "rss_end_kb": {r: v["rss_kb"] for r, v in snap1.items()},
                "rss_peak_kb": peak, "cpu_percent": cpu,
                "note": "write rate is what the flooders' write() calls were accepted at (pty to tmux), not bytes delivered to the app"}


def sc_type_into_flood(args):
    """Typing into a pane that is itself flooding. Its echo is written by a small program at a fixed
    cell, so pixel change there means the key's echo was painted (a scrolling flood would hide it)."""
    with App(args, 8) as app:
        ps = app.tmux.panes(f"{app.session}:0")
        target = ps[-1]
        app.tmux.send(target[0], f"exec {args.python} {os.path.abspath(__file__)} pane-park-flood")
        time.sleep(2)
        app.focus_pane(target[0])
        before, w, h = app.shot()
        app.x.key("z")  # not a letter the samples type, so the first sample is a change too
        time.sleep(1)
        after, _, _ = app.shot("calibrate-parked")
        box = diff_bbox(before, after, w, h)
        if box is None:
            return {"scenario": "type-into-flood", "error": "the parked echo cell never changed (keys not reaching the pane)"}
        rect = (max(0, box[0] - 2), max(0, box[1] - 2), box[2] - box[0] + 5, box[3] - box[1] + 5)
        os.kill(int(target[1]), signal.SIGUSR1)  # start flooding that pane
        app.start_flood(ps[:-1])
        time.sleep(3)
        n = 6 if args.quick else 20
        timeout = 8 if not args.quick else 4
        noise = app.idle_changes(rect, 1.0)
        samples, timeouts, rejected = type_samples(app, rect, lambda k: k >= n, (300, 400), timeout, clear_every=10_000)
        app.shot("type-into-flood-end")
        return {"scenario": "type-into-flood", "mechanism": "echo written by a small program in the pane at a fixed cell while it floods the other rows (not a kernel pty echo under a scrolling flooder, as in T0.4)", "keys": n, "completed": len(samples), "timeouts": timeouts, "key_timeout_s": timeout, "rect": rect,
                "idle_noise_changes_in_1s": noise, "blink_changes_ignored": rejected, "key_to_pixels": dist(samples), "raw_ms": [round(s * 1000, 2) for s in samples]}


def settle_bbox(app, rgb, tries=40):
    """Full-window bbox of a sentinel color once it has stopped moving."""
    last = None
    for _ in range(tries):
        img, w, h = app.shot()
        box = color_bbox(img, w, h, rgb)
        if box and box == last:
            return box
        last = box
        time.sleep(0.4)
    return None


MAGENTA, GREEN, YELLOW = (255, 0, 255), (0, 255, 0), (255, 255, 0)


def sc_resize(args, flood):
    name = "resize-flood" if flood else "resize-idle"
    sizes = [(1280, 800), (1000, 640)]
    with App(args, 8) as app:
        ps = app.tmux.panes(f"{app.session}:0")
        app.start_painter(ps[-1][0], MAGENTA)
        time.sleep(2)
        sig = {}
        for size in sizes + [sizes[0]]:
            app.x.resize(app.win, *size)
            time.sleep(1.0)
            box = settle_bbox(app, MAGENTA)
            if box is None:
                return {"scenario": name, "error": f"sentinel tile not found at {size}"}
            sig[size] = box
            app.shot(f"{name}-{size[0]}")
        xs = [max(sig[s][0] for s in sizes), min(sig[s][2] for s in sizes)]
        ys = [max(sig[s][1] for s in sizes), min(sig[s][3] for s in sizes)]
        if xs[0] >= xs[1] or ys[0] >= ys[1]:
            return {"scenario": name, "error": f"the sentinel tile at the two sizes does not overlap: {sig}"}
        xc, yc = (xs[0] + xs[1]) // 2, (ys[0] + ys[1]) // 2
        if flood:
            app.start_flood(ps[:-1])
            time.sleep(3)
        iterations = 4 if args.quick else (10 if flood else 30)
        timeout = 15 if flood else 5
        rng = random.Random(0xBEEF)
        def resized(size, want):
            row = app.x.grab(app.win, 0, yc, size[0], 1)
            col = app.x.grab(app.win, xc, 0, 1, size[1])
            fx, fy = first_color(row, MAGENTA), first_color(col, MAGENTA)
            return fx >= 0 and fy >= 0 and abs(fx - want[0]) <= 2 and abs(fy - want[1]) <= 2

        done, timeouts, aborted = [], 0, False
        for i in range(iterations):
            size = sizes[(i + 1) % 2]
            want = sig[size]
            t0 = app.x.resize(app.win, *size)
            ok = False
            while time.perf_counter() - t0 < timeout:
                if resized(size, want):
                    ok = True
                    break
            if ok:
                done.append(time.perf_counter() - t0)
            else:
                timeouts += 1
                if not recover(lambda: resized(size, want)):
                    aborted = True
                    break
            time.sleep(0.4)
            time.sleep(rng.uniform(0, 0.033))
        app.shot(f"{name}-end")
        return {"scenario": name, "iterations": iterations, "timeouts": timeouts, "timeout_s": timeout, "tile_bbox_by_size": {f"{k[0]}x{k[1]}": v for k, v in sig.items()}, "aborted_after_timeout": aborted, "resize_to_new_layout_painted": dist(done), "raw_ms": [round(s * 1000, 2) for s in done]}


def switch_tab(app, index):
    """Select window `index`: the app's own Alt+N shortcut for Tauri, which does not follow an
    external tmux select-window. The GPUI spike has no UI for it, so it gets the tmux command, as T0.4 did."""
    if app.args.app == "tauri":
        return app.x.key(str(index + 1), mods="Alt_L")
    app.ensure_control()
    return app.tmux.fast(f"select-window -t {app.session}:{index}")


def switch_pane(app, pane, direction):
    """Focus a pane: Ctrl+Alt+arrow for Tauri (its own directional focus), tmux select-pane for the spike."""
    if app.args.app == "tauri":
        return app.x.key(direction, mods=["Control_L", "Alt_L"])
    app.ensure_control()
    return app.tmux.fast(f"select-pane -t {pane}")


def switch_trigger(app):
    return "XTEST key (Alt+N / Ctrl+Alt+arrow)" if app.args.app == "tauri" else "tmux command through a control client (no UI for it in the spike)"


def sc_tab_switch(args, flood):
    name = "tab-switch-flood" if flood else "tab-switch-idle"
    with App(args, 8) as app:
        ps0 = app.tmux.panes(f"{app.session}:0")
        ps1 = app.tmux.panes(f"{app.session}:1")
        app.start_painter(ps1[0][0], YELLOW)
        time.sleep(1.5)
        switch_tab(app, 1)
        time.sleep(1.5)
        box = settle_bbox(app, YELLOW)
        if box is None:
            app.shot(f"{name}-failed")
            return {"scenario": name, "error": "the sentinel window never showed its color"}
        app.shot(f"{name}-b")
        pts = [(box[0] + (box[2] - box[0]) * a // 4, box[1] + (box[3] - box[1]) * b // 4) for a, b in ((1, 1), (3, 1), (2, 2), (1, 3), (3, 3))]
        switch_tab(app, 0)
        time.sleep(1.5)
        if flood:
            app.start_flood(ps0[:-1])
            time.sleep(3)
        iterations = 6 if args.quick else (8 if flood else 60)
        timeout = 15 if flood else 5
        rng = random.Random(0xCAFE)
        def state_b():
            return [pixel_is(app.x.grab(app.win, x, y, 1, 1), YELLOW) for x, y in pts]

        def shows(to_b):
            s = state_b()
            return all(s) if to_b else not all(s)

        done, timeouts, aborted = [], 0, False
        for i in range(iterations):
            to_b = i % 2 == 0
            t0 = switch_tab(app, 1 if to_b else 0)
            ok = False
            while time.perf_counter() - t0 < timeout:
                if shows(to_b):
                    ok = True
                    break
            if ok:
                done.append(time.perf_counter() - t0)
            else:
                timeouts += 1
                if not recover(lambda: shows(to_b)):
                    aborted = True
                    break
            time.sleep(0.05 + rng.uniform(0, 0.033))
        return {"scenario": name, "iterations": iterations, "timeouts": timeouts, "timeout_s": timeout, "aborted_after_timeout": aborted, "trigger": switch_trigger(app), "tab_switch": dist(done), "raw_ms": [round(s * 1000, 2) for s in done]}


def sc_pane_switch(args, flood):
    """Two quiet painted panes side by side (bottom row), focus alternating between them. The probe
    watches pane A's cursor cell only: unfocused it is a steady outline picture in both apps, focused it
    is a (blinking, in Tauri) block. Focus leaving A is done when the cell equals the outline, and focus
    returning is done when it no longer does."""
    name = "pane-switch-flood" if flood else "pane-switch-idle"
    with App(args, 8) as app:
        ps = app.tmux.panes(f"{app.session}:0")
        a, b = ps[6], ps[7]
        app.start_painter(a[0], GREEN)
        app.start_painter(b[0], MAGENTA)
        time.sleep(2)
        box = settle_bbox(app, GREEN)
        if box is None:
            app.shot(f"{name}-failed")
            return {"scenario": name, "error": "the sentinel pane never showed its color"}
        rect = (box[0], box[1], 14, 24)  # the cursor sits at the tile's top-left cell
        if args.app == "tauri":  # focus starts on pane 0 (top left): go to the bottom-left pane, A
            for key in ("Down", "Down", "Left"):
                switch_pane(app, None, key)
                time.sleep(0.6)
        else:
            app.focus_pane(a[0])
        switch_pane(app, b[0], "Right")  # focus B, so A is the unfocused one
        time.sleep(1.0)
        outline = app.x.grab(app.win, *rect)
        time.sleep(0.5)
        if app.x.grab(app.win, *rect) != outline or len(set(outline[i:i + 4] for i in range(0, len(outline), 4))) < 2:
            return {"scenario": name, "error": "the unfocused cursor did not settle to a steady outline"}
        app.shot(f"{name}-start")
        if flood:
            app.start_flood(ps[:6])
            time.sleep(3)
        iterations = 6 if args.quick else (8 if flood else 60)
        timeout = 15 if flood else 5
        rng = random.Random(0xF0CA)
        focused_a = lambda to_a: (app.x.grab(app.win, *rect) != outline) == to_a
        done, timeouts, aborted = [], 0, False
        for i in range(iterations):
            to_a = i % 2 == 0  # B is focused first
            t0 = switch_pane(app, (a if to_a else b)[0], "Left" if to_a else "Right")
            ok = False
            while time.perf_counter() - t0 < timeout:
                if focused_a(to_a):
                    ok = True
                    break
            if ok:
                done.append(time.perf_counter() - t0)
            else:
                timeouts += 1
                if not recover(lambda: focused_a(to_a)):
                    aborted = True
                    break
            time.sleep(0.05 + rng.uniform(0, 0.033))
        return {"scenario": name, "iterations": iterations, "timeouts": timeouts, "timeout_s": timeout, "flooding_panes": 6 if flood else 0, "aborted_after_timeout": aborted, "trigger": switch_trigger(app), "pane_switch": dist(done), "raw_ms": [round(s * 1000, 2) for s in done]}


SCENARIOS = {
    "key-1pane-idle": lambda a: sc_key(a, "key-1pane-idle", 1, False, 0, 0),
    "key-8pane-filled": lambda a: sc_key(a, "key-8pane-filled", 8, True, 0, 0),
    "key-8pane-flood7": lambda a: sc_key(a, "key-8pane-flood7", 8, False, 7, 60),
    "flood-8of8": sc_flood_all,
    "type-into-flood": sc_type_into_flood,
    "resize-idle": lambda a: sc_resize(a, False),
    "resize-flood": lambda a: sc_resize(a, True),
    "tab-switch-idle": lambda a: sc_tab_switch(a, False),
    "tab-switch-flood": lambda a: sc_tab_switch(a, True),
    "pane-switch-idle": lambda a: sc_pane_switch(a, False),
    "pane-switch-flood": lambda a: sc_pane_switch(a, True),
}


def cmd_run(args):
    def terminate(*_):
        raise Terminated()
    signal.signal(signal.SIGTERM, terminate)  # unwinds through App.__exit__
    out_dir = os.path.abspath(args.out)
    os.makedirs(out_dir, exist_ok=True)
    args.frames = os.path.join(out_dir, "frames")
    pattern = re.compile(args.scenarios)
    failed = 0
    for name, fn in SCENARIOS.items():
        if not pattern.search(name):
            continue
        print(f"== {args.app} {name} (load {open('/proc/loadavg').read().split()[0]})", flush=True)
        try:
            result = fn(args)
        except SystemExit as e:
            result = {"scenario": name, "error": str(e)}
        except Exception as e:  # report, keep going
            result = {"scenario": name, "error": f"{type(e).__name__}: {e}"}
        result["app"] = args.app
        if "error" in result:
            failed += 1
            print(f"   FAILED: {result['error']}", flush=True)
        with open(os.path.join(out_dir, f"{name}.json"), "w") as f:
            json.dump(result, f, indent=1)
    sys.exit(1 if failed else 0)


# ---------------------------------------------- programs run in panes ----


def cmd_pane_paint(args):
    """Fill the pane with one solid color, again after every resize."""
    import termios
    import tty
    r, g, b = (int(v) for v in args.rgb.split(","))
    tty.setcbreak(0)
    attrs = termios.tcgetattr(0)
    attrs[3] &= ~termios.ECHO
    termios.tcsetattr(0, termios.TCSANOW, attrs)
    dirty = [True]
    signal.signal(signal.SIGWINCH, lambda *_: dirty.__setitem__(0, True))
    while True:
        if dirty[0]:
            dirty[0] = False
            cols, rows = os.get_terminal_size()
            line = " " * cols
            os.write(1, (f"\x1b[48;2;{r};{g};{b}m\x1b[H" + "\r\n".join([line] * rows) + "\x1b[H").encode())
        time.sleep(0.02)


def cmd_pane_park_flood(args):
    """Raw mode: echoes typed characters at (last row, column 1) and, on SIGUSR1, floods every other row."""
    import select
    import termios
    import tty
    tty.setcbreak(0)
    attrs = termios.tcgetattr(0)
    attrs[3] &= ~termios.ECHO
    termios.tcsetattr(0, termios.TCSANOW, attrs)
    flooding = [False]
    signal.signal(signal.SIGUSR1, lambda *_: flooding.__setitem__(0, True))
    cols, rows = os.get_terminal_size()
    os.write(1, b"\x1b[2J\x1b[?25l\x1b[2 q")
    n = 0
    while True:
        if select.select([0], [], [], 0 if flooding[0] else 0.01)[0]:
            for ch in os.read(0, 64):
                os.write(1, f"\x1b[{rows};1H{chr(ch)}".encode())
        if flooding[0]:
            parts = []
            for _ in range(8):
                for r in range(1, rows):
                    n += 1
                    parts.append(f"\x1b[{r};1H\x1b[3{1 + n % 6}m" + os.urandom(cols // 2 + 1).hex()[:cols] + "\x1b[0m")
            os.write(1, "".join(parts).encode())


def cmd_summarize(args):
    """A table of p50/p95/p99 per scenario, one column per results directory."""
    cols = {}
    for d in args.dirs:
        label = os.path.basename(os.path.normpath(d))
        cols[label] = {}
        for f in sorted(os.listdir(d)):
            if f.endswith(".json"):
                cols[label][f[:-5]] = json.load(open(os.path.join(d, f)))
    names = sorted({n for c in cols.values() for n in c})
    def cell(r):
        if r is None:
            return "n/a"
        if "error" in r:
            return f"ERROR: {r['error'][:60]}"
        d = next((r[k] for k in ("key_to_pixels", "resize_to_new_layout_painted", "tab_switch", "pane_switch") if k in r and r[k]), None)
        t = r.get("timeouts", 0)
        if "flooder_write_mb_s" in r:
            return f"{r['flooder_write_mb_s']} MB/s (flooder writes); RSS app {r['rss_start_kb'].get('app', 0) // 1024}->{r['rss_end_kb'].get('app', 0) // 1024} MB, webview {r['rss_start_kb'].get('webview', 0) // 1024}->{r['rss_end_kb'].get('webview', 0) // 1024} MB"
        if d is None:
            return f"no samples (timeouts {t})"
        extra = f" (n={d['n']}, timeouts {t})"
        if "completed" in r:
            extra = f" (completed {r['completed']}/{r['keys']})"
        return f"{d['p50']} / {d['p95']} / {d['p99']}{extra}"
    print("| Scenario | " + " | ".join(cols) + " |")
    print("|---|" + "---|" * len(cols))
    for n in names:
        print(f"| {n} | " + " | ".join(cell(c.get(n)) for c in cols.values()) + " |")


def main():
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run")
    r.add_argument("--app", choices=["tauri", "gpui"], required=True)
    r.add_argument("--bin", required=True)
    r.add_argument("--helper", default="")
    r.add_argument("--flood-bin", required=True)
    r.add_argument("--python", default=sys.executable)
    r.add_argument("--socket", default="muxflow-qa-rb")
    r.add_argument("--out", required=True)
    r.add_argument("--work-dir", default=tempfile.gettempdir())
    r.add_argument("--scenarios", default=".")
    r.add_argument("--quick", action="store_true")
    r.set_defaults(fn=cmd_run)
    pp = sub.add_parser("pane-paint")
    pp.add_argument("--rgb", required=True)
    pp.set_defaults(fn=cmd_pane_paint)
    pf = sub.add_parser("pane-park-flood")
    pf.set_defaults(fn=cmd_pane_park_flood)
    s = sub.add_parser("summarize")
    s.add_argument("dirs", nargs="+")
    s.set_defaults(fn=cmd_summarize)
    args = p.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
