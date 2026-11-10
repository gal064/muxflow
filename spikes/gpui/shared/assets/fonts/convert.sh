#!/usr/bin/env bash
# Regenerates the TTFs here from the desktop app's woff2 files. woff2 is a
# lossless wrapper, so the glyphs are identical to what the Tauri app ships.
# Usage: ./convert.sh   (needs `uv`; fonttools and brotli are fetched on demand)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
src="$here/../../../../../apps/desktop/src/assets/fonts"
for f in JetBrainsMono-Regular JetBrainsMono-Bold JetBrainsMono-Italic JetBrainsMono-BoldItalic InterVariable InterVariable-Italic; do
  uv run --quiet --with fonttools --with brotli python -I -c '
import sys
from fontTools.ttLib import TTFont
f = TTFont(sys.argv[1]); f.flavor = None; f.save(sys.argv[2])
' "$src/$f.woff2" "$here/$f.ttf"
done
