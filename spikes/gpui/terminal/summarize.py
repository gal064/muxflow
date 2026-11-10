"""Turn the per-scenario JSON from measure.sh into a markdown summary."""
import json
import sys
from pathlib import Path


def load(root: Path, name: str):
    path = root / f"{name}.json"
    return json.loads(path.read_text())["result"] if path.exists() else None


def fmt(d) -> str:
    if not d or not d.get("count"):
        return "n/a"
    return f"{d['p50_ms']:.2f} / {d['p95_ms']:.2f} / {d['p99_ms']:.2f}"


def key_rows(title: str, data) -> list[str]:
    if data is None:
        return []
    keys = data["keys"]
    return [
        f"| {title} | {keys['count']} | {keys['timeouts']} | {keys['verified_glyph']} | "
        f"{keys['next_output_is_echo']} | {fmt(keys['a_next_output_echoLagProbe'])} | "
        f"{fmt(keys['a_marker_output'])} | {fmt(keys['b_frame_paint'])} | "
        f"{fmt(keys['b_frame_present'])} | {fmt(keys['dispatch_wait'])} |"
    ]


def wait_row(title, data):
    if data is None:
        return []
    k = data["keys"]
    return [f"| {title} | {fmt(k['frame_wait'])} |"]


def frame_row(title: str, frames) -> str:
    return (
        f"| {title} | {frames['count']} | {fmt(frames['interval'])} | {fmt(frames['build'])} | "
        f"{fmt(frames['present'])} | {fmt(frames['total'])} | {frames['over_16_7ms']} | {frames['begin_intervals_under_8ms']} | {frames.get('key_event_draws', 0)} | "
        f"{frames['rows_reshaped_per_frame']:.1f} | {frames['rows_reused_per_frame']:.1f} | "
        f"{frames['pane_views_rendered_per_frame']:.1f} | {frames['panes_prepainted_per_frame']:.1f} |"
    )


def rss_stats(series):
    if not series:
        return "n/a"
    start, end = series[0][1] / 1024, series[-1][1] / 1024
    peak = max(s[1] for s in series) / 1024
    huge = f", AnonHugePages {series[0][2] / 1024:.0f} -> {series[-1][2] / 1024:.0f} MB"
    return f"{start:.0f} -> {end:.0f} MB (peak {peak:.0f}){huge} over {series[-1][0]:.0f}s"


def main() -> None:
    root = Path(sys.argv[1])
    sample = root / "latency-1pane.json"
    if sample.exists():
        print(f"Renderer: {json.loads(sample.read_text())['gpu']}\n")
    load_log = root / "load.txt"
    failed = [l for l in load_log.read_text().splitlines() if l.startswith("FAILED")] if load_log.exists() else []
    if failed:
        print("**Failed scenarios (their rows are missing below):**\n")
        for line in failed:
            print(f"- {line}")
        print()
    for name in ("baseline-tmux-only", "baseline-tmux-term", "baseline-tmux-term-input",
                 "baseline-tmux-only-r2", "baseline-tmux-term-r2", "baseline-tmux-term-input-r2",
                 "baseline-tmux-only-r3", "baseline-tmux-term-r3", "baseline-tmux-term-input-r3"):
        data = load(root, name)
        if data:
            print(f"Control baseline ({name}, no GPUI): {data['throughput_mb_s']:.1f} MB/s, pauses {data['pauses']}\n")
    lat1 = load(root, "latency-1pane")
    lat8 = load(root, "latency-8pane")
    flood = load(root, "flood-7of8")
    flood_all = load(root, "flood-8of8")
    print("## Keystroke latency (ms, p50 / p95 / p99)\n")
    print("| Scenario | samples | timeouts | glyph verified | next output == echo | (a) next output (echoLagProbe semantics) | (a) echo byte delivered | (b) frame paint | (b) frame present | main-thread dispatch wait |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for row in (
        key_rows("1 pane, idle", lat1)
        + key_rows("8 panes, idle", lat8)
        + key_rows("8 panes, filled with text", load(root, "latency-8pane-filled"))
        + key_rows("8 panes, 7 flooding, typing in quiet pane", flood)
        + key_rows("8 panes, all flooding incl typing pane", flood_all)
    ):
        print(row)
    print("\n## Run-to-run spread, (b) frame present p50 / p95 / p99 (ms)\n")
    print("| Scenario | run 1 | run 2 | run 3 |\n|---|---|---|---|")
    for panes in ("1pane", "8pane"):
        cells = []
        for suffix in ("", "-r2", "-r3"):
            data = load(root, f"latency-{panes}{suffix}")
            cells.append(fmt(data["keys"]["b_frame_present"]) if data else "n/a")
        print(f"| {panes} | " + " | ".join(cells) + " |")
    print("\n## Echo applied -> frame build start: the frame-tick wait (ms, p50 / p95 / p99)\n")
    print("| Scenario | frame wait |\n|---|---|")
    for title, data in [("1 pane", lat1), ("8 panes", lat8), ("flood 7 of 8", flood), ("flood 8 of 8", flood_all)]:
        for row in wait_row(title, data):
            print(row)
    print("\n## Frames (ms, p50 / p95 / p99)\n")
    print("| Scenario | frames | interval | GPUI build (CPU) | present (renderer+swapchain) | total | total > 16.7 ms | frame starts < 8 ms apart | extra draws inside key dispatch (no present) | rows reshaped/frame | rows reused/frame | pane views rendered/frame | panes prepainted/frame |")
    print("|---|---|---|---|---|---|---|---|---|---|---|---|---|")
    rows = [
        ("latency, 1 pane", lat1),
        ("latency, 8 panes", lat8),
        ("flood 7 of 8", flood),
        ("flood 8 of 8", flood_all),
    ]
    for name, data in rows:
        if data:
            print(frame_row(name, data["frames"]))
    one_cached = load(root, "onepane-cached")
    one_nocache = load(root, "onepane-nocache")
    for name, data in [("one pane changing, view cache on", one_cached), ("one pane changing, view cache off", one_nocache)]:
        if data:
            print(frame_row(name, data["frames"]))
    print()
    if flood:
        print(f"Flood 7-of-8: {flood['throughput_mb_s']:.1f} MB/s, app CPU {flood['cpu_percent']:.0f}%, tmux server CPU {flood['tmux_server_cpu_percent']:.0f}%, threads {flood['thread_cpu_percent']}, feed thread {flood['feed_thread']}, pauses {flood['pauses']}, resnapshots {flood['resnapshots']}, RSS {rss_stats(flood['rss_kb'])}\n")
    if flood_all:
        print(f"Flood 8-of-8: {flood_all['throughput_mb_s']:.1f} MB/s, app CPU {flood_all['cpu_percent']:.0f}%, tmux server CPU {flood_all['tmux_server_cpu_percent']:.0f}%, threads {flood_all['thread_cpu_percent']}, feed thread {flood_all['feed_thread']}, pauses {flood_all['pauses']}, resnapshots {flood_all['resnapshots']}, RSS {rss_stats(flood_all['rss_kb'])}\n")
    for name, title in [("flood-rss-thp", "RSS control, flood 7-of-8, THP on"), ("flood-rss-nothp", "RSS control, flood 7-of-8, THP off"),
                        ("idle-rss-thp", "RSS control, idle, THP on"), ("idle-rss-nothp", "RSS control, idle, THP off")]:
        data = load(root, name)
        if data:
            print(f"{title}: RSS {rss_stats(data['rss_kb'])}\n")
    nopaint = load(root, "flood-nopaint")
    if nopaint:
        print(f"Flood 7-of-8, panes not painted: {nopaint['throughput_mb_s']:.1f} MB/s, app CPU {nopaint['cpu_percent']:.0f}%, tmux server CPU {nopaint['tmux_server_cpu_percent']:.0f}%, feed thread {nopaint['feed_thread']}\n")
    for name, title in [("resize-8pane", "near-empty panes"), ("resize-8pane-filled", "panes filled with 400 lines"), ("resize-8pane-flood", "7 panes flooding")]:
        resize = load(root, name)
        if resize:
            print(f"Resize, {title} ({resize['applied']['count']} of {resize['iterations']} iterations, {resize['timeouts']} timed out): applied {fmt(resize['applied'])} ms; quiescent {fmt(resize['quiescent'])} ms\n")
    flood_switch = load(root, "switch-8pane-flood")
    if flood_switch:
        print(f"Under flood, tab switch ({flood_switch['tab_timeouts']} of {flood_switch['iterations']} timed out): {fmt(flood_switch['tab_switch'])} ms; pane switch ({flood_switch['pane_timeouts']} timed out): {fmt(flood_switch['pane_switch'])} ms\n")
    switch = load(root, "switch-8pane")
    if switch:
        print(f"Tab switch ({switch['tab_timeouts']} timed out): {fmt(switch['tab_switch'])} ms; pane switch ({switch['pane_timeouts']} timed out): {fmt(switch['pane_switch'])} ms\n")
    pixel = load(root, "pixel-calib")
    if pixel and "error" not in pixel:
        print(f"X11 pixel calibration ({pixel['samples']} keys, {pixel['missed']} missed, {pixel['pixels_seen_before_present_returned']} seen before present returned): key to pixels on the X server {fmt(pixel['key_to_x11_pixels'])} ms; pixels after present returned {fmt(pixel['pixels_after_present_returned'])} ms\n")
    elif pixel:
        print(f"X11 pixel calibration unavailable: {pixel['error']}\n")
    if flood and "dispatch_wait_plus_b_present" in flood["keys"]:
        print(f"Flood 7-of-8, driver deadline to frame present (main-thread wait + leg b, per sample): {fmt(flood['keys']['dispatch_wait_plus_b_present'])} ms; missed-tick frame gap max {flood['frames']['begin_interval']['max_ms']:.0f} ms\n")
    if flood_all:
        k = flood_all["keys"]
        print(f"Typing into a flooded pane: {k['count']} samples, {k['timeouts']} timeouts, {k['timeouts_after_pause']} of those timed out after a %pause of that pane\n")
    ime = load(root, "ime")
    if ime:
        print(f"IME: commit echoed in grid {ime.get('commit_echoed_in_grid')}, caret bounds {ime.get('caret_bounds')}\n")
    ligature = load(root, "ligature")
    if ligature:
        checks = ligature.get("checks", [])
        print(f"Ligatures: {sum(c['ligated_by_default'] for c in checks)} of {len(checks)} strings ligate by default; terminal features change the glyphs\n")
    idle = load(root, "idle-8pane")
    if idle:
        print(f"Idle 8 panes: CPU {idle['cpu_percent']:.2f}%, RSS {rss_stats(idle['rss_kb'])}, frames in {idle['seconds']:.0f}s window: {idle['frames']['count']}\n")
    for name in ["onepane-cached", "onepane-nocache"]:
        data = load(root, name)
        if data:
            print(f"{name}: CPU {data['cpu_percent']:.1f}%, view cache {data['view_cache']}\n")


if __name__ == "__main__":
    main()
