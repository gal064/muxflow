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

The Linux internal release is unsigned. The macOS internal release below uses
only an ad-hoc code identity so native macOS services can identify the bundle;
Developer ID signing and notarization remain outside the internal scope.

# macOS internal release

Run `release/macos/build-package.sh` on a physical Apple-Silicon Mac with Xcode,
Node 24, pnpm 11, Rust 1.97.1, and Docker. The script creates an arm64 `.app`
and DMG, embeds a native Mach-O local helper, builds separate Debian 12 Linux
ELF helpers for `aarch64` and `x86_64`, and runs format/architecture/package
verification. Use `release/macos/install.sh` for a rootless transactional
install or upgrade and `release/macos/uninstall.sh` for confined removal.

This artifact remains `UNSIGNED_INTERNAL` in the distribution sense and is
`APPLE_SILICON_ONLY`: its ad-hoc identity provides no publisher trust.
Gatekeeper may reject a quarantined copy; Developer ID signing, notarization,
hardened-runtime entitlement, universal-binary, and Intel runtime claims require
their own configured release credentials and physical gates.

## Current Phase 10 candidate status

macOS QA has run end to end and is not yet closed. The current Apple-Silicon
candidate is the third 2026-08-13 rebuild, **not** the 2026-08-12 build the
earlier text referred to: QA found and fixed three defects, and each fix forced
a rebuild.

| Item | Value |
|---|---|
| DMG SHA-256 | `6df2176a47207f4fe6c15570401383977043e411ad06ea48d14e3bf8853e358f` |
| Desktop Mach-O SHA-256 | `e016a739ad76f37b3f9ea61d846c6b1bbe077c464980a53da3c37a5f867d90a6` |
| Local host Mach-O SHA-256 | `ea27f5981052caf4cff460988dc6500496745cdfd157dcf60fb578fc9cbc394e` |
| Linux aarch64 helper SHA-256 | `1bef279fe78861fc91f140dc35ceb0c0d3ad0a554289e1d26edc0ae3eb5a2a19` |
| Linux x86-64 helper SHA-256 | `53b57efc944f4cfbf5df5bf36f47ad6969f2648295ea0c9c8bb7284337d20405` |
| Source manifest | `08236c2662bf821f1ae2dd0a333624f14d517fd1b83f66ff77c49ce2cc0ce3c3` over 739 files |

Every artifact digest moved, which is correct rather than alarming: the M10-E058
fix adds a socket-path length check in `apps/host/`, and that is the source the
local host binary and both Linux helpers are built from. Helper reproducibility
(M10-E048) was re-proven against the new source — an independent rebuild of the
aarch64 helper is byte-identical to the shipped one.

The consequence is that gates previously bound to the old host and helper
digests are no longer bound, and the physical macOS rows were executed against
an earlier desktop binary. Those are re-run before Phase 10 closes; see the QA
ledger for exactly which.

The three defects fixed during QA are worth knowing about because the first two
were total rather than intermittent on macOS:

- **Paste from another application did not work at all.** Text copied inside the
  app pasted; text copied anywhere else silently did nothing, because the native
  pasteboard reader handled files and images but not plain text and the WebKit
  fallback refuses foreign clipboard content. Fixed in `native_clipboard.rs` and
  the terminal transfer surface; bounded to the 1 MiB terminal input limit.
- **`View > Toggle Full Screen` never entered macOS full screen.** It resized the
  window under the menu bar instead, because `tao` never opts the window into
  `NSWindowCollectionBehaviorFullScreenPrimary`. Fixed in `macos_window.rs`.
- **A failing connection threw away the reason.** The bridge child's standard
  error was discarded, so a handshake failure surfaced as a bare message with
  nothing to act on. It is now captured, bounded to 2 KiB and drained on its own
  thread so the pipe cannot stall the child, and appended to the error. The same
  fix adds the missing socket-path length check on the daemon's own socket —
  over the limit, the kernel's refusal never named the limit or the length.

Full evidence, including the disposition of every acceptance row and the limits
that remain, is in
[`tests/phase10/qa-macos-2026-08-12.md`](../tests/phase10/qa-macos-2026-08-12.md)
and [`tests/phase10/findings.md`](../tests/phase10/findings.md). 