"""Process accounting and statistics shared by the app-bench runners.

Everything here reads /proc (Linux) or tables shaped like it, so the Linux
runner and the remote (SSH host) sampler use the same CPU accounting.

CPU: utime+stime from /proc/<pid>/stat, in clock ticks. A process that exits
between two samples would lose its last interval, so a tracked parent's
cutime+cstime growth is credited too, minus what its vanished tracked
children were already credited with. Processes that start inside the window
are counted from zero; processes that existed before it from the first sample.

Memory: Pss and Rss from /proc/<pid>/smaps_rollup, summed per category.

GPU: /proc/<pid>/fdinfo drm-engine-* counters (ns of engine busy time), deduped
per drm-client-id, so a client shared across fds or processes counts once.
"""

from __future__ import annotations

import json
import os
import statistics
from collections import defaultdict
from dataclasses import dataclass, field

CLK_TCK = os.sysconf("SC_CLK_TCK") if hasattr(os, "sysconf") else 100
PAGE_KB = (os.sysconf("SC_PAGE_SIZE") // 1024) if hasattr(os, "sysconf") else 4


@dataclass
class Proc:
    pid: int
    ppid: int
    comm: str
    self_ticks: int
    child_ticks: int
    start: int
    rss_kb: int = 0
    pss_kb: int | None = None


def read_stat(pid: int) -> Proc | None:
    try:
        with open(f"/proc/{pid}/stat", "rb") as handle:
            raw = handle.read()
    except OSError:
        return None
    left = raw.find(b"(")
    right = raw.rfind(b")")
    comm = raw[left + 1 : right].decode(errors="replace")
    rest = raw[right + 2 :].split()
    return Proc(
        pid=pid,
        ppid=int(rest[1]),
        comm=comm,
        self_ticks=int(rest[11]) + int(rest[12]),
        child_ticks=int(rest[13]) + int(rest[14]),
        start=int(rest[19]),
        rss_kb=int(rest[21]) * PAGE_KB,
    )


def proc_table() -> dict[int, Proc]:
    table = {}
    for name in os.listdir("/proc"):
        if name.isdigit():
            proc = read_stat(int(name))
            if proc is not None:
                table[proc.pid] = proc
    return table


def children_map(table: dict[int, Proc]) -> dict[int, list[int]]:
    children: dict[int, list[int]] = defaultdict(list)
    for proc in table.values():
        children[proc.ppid].append(proc.pid)
    return children


def descendants(root: int, children: dict[int, list[int]]) -> list[int]:
    out, stack = [], [root]
    while stack:
        pid = stack.pop()
        out.append(pid)
        stack.extend(children.get(pid, ()))
    return out


def smaps_rollup(pid: int) -> tuple[int, int] | None:
    """(pss_kb, rss_kb), or None when unreadable."""
    pss = rss = None
    try:
        with open(f"/proc/{pid}/smaps_rollup", "rb") as handle:
            for line in handle:
                if line.startswith(b"Pss:"):
                    pss = int(line.split()[1])
                elif line.startswith(b"Rss:"):
                    rss = int(line.split()[1])
    except OSError:
        return None
    if pss is None or rss is None:
        return None
    return pss, rss


def boot_ticks_now() -> int:
    with open("/proc/uptime") as handle:
        return int(float(handle.read().split()[0]) * CLK_TCK)


def drm_clients(pid: int) -> dict[str, dict[str, int]]:
    """client-id -> {"engine:<name>": ns, "vram_kb": kb, "gtt_kb": kb}."""
    clients: dict[str, dict[str, int]] = {}
    fd_dir = f"/proc/{pid}/fd"
    try:
        fds = os.listdir(fd_dir)
    except OSError:
        return clients
    for fd in fds:
        try:
            target = os.readlink(f"{fd_dir}/{fd}")
        except OSError:
            continue
        if not target.startswith("/dev/dri/"):
            continue
        try:
            with open(f"/proc/{pid}/fdinfo/{fd}", "rb") as handle:
                text = handle.read().decode(errors="replace")
        except OSError:
            continue
        values: dict[str, int] = {}
        client = None
        for line in text.splitlines():
            key, _, value = line.partition(":")
            value = value.strip()
            if key == "drm-client-id":
                client = value
            elif key.startswith("drm-engine-") and value.endswith("ns"):
                values["engine:" + key[len("drm-engine-") :]] = int(value.split()[0])
            elif key in ("drm-memory-vram", "drm-resident-vram") and value:
                values["vram_kb"] = max(values.get("vram_kb", 0), _kib(value))
            elif key in ("drm-memory-gtt", "drm-resident-gtt") and value:
                values["gtt_kb"] = max(values.get("gtt_kb", 0), _kib(value))
        if client is not None and client not in clients:
            clients[client] = values
    return clients


def _kib(value: str) -> int:
    number, *unit = value.split()
    amount = int(number)
    unit_name = unit[0] if unit else ""
    return {"": amount // 1024, "KiB": amount, "MiB": amount * 1024, "GiB": amount * 1024 * 1024}.get(
        unit_name, amount
    )


@dataclass
class CpuAccountant:
    """Per-category CPU ticks across samples, robust to processes exiting."""

    window_start_ticks: int
    prev: dict[int, tuple[int, int, int, str, int]] = field(default_factory=dict)
    initialized: bool = False

    def update(self, table: dict[int, Proc], cats: dict[int, str]) -> dict[str, int]:
        deltas: dict[str, int] = defaultdict(int)
        current: dict[int, tuple[int, int, int, str, int]] = {}
        vanished_by_parent: dict[int, int] = defaultdict(int)
        for pid, previous in self.prev.items():
            proc = table.get(pid)
            if proc is None or proc.start != previous[0]:
                vanished_by_parent[previous[4]] += previous[1] + previous[2]
        for pid, cat in cats.items():
            proc = table[pid]
            current[pid] = (proc.start, proc.self_ticks, proc.child_ticks, cat, proc.ppid)
            if not self.initialized:
                continue
            previous = self.prev.get(pid)
            if previous is not None and previous[0] == proc.start:
                d_self = proc.self_ticks - previous[1]
                d_child = proc.child_ticks - previous[2]
                deltas[cat] += d_self + max(0, d_child - vanished_by_parent.get(pid, 0))
            elif proc.start >= self.window_start_ticks:
                deltas[cat] += proc.self_ticks + proc.child_ticks
        self.prev = current
        self.initialized = True
        return deltas


@dataclass
class GpuAccountant:
    prev: dict[tuple[int, int, str], dict[str, int]] = field(default_factory=dict)
    initialized: bool = False

    def update(
        self, samples: dict[tuple[int, int, str], dict[str, int]], cats: dict[int, str]
    ) -> tuple[dict[str, int], dict[str, int]]:
        """Returns (busy-ns delta per category over all engines, vram kb per category)."""
        busy: dict[str, int] = defaultdict(int)
        vram: dict[str, int] = defaultdict(int)
        for key, values in samples.items():
            cat = cats.get(key[0])
            if cat is None:
                continue
            vram[cat] += values.get("vram_kb", 0)
            if not self.initialized:
                continue
            previous = self.prev.get(key, {})
            for name, value in values.items():
                if name.startswith("engine:"):
                    busy[cat] += max(0, value - previous.get(name, 0))
        self.prev = samples
        self.initialized = True
        return busy, vram


def summarize_ticks(ticks: list[dict]) -> dict:
    """Window totals from sampler ticks (each: t, cpu{cat:ticks}, gpu_ns{cat}, pss{cat}, rss{cat}, vram{cat})."""
    if len(ticks) < 2:
        return {}
    duration = ticks[-1]["t"] - ticks[0]["t"]
    cpu_ticks: dict[str, int] = defaultdict(int)
    gpu_ns: dict[str, int] = defaultdict(int)
    for tick in ticks[1:]:
        for cat, value in tick.get("cpu", {}).items():
            cpu_ticks[cat] += value
        for cat, value in tick.get("gpu_ns", {}).items():
            gpu_ns[cat] += value
    cats = set(cpu_ticks)
    for tick in ticks:
        cats.update(tick.get("pss", {}))
    out = {"duration_s": round(duration, 3), "cpu_s": {}, "cpu_pct": {}, "gpu_pct": {}, "pss_mb": {}, "rss_mb": {}, "vram_mb": {}}
    for cat in sorted(cats):
        seconds = cpu_ticks.get(cat, 0) / CLK_TCK
        out["cpu_s"][cat] = round(seconds, 3)
        out["cpu_pct"][cat] = round(100.0 * seconds / duration, 2) if duration else 0.0
        pss = [tick["pss"][cat] for tick in ticks if cat in tick.get("pss", {})]
        rss = [tick["rss"][cat] for tick in ticks if cat in tick.get("rss", {})]
        vram = [tick["vram"][cat] for tick in ticks if cat in tick.get("vram", {})]
        if pss:
            out["pss_mb"][cat] = {"mean": round(statistics.fmean(pss) / 1024, 1), "max": round(max(pss) / 1024, 1)}
        if rss:
            out["rss_mb"][cat] = {"mean": round(statistics.fmean(rss) / 1024, 1), "max": round(max(rss) / 1024, 1)}
        if vram and max(vram) > 0:
            out["vram_mb"][cat] = {"mean": round(statistics.fmean(vram) / 1024, 1), "max": round(max(vram) / 1024, 1)}
    for cat, value in gpu_ns.items():
        out["gpu_pct"][cat] = round(100.0 * value / 1e9 / duration, 2) if duration else 0.0
    return out


def write_jsonl(path: str, rows: list[dict]) -> None:
    with open(path, "w") as handle:
        for row in rows:
            handle.write(json.dumps(row, separators=(",", ":")) + "\n")


def percentile(values: list[float], fraction: float) -> float:
    if not values:
        return float("nan")
    ordered = sorted(values)
    index = (len(ordered) - 1) * fraction
    low = int(index)
    high = min(low + 1, len(ordered) - 1)
    return ordered[low] + (ordered[high] - ordered[low]) * (index - low)
