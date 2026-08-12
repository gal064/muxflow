# Linux internal release

## Architectures and artifacts

`release/linux/build-package.sh` creates a deterministic rootless archive for
the requested architecture. The archive contains the desktop, a Debian 12
baseline remote helper, desktop integration, installer/uninstaller, and a
SHA-256 manifest. Docker is required for the conservative helper build unless a
separately verified `ADE_HOST_BINARY_OVERRIDE` is supplied.

```sh
SOURCE_DATE_EPOCH=1704067200 release/linux/build-package.sh x86_64
release/linux/verify-package.sh tmp/release/tmux-agent-ide-*-linux-x86_64.tar.gz
```

ARM64 uses `aarch64-unknown-linux-gnu` and requires the Rust target, an ARM64
linker, and an ARM64 GTK/WebKitGTK sysroot. If those prerequisites are absent,
the script exits 69 with an `ARM64_BLOCKER` message. The workflow defines a
native ARM64 job, but it is not evidence of a successful run by itself. Until a
retained artifact exists, ARM64 remains an explicit unverified release lane;
an x86-only environment must never relabel x86 output.

## Reproducibility

Set `SOURCE_DATE_EPOCH` to a fixed release epoch. The package uses sorted paths,
fixed mtimes, numeric ownership, deterministic gzip headers, Rust path remapping,
and an internal file manifest. The builder also reorders Vite's concurrently
written asset tree before Tauri embedding. `tests/phase8/run-package.sh` builds
from two different checkout and target paths, requires identical archive
digests, runs the packaged helper on Debian 12, and then exercises confined
clean install, upgrade, rollback, and uninstall from an isolated home/prefix.

## Gate separation

Every push runs formatting, warning-free Clippy, Rust/frontend tests, builds,
bounded parser fuzzing, and deterministic Phase 0–8 contracts. The default
`pnpm phase8:matrix` still uses the real local/SSH desktop-manager paths but
bounded 16/64 MiB payloads. Scheduled or manual heavy jobs run
`pnpm phase8:release-matrix` with shaped Docker SSH, scale/fault matrices, exact
5 GiB transfers, and release reproduction. One exact upload/download pair is
about 10 GiB; exercising both driver and desktop-manager paths is about 20 GiB
per local/SSH gate (about 40 GiB for both), and shaped SSH commonly takes
15–20 minutes. The packaged virtual-X11/CUA journey is a
separate manual release gate because the hosted workflow does not provide the
project's persistent accessibility-enabled desktop. Heavy gates are not
silently replaced by smaller payloads.

## Release checklist

1. Capture the exact source-tree digest.
2. Run `pnpm phase8:regressions` and the bounded `pnpm phase8:matrix`.
3. Run `pnpm phase8:release-matrix` once for a release candidate, or reuse a
   passing nightly result only when its transfer-component digest and contract
   version match exactly.
4. Run packaged `cua-virtual-driver` release journeys.
5. Build twice and compare x86-64 package digests.
6. Build/test ARM64 natively, or record the exact unavailable-runner blocker.
7. Verify clean install, upgrade, uninstall, hook lifecycle, and diagnostics.
8. Confirm no test containers, tmux servers, daemons, SSH masters, or CUA
   sessions remain; retain only evidence under ignored `tmp`.

The Linux internal release is unsigned. Signing/notarization and all macOS
packages are deferred with the user-approved Linux-only scope.
