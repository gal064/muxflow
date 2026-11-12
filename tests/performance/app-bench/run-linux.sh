#!/usr/bin/env bash
# Tauri vs GPUI desktop benchmark on Linux (Wayland/Hyprland, amdgpu).
#
#   run-linux.sh <scratch-root> [bench_linux.py options]
#
# Builds nothing: build both apps from one commit first (see README.md). The
# scratch root holds the apps' HOME, helper runtime dir and tmux dir; keep it
# short, because tmux and the helper put Unix sockets in it (107-byte limit).
# Results go to <scratch-root>/results-linux unless --results is given.
#
# Environment:
#   BENCH_SSH_CONFIG   ssh_config with a Host for the SSH target (enables --hosts ssh)
#   BENCH_SSH_TARGET   Host alias in that config (default muxflow-bench)
#   BENCH_RECORDINGS   dir with claude/codex .data + .timing recordings (agents scenario)
#   BENCH_COMPOSITOR   private (default) or session; see README.md before using session
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../.." && pwd)"
root="${1:?usage: run-linux.sh <scratch-root> [options]}"
shift
mkdir -p "$root"
root="$(cd "$root" && pwd)"

for tool in Hyprland hyprctl grim wtype tmux perl uv jq; do
  command -v "$tool" >/dev/null || { echo "missing tool: $tool" >&2; exit 69; }
done
[[ -n "${WAYLAND_DISPLAY:-}" ]] || { echo "run from inside a Wayland session" >&2; exit 69; }
for binary in muxflow muxflow-gpui muxflow-host; do
  [[ -x "$repo/target/release/$binary" ]] || {
    echo "missing $repo/target/release/$binary; build both apps first (README.md)" >&2
    exit 69
  }
done

args=(--root "$root" --repo "$repo" --results "$root/results-linux"
  --compositor "${BENCH_COMPOSITOR:-private}")
if [[ -n "${BENCH_SSH_CONFIG:-}" ]]; then
  args+=(--ssh-config "$BENCH_SSH_CONFIG" --ssh-target "${BENCH_SSH_TARGET:-muxflow-bench}")
fi
if [[ -n "${BENCH_RECORDINGS:-}" ]]; then
  # The replay binary is named claude/codex at run time so the host detects an agent.
  mkdir -p "$BENCH_RECORDINGS/bin"
  rustc --edition 2024 -O "$here/workloads/agent-replay.rs" -o "$BENCH_RECORDINGS/bin/agent-replay"
  args+=(--recordings "$BENCH_RECORDINGS")
fi

uv run --script "$here/bench_linux.py" "${args[@]}" "$@"
bash "$here/sizes.sh" "$root/results-linux" "$repo/target/release"
uv run --script "$here/summarize.py" "$root/results-linux"
