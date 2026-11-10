#!/usr/bin/env bash
# S7 pixel capture for CI: launch the shared spike on the current X display,
# capture the screen until it holds a rendered frame, and fail if it never does.
#
#   xvfb-run -a bash spikes/gpui/ci-frame.sh frame.png     # CI
#   source spikes/gpui/env.sh && bash spikes/gpui/ci-frame.sh frame.png
#
# Needs DISPLAY and a Vulkan driver (VK_DRIVER_FILES) in the environment, and
# ImageMagick. A blank screen has one color; the showcase's text and button
# give hundreds, so 32 distinct colors counts as rendered.
set -euo pipefail

out="${1:?usage: ci-frame.sh OUT.png}"
bin="${SPIKE_BIN:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/target/debug/spike-shared}"
: "${DISPLAY:?needs an X display}"
mkdir -p "$(dirname "$out")"

"$bin" >"${out%.png}.log" 2>&1 &
pid=$!
trap 'kill "$pid" 2>/dev/null || true' EXIT

colors=0
for _ in $(seq 1 40); do
  sleep 0.5
  kill -0 "$pid" 2>/dev/null || { echo "the spike exited before a frame was captured:" >&2; cat "${out%.png}.log" >&2; exit 1; }
  import -window root "$out"
  colors="$(identify -format %k "$out")"
  if [ "$colors" -ge 32 ]; then
    echo "captured $out: $colors distinct colors"
    exit 0
  fi
done
echo "the captured frame is blank ($colors distinct colors); the spike printed:" >&2
cat "${out%.png}.log" >&2
exit 1
