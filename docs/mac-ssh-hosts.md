# macOS SSH hosts: plan

This plan lets the desktop app connect over SSH to a Mac, the same way it
connects to a Linux machine today. The host helper already runs on macOS: the
Mac app starts it for Local, and every OS-specific path in `apps/host`
(process identity, agents, files, git, hooks, staging) has a macOS arm. What is
Linux-only is the **remote install path**:

- which helper the desktop uploads;
- the probe and install shell scripts;
- the release packaging.

The wire protocol does not change, so mobile is untouched and `PROTOCOL_MAJOR`
does not move.

## Decisions

- **Apple Silicon only.** The first version supports `arm64` Macs. An Intel
  Mac gets a clear refusal ("Intel Macs aren't supported as SSH hosts"), not a
  broken install. Adding `x86_64` later is one more build target and artifact,
  with no design change.
- **One remote artifact per OS and architecture.** The desktop picks the
  artifact by the probed `uname -s` and `uname -m`, never by architecture
  alone.
- **Reuse the Rust process checks; delete the shell copy.** The install
  scripts carry a `/proc`-based copy of the daemon-retirement check that
  already exists, portably, in `daemon::retire_verified`. The plan removes the
  shell copy instead of writing a second, macOS shell copy.
- **Same install location and runtime directory** as Linux:
  - the helper goes to `$HOME/.local/bin/muxflow-host`;
  - the socket goes in `/tmp/muxflow-$UID`.

  Nothing is installed system-wide, and no `launchd` agent is added: the
  daemon is started on demand by `bridge`/`restart`, as it is on Linux.

## Changes

### 1. Helper selection (desktop, `connection/helper.rs`)

- `helper_artifact_for_arch(arch)` becomes `helper_artifact(probe)`. It reads
  `operatingSystem` (`uname -s`) and `architecture` from the probe, which
  already reports both.
- The packaged file names are `muxflow-host-{linux,macos}-{arch}`, beside the
  executable (Linux package) or in `Contents/Resources` (DMG). The Linux
  package used to name its helpers `muxflow-host-{arch}`, which the desktop's
  `muxflow-host-linux-{arch}` lookup never found. The Linux package, its
  installer, uninstaller and verifier now use the same names as the DMG.
- **Reuse the app's own helper when the host matches it.** Today a Linux
  desktop reuses its own helper for a Linux host of the same architecture.
  The new rule: reuse `host_helper_path()` when the desktop's OS and
  architecture both equal the host's. A Mac app connecting to an arm64 Mac
  therefore uploads its own signed sidecar, so the DMG does not grow.
- **Test overrides.** The variables are renamed to match:
  `ADE_HOST_HELPER_{LINUX,MACOS}_{X86_64,AARCH64}_PATH`. Nothing set the old
  names.
- No `--expected-os` flag: `helper install` reads the OS from the artifact's
  own header and compares it with the probe.

### 2. Install and probe (host, `remote_helper.rs`)

- **Binary check.**
  - `elf_architecture` becomes `executable_target(path) -> (os, arch)`.
  - It reads the ELF header as today, plus the 64-bit Mach-O header: magic
    `0xfeedfacf`, CPU type `0x0100000c` for arm64 and `0x01000007` for x86_64.
  - Fat or universal binaries are refused, since we never ship one.
  - The existing install refusals are then checked against the artifact's own
    OS: wrong OS, wrong architecture, and tmux older than 3.3.
- **Remove the Linux-only gate** at `install`: the
  `operating_system != "Linux"` bail becomes "the artifact's OS must match
  the host's", with only Linux and Darwin accepted.
- **Digest on the host** (probe and post-upload check). Replace
  `sha256sum FILE | cut -d' ' -f1` with:

  ```sh
  if command -v sha256sum >/dev/null 2>&1; then sha256sum FILE; else shasum -a 256 FILE; fi | cut -d' ' -f1
  ```

  Every macOS ships `shasum`, and both commands print the same format.
- **Daemon restart and rollback scripts.** Delete the shell fallback that
  parses `daemon.json` and reads `/proc/$pid/stat` and `/proc/$pid/exe`.
  Instead, `daemon-stop` itself falls back to `daemon::retire_verified` when
  cooperative shutdown fails. That is the same identity-checked SIGTERM, and
  it already has Linux and macOS arms (`proc_pidinfo`, `proc_pidpath`). Both
  scripts shrink to:

  ```sh
  if [ -S "$socket" ]; then {final_path} daemon-stop --force >/dev/null; fi
  ```

  `--force` is opt-in, so the plain `daemon-stop` other callers use is
  unchanged. The bridge's own replace-a-different-build path uses the same
  `daemon::stop_or_retire`.

  The socket-wait, `nohup` start and `protocol-check` steps stay unchanged.
  The rest of each script was checked for BSD userland:
  - `install -d -m`, `seq`, `sleep 0.05`, `sed -n`, `mkdir` (the lock), `mv`,
    `cp -p` and `nohup` all behave the same on macOS;
  - `awk` and `readlink` go away with the fallback.
- **tmux on a non-interactive Mac PATH.** Add `/opt/homebrew/bin/tmux`
  (Homebrew), `/opt/local/bin/tmux` (MacPorts) and `/opt/pkg/bin/tmux`
  (pkgsrc) to the probe's candidates, after the Linux ones so a Linux host
  checks exactly what it did before.
  This matches `tmux-control`'s macOS list in
  `crates/tmux-control/src/executable.rs`. `sshd` on macOS runs commands with
  `/usr/bin:/bin:/usr/sbin:/sbin` and no Homebrew. The doc comment changes
  from "Linux candidates" to "Linux and macOS candidates".

### 3. Port detection (desktop, `connection/ports.rs`)

macOS has no `ss`. The detect command becomes:

```sh
ss -ltnHp 2>/dev/null || lsof -nP -iTCP -sTCP:LISTEN -Fcn
```

`parse_listening_ports` gains an `lsof -F` branch:

- `c` lines give the process name;
- `n` lines give the address and port;
- the same filters apply: port 1024 or above, loopback or wildcard only.

Without this step, the Ports tab still forwards typed ports on a Mac. Only the
detected-port chips are missing, which the panel already handles.

### 4. Release (`.github/workflows/release.yml`, `release/`)

Today the flow is linux → macos, because the DMG embeds the Linux helpers.
Linux packages now also need the Mac helper, so the order becomes:

```
linux (x86_64, aarch64) ──► macos ──► linux-universal ──► publish
                                └─ exports the signed muxflow-host
```

- **`macos` job.** After `build-package.sh` signs
  `Contents/MacOS/muxflow-host`, it copies those exact bytes out as the
  artifact `muxflow-host-macos-aarch64`. The copy is taken from the finished,
  signed and stapled app the DMG is built from, so its bytes are the
  sidecar's.
- **`linux-universal` job.**
  - It gains `needs: macos`.
  - `add-cross-helper.sh` (or a sibling) adds `muxflow-host-macos-aarch64`
    beside the cross-arch Linux helper.
  - `verify-package.sh` checks its Mach-O header and digest.
- **Linux `install.sh`/`uninstall.sh`.** The installers place and remove the
  new file wherever the cross helper goes today.
- **`docs/release.md` and `docs/remote-host.md`.** List macOS hosts and their
  requirements:
  - Remote Login enabled;
  - tmux 3.3+ from Homebrew or MacPorts;
  - Apple Silicon.

## Same-digest coexistence (known limit)

A daemon retires any other daemon at its socket that isn't the same build
(`bridge.rs` `daemon_is_current_build` compares executable digests). Suppose a
Mac runs the Muxflow app *and* is an SSH host for another machine. Both helpers
then share one socket, and they work together only when their digests match:

- The Mac app's sidecar and the uploaded `muxflow-host-macos-aarch64` are the
  same bytes (step 4), so **two machines on the same version coexist**.
- When the versions differ, each side retires the other's daemon whenever it
  reconnects. tmux sessions survive, since tmux is its own server, but panes
  reattach. Linux has the same limit today with a Linux desktop SSH'd into
  from another machine.
- This plan does not fix version skew. It documents it in
  `docs/remote-host.md`: keep both machines on the same release.

## macOS-only behaviour to verify, not code

- **TCC (privacy prompts).** A daemon started from an SSH session inherits
  `sshd`'s privacy grants. Without Full Disk Access for `sshd-keygen-wrapper`,
  reading `~/Documents`, `~/Desktop` or `~/Downloads` fails with EPERM. Files
  and git there show an error; everything else works. To do: document the
  System Settings → Privacy → Full Disk Access step in
  `docs/remote-host.md`. Nothing to build.
- **Daemon survives SSH logout.** The daemon is started with `nohup` and is
  not in the SSH session's process group, the same as on Linux. To do:
  confirm on a real Mac that it outlives the SSH connection and a screen lock.
- **Gatekeeper.** Files written by `scp`/`sftp` carry no quarantine
  attribute, so the signed helper runs without a prompt. To do: confirm on a
  real Mac, with both an ad-hoc signed build and a Developer ID build.
- **git without the Command Line Tools.** `/usr/bin/git` is a shim that opens
  an install dialog. The probe already reports `unavailable` when
  `git --version` fails, and the Git tab already handles a host without git.

## Test results (2026-10-05)

A test run on macOS 26.5.1 (arm64) connected to itself with `ssh localhost`
through Remote Login. It used the 0.1.10-rc.1 app's own helper, kept in an
isolated runtime folder.

- **Copied over `scp`:** the copy has no quarantine attribute, runs, and has
  the same digest as the app's helper.
- **The SSH session's PATH** was `~/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin`.
  On that PATH:
  - `tmux` is **missing**, though `/opt/homebrew/bin/tmux` (3.7b) exists, so
    the Homebrew candidate is required;
  - `ss` is missing and `lsof` is present;
  - `/sbin/sha256sum` exists on macOS 26 and `shasum` is in `/usr/bin`. The
    fallback still covers older macOS.
- **The daemon outlived the SSH session:** it was started with `nohup` over
  SSH, and 5 s after the connection closed it still answered
  `protocol-check`. `daemon-stop` then stopped it.
- **Privacy:** `~/Documents`, `~/Desktop`, `~/Downloads` and `~/Pictures`
  were all readable over SSH, with "Allow full disk access for remote users"
  **on**. The run with it off is still to do; until then, the docs should
  tell users to turn it on.
- **Uploaded from Linux over the tailnet:**
  - the app's helper (Mach-O arm64, ad-hoc signed) was copied from Linux to
    the Mac with `scp`;
  - it arrived with no quarantine and runs;
  - its digest matches;
  - `/sbin/sha256sum` on macOS 26 prints the same `digest  path` format as
    GNU, so `cut -d' ' -f1` works with it and with `shasum -a 256`.
- **Tailscale SSH** can't serve a Mac running the Tailscale GUI app: "The
  Tailscale SSH server does not run in sandboxed Tailscale GUI builds". A Mac
  host needs Remote Login, though the connection can still go over the
  tailnet.

### 0.1.11-rc.1 helper on the Mac (CLI, Linux to Mac)

- The RC's Linux helper installed the RC's Mac helper in 8.3 s. Installing
  the same build again took 1 s, and `doctor` found Homebrew tmux 3.7b, which
  is not on PATH.
- **Forced retire after replacement works.** The daemon was frozen with
  SIGSTOP the moment the install lock appeared, so cooperative shutdown timed
  out. `retire_verified` then matched the start time and the
  `proc_pidpath` of the old daemon even though `mv` had already replaced its
  file. It sent SIGTERM, which ends a stopped process on macOS immediately,
  and the upgrade passed in 9.5 s with no rollback.
- **Found: `ControlPersist` in ssh_config broke connect.** With
  `ControlPersist` set in the user's ssh_config, OpenSSH daemonized the
  control master. The app took the exited child for a dead master, unlinked
  its socket, never offered the helper install, and leaked one master per
  reconnect. Both master spawns now pass `-o ControlPersist=no`. This affects
  Linux hosts too.

## Tests

- **Rust, host:**
  - `executable_target` reads ELF x86_64/aarch64 and Mach-O arm64/x86_64, and
    refuses fat binaries and junk;
  - install refuses an OS mismatch (Linux artifact to Darwin, and the
    reverse);
  - the probe script contains the `shasum` fallback and the Homebrew
    candidate;
  - `daemon-stop` falls back to `retire_verified` when cooperative shutdown
    fails. This reuses the existing daemon test fixtures.
- **Rust, desktop:** `helper_artifact(os, arch)`:
  - reuses the app's own helper only when OS and architecture match;
  - picks the packaged `macos`/`linux` file otherwise;
  - errors for Intel Macs.
- **Ports** (if step 3 is done): an `lsof -F` fixture parses to the same
  `DetectedPort` list as the `ss` fixture.
- **CI (deferred, see Todo).** There is no `sshd`-based install test today. Add one small
  job on `macos-15`. It enables a localhost `sshd` with a throwaway key, then
  runs `muxflow-host helper install localhost` twice: once to install, once to
  upgrade with `--allow-upgrade`. Finally it runs `protocol-check`. This
  covers install, upgrade and restart against a real Darwin `sshd`.
  Optional: run the same job on `ubuntu-24.04` to guard the Linux path.
- **Manual QA on a real Mac:**
  - Linux desktop → Mac: install, terminal, Files, Git, Ports, agents;
  - Mac desktop → Mac: the same flow, plus coexistence with the local app on
    the same version;
  - kill the SSH connection: the daemon survives and a reconnect reattaches.

## Performance and impact on Linux

| Area | Change on Linux | Cost |
| --- | --- | --- |
| Daemon at runtime (terminal, files, git, agents) | None. OS differences are `cfg`-gated at compile time; the Linux daemon's code paths are byte-for-byte the same logic. | 0 |
| Helper probe (each connect) | One `command -v sha256sum` (a shell builtin) before hashing. The extra tmux candidates are only checked when tmux is not on PATH. | microseconds |
| Install / upgrade | The restart fallback moves from shell to the Rust `retire_verified`. The happy path is unchanged; the failure path does one fewer SSH round-trip of parsing. | same or faster |
| Port detection | `ss` runs first exactly as today; `lsof` runs only if `ss` fails. | 0 |
| Linux package size | Adds the Mac helper (~11 MB, ~3.7 MB compressed): tarball ~22 MB → ~26 MB (+17%), installed size +11 MB. | download / disk |
| DMG size | None for arm64 Macs: the Mac → Mac case reuses the sidecar. | 0 |
| Release wall time | `linux-universal` now waits for `macos`; it is a ~2 minute job. | ~+2–3 min |

There is no change to Linux behaviour that a user would notice, apart from the
larger download.

## RCC

**Confidence: 88.**
- The Mac test (see Test results) settled most of the unknowns: Gatekeeper,
  the daemon outliving SSH, the PATH and its tools, and that the app's own
  helper works when uploaded.
- What's left:
  - whether the full disk access option was off during the privacy check;
  - long-lived daemon behaviour across sleep and lock;
  - the real `helper install` flow, which is covered by implementation plus
    the `sshd` CI job.

**Risk: 30.**
- The install/restart scripts are the most delicate path in the product, and
  every Linux upgrade goes through them.
- Moving the fallback into `retire_verified` replaces shell that is lightly
  tested with Rust that is unit-tested, but it is still a change to how every
  Linux upgrade restarts.
- The release reordering can break a release run, though not users.
- How to lower it:
  - land step 2 (scripts) on its own and run it through an RC against Linux
    hosts first;
  - add the `sshd` install test (see Tests) and run it on Linux as well.

**Complexity: 20.**
- Net code is roughly neutral. Two copies of the shell `/proc` fallback (the
  densest part of the install scripts) are deleted. Added: a small Mach-O
  header check, an OS dimension in artifact selection, two tmux paths, a
  digest fallback, and optionally an `lsof` parser of about 40 lines.
- No new mechanism, protocol or state.
- The release graph gains one dependency edge and one artifact.

## Estimate

| Step | Size |
| --- | --- |
| 1–2 code and tests | ~1 day |
| 3 (Ports `lsof`, optional) | ~2 hours |
| 4 release and docs | ~½ day, plus one RC run |
| Manual QA on a Mac | ~½ day; needs a Mac with Remote Login on |

## Test Mac access and cleanup

The test Mac (`gals-macbook-pro`, user `galvered`) stays reachable over SSH
from the Linux dev machine until manual QA of the RC is done:
- Remote Login is on, with full disk access for remote users on.
- The throwaway key `muxflow-mac-spike` is in `~/.ssh/authorized_keys`.

**Only after manual QA of the RC is finished**, clean up:

1. On the Mac, delete the test files:
   `rm -rf ~/.muxflow-spike ~/muxflow-spike.sh ~/muxflow-spike-report*.txt; ssh-keygen -R localhost`.
2. On the Mac, remove the test key:
   `sed -i '' '/muxflow-mac-spike$/d' ~/.ssh/authorized_keys`.
3. If the RC installed a helper on the Mac and the Mac isn't kept as a host,
   run `~/.local/bin/muxflow-host daemon-stop`, then delete
   `~/.local/bin/muxflow-host`.
4. If the Mac isn't kept as an SSH host, turn Remote Login off in System
   Settings → General → Sharing.
5. On the Linux machine, delete the test key, `ssh_config` and
   `known_hosts`. They are in the session scratchpad (`mac-spike/`) and the
   control socket folder is `/tmp/claude-1000/mfs`.

## Out of scope

- Intel Mac hosts (one target to add later).
- Version-skew coexistence between a Mac's own app and a remote desktop.
- A `launchd` agent, or starting the daemon at login.
- BSD and other Unix hosts.

## Todo

- **Phone → a Mac that only runs the app.** Mobile, like every SSH client,
  runs `$HOME/.local/bin/muxflow-host bridge --stdio`. A Mac that has only
  used the app's Local connection has no helper there, so the phone reports
  "muxflow-host isn't installed on this host". Today's workaround is to
  connect to the Mac once from a desktop over SSH and install the helper
  (see `docs/troubleshooting.md`). The fix is for the Mac app to place its
  own helper at `~/.local/bin/muxflow-host` (same bytes, so one shared
  daemon), or for mobile to fall back to the app's bundled helper.
- **`sshd` install test in CI.** A `macos-15` job that turns on a localhost
  `sshd`, then runs `helper install` (fresh, current, `--allow-upgrade`) and
  `protocol-check`. Deferred from the first version: getting `sshd` up on a
  hosted runner needs iteration, and manual QA against a real Mac covered the
  same path.
- **git without the Command Line Tools.** On such a Mac, `/usr/bin/git` is a
  shim that may pop the "install developer tools" dialog on the Mac's screen
  when the probe or the Git tab runs git over SSH. To do: confirm, and if so
  check `xcode-select -p` before running git on Darwin.
- **Privacy with full disk access off.** Confirm what Files and Git show on a
  Mac host when "Allow full disk access for remote users" is off.
