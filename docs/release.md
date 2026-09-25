# Continuous integration and versions

`.github/workflows/ci.yml` is the merge gate. It runs on every pull request and
every push to `main`, uses no secrets, and holds only checks that are
deterministic on a hosted runner: the privacy scan, version agreement,
formatting, warning-free Clippy, the Rust workspace tests on Linux and macOS,
the test-only crates outside the workspace, a bounded parser fuzz smoke, the
TypeScript checks and tests, committed generated code, and the voice sidecar
contracts. The Docker SSH suites, the transfer and scale matrices, performance,
and packaged desktop journeys are not part of it; they are the manual
release-candidate gates listed below.

The desktop app, the host helper and the mobile app share one `X.Y.Z` version.
Change it only with `release/set-version.sh X.Y.Z`, which rewrites every copy
and derives the Android `versionCode` as `major*10000 + minor*100 + patch`;
`release/check-version.sh` verifies them. The wire protocol version in
`crates/protocol` is independent.

# Publishing a release

`.github/workflows/release.yml` builds every downloadable artifact from a
pushed version tag and leaves them in a draft GitHub Release:

1. Run `release/set-version.sh X.Y.Z`, open a pull request, and merge it once CI
   passes.
2. Tag the merged commit and push the tag:
   `git tag vX.Y.Z && git push origin vX.Y.Z`.
3. The workflow stops unless the tagged commit is on `main` and the tree's
   version matches the tag. It then reruns the CI gate. The signing secrets
   live in the `release` environment, which only `v*` tags can use; a ruleset
   lets only the repository admin create, move or delete those tags.
4. The Linux x86-64 and ARM64 packages are built natively on their own runners.
   Each is then given the other architecture's helper. The macOS job takes both
   Linux helpers from those packages, then builds, signs, notarizes and
   verifies the DMG (ad-hoc signed while the Apple secrets are absent). The
   Android job builds the APK with the release key and
   checks its certificate against `apps/mobile/release-cert.sha256`.
5. A draft release appears with the DMG, both Linux tarballs and their
   `.sha256` files, the APK, `SHA256SUMS`, and `latest.json`.
6. Review the draft: install each artifact and run the manual checks below.
   Then press Publish.

`latest.json` is the manifest the desktop and Android apps poll for updates:
`{"version": "X.Y.Z", "url": "<release page>"}`. The apps read it through
`releases/latest/download/latest.json`, which never resolves to a draft or a
pre-release. So the red "Update" pill appears only once a release is
published. A tag with a suffix such as `vX.Y.Z-rc.1` builds from a tree
versioned `X.Y.Z` and is drafted as a pre-release. Use it to rehearse a
release, then delete the draft and the tag.

The `release` environment holds these secrets:

| Secret | Contents |
|---|---|
| `MUXFLOW_MACOS_CERTIFICATE_P12_BASE64`, `MUXFLOW_MACOS_CERTIFICATE_PASSWORD` | Optional. The Developer ID Application certificate, exported as `.p12`; with it and the rows below, the DMG is signed and notarized |
| `MUXFLOW_MACOS_SIGNING_IDENTITY` | `Developer ID Application: Name (TEAMID)` |
| `MUXFLOW_APPLE_TEAM_ID` | The team ID |
| `MUXFLOW_NOTARY_KEY_P8_BASE64`, `MUXFLOW_NOTARY_KEY_ID`, `MUXFLOW_NOTARY_ISSUER` | An App Store Connect API key with the Developer role |
| `MUXFLOW_ANDROID_KEYSTORE_BASE64`, `MUXFLOW_ANDROID_KEYSTORE_PASSWORD`, `MUXFLOW_ANDROID_KEY_ALIAS`, `MUXFLOW_ANDROID_KEY_PASSWORD` | The Android release keystore |

# Linux internal release

## Architectures and artifacts

`release/linux/build-package.sh` creates a deterministic rootless archive for
the requested architecture. The archive contains the desktop, a Debian 12
baseline remote helper, desktop integration, installer/uninstaller, and a
SHA-256 manifest. Docker is required for the conservative helper build unless a
separately verified `ADE_HOST_BINARY_OVERRIDE` is supplied.

```sh
SOURCE_DATE_EPOCH=1704067200 release/linux/build-package.sh x86_64
release/linux/verify-package.sh tmp/release/muxflow-*-linux-x86_64.tar.gz
```

ARM64 uses `aarch64-unknown-linux-gnu` and requires the Rust target, an ARM64
linker, and an ARM64 GTK/WebKitGTK sysroot. If those prerequisites are absent,
the script exits 69 with an `ARM64_BLOCKER` message. Until a verified artifact
exists, ARM64 remains an unverified release lane; an x86-only environment must
never relabel x86 output.

## Reproducibility

Set `SOURCE_DATE_EPOCH` to a fixed release epoch. The package uses sorted paths,
fixed mtimes, numeric ownership, deterministic gzip headers, Rust path remapping,
and an internal file manifest. The builder also reorders Vite's concurrently
written asset tree before Tauri embedding. `tests/release/run-package.sh` builds
from two different checkout and target paths, requires identical archive
digests, runs the packaged helper on Debian 12, and then exercises confined
clean install, upgrade, rollback, and uninstall from an isolated home/prefix.

## Manual Linux package checklist

1. Capture the exact source-tree digest and package version.
2. Run `pnpm test:release` and the bounded `pnpm test:release:matrix`.
3. Run `pnpm test:release:heavy` for a release candidate.
4. Run packaged `cua-virtual-driver` release journeys.
5. Build twice and compare x86-64 package digests.
6. Build/test ARM64 natively, or record the exact unavailable-runner blocker.
7. Manually exercise the packaged Linux app and verify clean install, core use,
   upgrade, uninstall, hook lifecycle, and diagnostics.
8. Confirm no test containers, tmux servers, daemons, SSH masters, or CUA
   sessions remain; retain only evidence under ignored `tmp`.

The Linux package is not code-signed; its integrity comes from the published
SHA-256 sums.

# macOS release

Run `release/macos/build-package.sh` on an Apple-Silicon Mac with Xcode,
Node 24, pnpm 11, Rust 1.97.1, and Docker. The script creates an arm64 `.app`
and DMG, embeds a native Mach-O local helper, builds separate Debian 12 Linux
ELF helpers for `aarch64` and `x86_64` (or takes them prebuilt from
`MUXFLOW_LINUX_HELPERS_DIR`, which needs no Docker), and runs
format/architecture/package verification. Use `release/macos/install.sh` for a
transactional install or upgrade into `/Applications` and
`release/macos/uninstall.sh` for confined removal. With no argument the
installer publishes the bundle it just built;
`ADE_MACOS_APPLICATIONS_DIR="$HOME/Applications"` selects a rootless per-user
install instead.

Signing is chosen by `MUXFLOW_MACOS_SIGNING_IDENTITY` (see `.env.example`):

- Unset: an ad-hoc seal. Native services such as notifications can identify
  the bundle, but it carries no publisher trust and Gatekeeper rejects a
  quarantined copy. This is the local development build.
- A `Developer ID Application` identity from the keychain, with the App Store
  Connect API key in `MUXFLOW_NOTARY_KEY_PATH`, `MUXFLOW_NOTARY_KEY_ID` and
  `MUXFLOW_NOTARY_ISSUER`, and the team in `MUXFLOW_APPLE_TEAM_ID`: the helper
  and then the app are signed with the hardened runtime and a secure
  timestamp, the app is notarized and stapled, and the DMG is then signed,
  notarized and stapled too. `verify-package.sh` then requires the pinned
  team, the hardened runtime and timestamp on both Mach-O binaries, Gatekeeper
  acceptance and a stapled ticket.

Published releases currently use the ad-hoc seal. The Developer ID path is
ready but unexercised until the Apple credentials exist; adding them to the
`release` environment is all it takes.

No entitlements are requested: the app loads no unsigned code, WebKit runs
JIT in its own processes, and the desktop does not use the microphone. The
package remains `APPLE_SILICON_ONLY`.
