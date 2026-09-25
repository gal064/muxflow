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

The Linux internal release is unsigned. The macOS internal release below uses
only an ad-hoc code identity so native macOS services can identify the bundle;
Developer ID signing and notarization remain outside the internal scope.

# macOS internal release

Run `release/macos/build-package.sh` on a physical Apple-Silicon Mac with Xcode,
Node 24, pnpm 11, Rust 1.97.1, and Docker. The script creates an arm64 `.app`
and DMG, embeds a native Mach-O local helper, builds separate Debian 12 Linux
ELF helpers for `aarch64` and `x86_64`, and runs format/architecture/package
verification. Use `release/macos/install.sh` for a transactional install
or upgrade into `/Applications` and `release/macos/uninstall.sh` for confined
removal. With no argument the installer publishes the bundle it just built;
`ADE_MACOS_APPLICATIONS_DIR="$HOME/Applications"` selects a rootless per-user
install instead.

This artifact remains `UNSIGNED_INTERNAL` in the distribution sense and is
`APPLE_SILICON_ONLY`: its ad-hoc identity provides no publisher trust.
Gatekeeper may reject a quarantined copy; Developer ID signing, notarization,
hardened-runtime entitlement, universal-binary, and Intel runtime claims require
their own configured release credentials and physical gates.
