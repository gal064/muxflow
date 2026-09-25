#!/usr/bin/env bash
# Assert that every copy of the release version agrees, and that the Android
# versionCode is the one derived from it. With an argument (a tag such as
# v0.2.0, or a bare 0.2.0), also assert that it names that version.
#
# release/set-version.sh is the only writer of these fields.
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
expected=${1:-}
expected=${expected#v}

node - "$repo" "$expected" <<'NODE'
const fs = require("fs");
const path = require("path");
const [repo, expected] = process.argv.slice(2);
const read = (file) => fs.readFileSync(path.join(repo, file), "utf8");
const json = (file) => JSON.parse(read(file));
const cargo = (file) => {
  const match = read(file).match(/^\[package\][^[]*?^version = "([^"]+)"/m);
  if (!match) throw new Error(`${file}: no [package] version`);
  return match[1];
};

const app = json("apps/mobile/app.json").expo;
const found = {
  "apps/desktop/src-tauri/tauri.conf.json": json("apps/desktop/src-tauri/tauri.conf.json").version,
  "apps/desktop/src-tauri/Cargo.toml": cargo("apps/desktop/src-tauri/Cargo.toml"),
  "apps/desktop/package.json": json("apps/desktop/package.json").version,
  "apps/host/Cargo.toml": cargo("apps/host/Cargo.toml"),
  "apps/mobile/app.json": app.version,
  "apps/mobile/package.json": json("apps/mobile/package.json").version,
};

const version = expected || found["apps/desktop/src-tauri/tauri.conf.json"];
const semver = /^(\d+)\.(\d+)\.(\d+)$/.exec(version);
const problems = [];
if (!semver) problems.push(`version ${JSON.stringify(version)} is not X.Y.Z`);
for (const [file, value] of Object.entries(found)) {
  if (value !== version) problems.push(`${file}: ${value}, expected ${version}`);
}
if (semver) {
  const [major, minor, patch] = semver.slice(1).map(Number);
  const code = major * 10000 + minor * 100 + patch;
  if (minor > 99 || patch > 99) problems.push(`${version}: minor and patch must stay below 100`);
  if (app.android.versionCode !== code) {
    problems.push(`apps/mobile/app.json: versionCode ${app.android.versionCode}, expected ${code}`);
  }
}
if (problems.length) {
  for (const problem of problems) console.error(`VERSION_MISMATCH ${problem}`);
  process.exit(1);
}
console.log(`VERSION_OK ${version}`);
NODE
