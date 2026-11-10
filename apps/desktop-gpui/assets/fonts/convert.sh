#!/usr/bin/env bash
# Regenerates the TTFs here from the desktop app's woff2 files.
#
#   - JetBrains Mono: woff2 is a lossless wrapper, so the glyphs are identical.
#   - Inter: GPUI's Linux text system does not honor variable weights, so the
#     chrome's weights are pinned out of InterVariable as static fonts (opsz 14,
#     the text cut); the in-between 550 and 650 resolve to the next heavier face.
#     The family name stays "Inter Variable", so a system-installed "Inter" can
#     never be picked up by accident. Each file gets its own PostScript and full
#     name, because GPUI's macOS text system drops faces that share one.
#
# Usage: ./convert.sh   (needs `uv`; fonttools and brotli are fetched on demand)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
src="$here/../../../desktop/src/assets/fonts"

for f in JetBrainsMono-Regular JetBrainsMono-Bold JetBrainsMono-Italic JetBrainsMono-BoldItalic; do
  uv run --quiet --no-project --with fonttools --with brotli python -I -c '
import sys
from fontTools.ttLib import TTFont
f = TTFont(sys.argv[1]); f.flavor = None; f.save(sys.argv[2])
' "$src/$f.woff2" "$here/$f.ttf"
done

instance() { # source weight output style
  uv run --quiet --no-project --with fonttools --with brotli python -I -c '
import sys
from fontTools.ttLib import TTFont
from fontTools.varLib import instancer
src, weight, out, style = sys.argv[1], float(sys.argv[2]), sys.argv[3], sys.argv[4]
font = instancer.instantiateVariableFont(TTFont(src), {"wght": weight, "opsz": 14})
for record in font["name"].names:
    if record.nameID == 4:
        record.string = f"Inter Variable {style}"
    elif record.nameID == 6:
        record.string = f"InterVariable-{style.replace(chr(32), str())}"
font.flavor = None
font.save(out)
' "$src/$1.woff2" "$2" "$here/$3.ttf" "$4"
}

instance InterVariable 400 Inter-Regular Regular
instance InterVariable 500 Inter-Medium Medium
instance InterVariable 600 Inter-SemiBold SemiBold
instance InterVariable 700 Inter-Bold Bold
instance InterVariable-Italic 400 Inter-Italic Italic
instance InterVariable-Italic 700 Inter-BoldItalic "Bold Italic"

cp "$src/LICENSE.txt" "$here/LICENSE-JetBrainsMono.txt"
cp "$src/LICENSE-Inter.txt" "$here/LICENSE-Inter.txt"
