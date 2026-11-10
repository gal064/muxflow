#!/usr/bin/env bash
# Build ONCE before granting TCC. Rebuilding an ad-hoc executable changes identity.
set -euo pipefail
cd "$(dirname "$0")/../../../.."
mkdir -p 'tmp/s11/Muxflow S11 Meter.app/Contents/MacOS'
uv run --no-project --python 3.12 - <<'PLIST'
from pathlib import Path
import plistlib
p=Path('tmp/s11/Muxflow S11 Meter.app/Contents/Info.plist')
with p.open('wb') as f:plistlib.dump(dict(CFBundleIdentifier='dev.muxflow.s11-meter',CFBundleName='Muxflow S11 Meter',CFBundleDisplayName='Muxflow S11 Meter',CFBundleExecutable='meter',CFBundleVersion='1',CFBundleShortVersionString='1.0',CFBundlePackageType='APPL',LSUIElement=True),f)
PLIST
swiftc -parse-as-library tests/performance/runtime/macos/pixels.swift -o 'tmp/s11/Muxflow S11 Meter.app/Contents/MacOS/meter'
codesign -s - --force 'tmp/s11/Muxflow S11 Meter.app'
