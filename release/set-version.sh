#!/usr/bin/env bash
# Set the release version everywhere it is recorded, then verify.
#
#   release/set-version.sh 0.2.0
#
# The desktop app, the host helper and the mobile app always ship together, so
# they share one X.Y.Z. The Android versionCode is derived from it as
# major*10000 + minor*100 + patch, which keeps every release an in-place
# upgrade of the one before. The wire protocol version (crates/protocol) is
# separate and is not touched.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
version=${1:?usage: release/set-version.sh X.Y.Z}
version=${version#v}

node - "$repo" "$version" <<'NODE'
const fs = require("fs");
const path = require("path");
const [repo, version] = process.argv.slice(2);
const semver = /^(\d+)\.(\d+)\.(\d+)$/.exec(version);
if (!semver) throw new Error(`${version} is not X.Y.Z`);
const [major, minor, patch] = semver.slice(1).map(Number);
if (minor > 99 || patch > 99) throw new Error("minor and patch must stay below 100");

// Rewrite only the version values, never the formatting around them.
const edit = (name, pattern, replacement) => {
  const target = path.join(repo, name);
  const text = fs.readFileSync(target, "utf8");
  if (!pattern.test(text)) throw new Error(`${name}: no match for ${pattern}`);
  fs.writeFileSync(target, text.replace(pattern, replacement));
};
// The first "version" key is the top-level one in each of these files.
const topLevelVersion = /("version":\s*)"[^"]*"/;
edit("apps/desktop/src-tauri/tauri.conf.json", topLevelVersion, `$1"${version}"`);
edit("apps/desktop/package.json", topLevelVersion, `$1"${version}"`);
edit("apps/mobile/package.json", topLevelVersion, `$1"${version}"`);
edit("apps/mobile/app.json", topLevelVersion, `$1"${version}"`);
edit("apps/mobile/app.json", /("versionCode":\s*)\d+/, `$1${major * 10000 + minor * 100 + patch}`);
const cargoVersion = /^(\[package\][^[]*?^version = )"[^"]+"/m;
edit("apps/desktop/src-tauri/Cargo.toml", cargoVersion, `$1"${version}"`);
edit("apps/host/Cargo.toml", cargoVersion, `$1"${version}"`);
NODE

# Refresh only the workspace's own entries in Cargo.lock.
cargo update --workspace --offline --manifest-path "$repo/Cargo.toml" >/dev/null 2>&1 \
  || cargo update --workspace --manifest-path "$repo/Cargo.toml"
"$repo/release/check-version.sh" "$version"
