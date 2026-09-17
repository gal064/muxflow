# Post-V1 Implementation Plan: Phases 9–11

## Context

Linux V1 is complete through Phase 8. The next work has three goals:

1. Remove the retained nested-tmux machinery now that nested tmux is explicitly unsupported.
2. Make the desktop genuinely ready for day-to-day use and release QA on macOS while preserving Linux behavior.
3. Polish the UI based on hands-on user critique after the first two phases, rather than guessing at a redesign now.

Phase 9 is complete. Phase 10 implementation and its five-review cycle are
complete; final physical QA is pending against the digest-bound macOS candidate
recorded in `tests/acceptance/macos/implementation-handoff.md`. Phase 11 is intentionally
open and must not begin until Phase 10 acceptance closes, the user tests the
post-Phase-10 app, and a concrete UI brief is approved.

### Current Phase 10 continuation point — 2026-08-12

A fresh session must not repeat preflight, baseline implementation, or the five
reviews. Start by reading, in order:

1. `tests/acceptance/macos/implementation-handoff.md` — exact completed work, accepted
   Git trust boundary, passing source gates, candidate paths/digests, and limits;
2. `tests/acceptance/macos/qa-macos-2026-08-12.md` — pending acceptance ledger;
3. `tests/acceptance/macos/findings.md` — physical findings whose final rebuilt-package
   retests remain open; and
4. `tests/acceptance/macos/README.md` — exact QA run order, safety boundaries, and cleanup.

Stages 10.0–10.2 are complete. Resume at the project-specific native-readiness
check and Stage 10.3. Use the CuaDriver CLI for every GUI action and screenshot,
visually inspect each retained capture, warn the user immediately before the
authorized sleep/wake action, keep remote Linux host read-only, and stop before Phase 11.

## Baseline and invariants

- The Phase 8 Linux release remains the behavioral baseline.
- A host is still one local machine or one OpenSSH target connected to one tmux server.
- A tmux session remains a workspace, a tmux window remains a terminal tab, and a tmux pane remains a terminal split.
- Codex and Claude Code lifecycle, notification routing, Explorer, editor, Markdown, Git, upload/download, reconnect, and tmux synchronization must not regress.
- Nested tmux is unsupported. Running `tmux` inside a managed pane may display ordinary terminal output, but the app will not discover, route, label, or control the inner server.
- Existing remote Linux helper compatibility and explicit-upgrade behavior must remain safe.
- Verification must use disk-backed `tmp/work`, shared bounded caches, compact evidence, and cleanup traps. The normal gate uses small representative transfers; exact 5 GiB transfer tests remain manual/nightly/release-only and run only when transfer components change.
- Each closed phase follows the established development cycle: scoped implementation, deterministic QA, no more than five independent high-rigor reviews, fixes for all accepted findings, append-only `implementation.md` evidence, and cleanup. Phase 11 does not enter this cycle until its brief is approved.

## Phase 9 — Remove nested-tmux support machinery

**Status:** Completed on Linux on 2026-08-11. The historical QA record
`tests/acceptance/linux/qa-linux-2026-08-11.md` is not in the public source snapshot.

### Outcome

Delete the approximately 300–400 lines of cross-layer nested-routing behavior and its associated tests/documentation. Preserve only a generic fail-closed rule: an agent hook or action that cannot be matched exactly to the active server topology is reported as unmapped and is never guessed, rerouted, or replayed.

### 9.1 Freeze the migration contract

1. Inventory every persisted and wire-level nested field before editing.
2. Remove nested semantics from the current protocol and reserve the removed protobuf field numbers and names so they cannot be accidentally reused:
   - `Pane.nested_tmux`, `Pane.outer_pane_id`, and `Pane.nested_provenance`.
   - `AgentRoute.nested_provenance` and nested-only reported-pane semantics.
   - `AgentHookEvent.outer_pane_id` and `AgentHookEvent.nested_provenance`.
   - `NestedProvenance` when no remaining message uses it.
3. Bump the protocol/helper compatibility version because current clients and helpers no longer share the same state model. Older helpers must enter the existing read-only/incompatible flow and require the existing explicit upgrade path; no silent mutation is allowed.
4. Migrate persisted desktop/host agent records by discarding obsolete nested metadata and rebuilding route identity from the authoritative current tmux snapshot. Unknown old routes remain safely unmapped.

Critical files:

- `crates/protocol/proto/envelope.proto`
- `crates/protocol/src/`
- `apps/desktop/src-tauri/src/connection.rs`
- `apps/desktop/src-tauri/src/connection/agent.rs`
- `apps/host/src/service/agents/store.rs`

### 9.2 Simplify tmux discovery

1. Remove `/proc`-based inner-tmux command/environment detection, outer-pane resolution, and nested provenance from discovery.
2. Return only topology that belongs to the connected tmux server.
3. Remove nested marker injection from shell/agent/tmux action launch paths.
4. Keep normal active pane path, command, layout, rename, split, and reconnect behavior unchanged.

Critical files:

- `crates/tmux-control/src/discovery.rs`
- `crates/tmux-control/src/layout.rs`
- `crates/tmux-control/src/lib.rs`
- `apps/host/src/service/snapshot.rs`
- `apps/host/src/service/tmux_actions.rs`

### 9.3 Replace nested routing with exact routing

1. Accept a hook pane ID only when it exists in the current connected server identity and authoritative topology.
2. Preserve server/session/window/pane fallback data used for rename and reconnect, but never translate an inner pane to an outer pane.
3. Keep the generic unmapped-agent state and in-app visibility for stale, missing, or foreign routes; notification activation must degrade safely without focusing a guessed pane.
4. Remove nested-specific identity, deduplication, liveness grace, reconciliation, hook validation, and snapshot branches.
5. Ensure launch/resume creates agents only in explicitly selected outer tmux windows/panes.

Critical files:

- `apps/host/src/hook.rs`
- `apps/host/src/service/agents/identity.rs`
- `apps/host/src/service/agents/reconcile.rs`
- `apps/host/src/service/agents/snapshot.rs`
- `apps/host/src/service/agents/mod.rs`
- `apps/host/src/service/requests/agent_dispatch.rs`

### 9.4 Remove desktop presentation and routing branches

1. Remove nested badges, labels, CSS, wire types, and pane-routing branches.
2. Route terminal tabs, agent attention, and notification activation through the ordinary exact pane identity path.
3. Remove nested-only frontend state and tests while retaining tests for stale and unmapped routes.

Critical files:

- `apps/desktop/src/app/TerminalWorkspaceSurface.tsx`
- `apps/desktop/src/app/paneRouting.ts`
- `apps/desktop/src/app/paneRouting.test.ts`
- `apps/desktop/src/app/types.ts`
- `apps/desktop/src/features/agents/useAgentNotificationActivation.test.tsx`
- related component styles

### 9.5 Remove or rewrite tests and documentation

1. Delete nested-specific fixtures and assertions from protocol, tmux-control, Phase 2, Phase 6, hook, routing, and notification tests.
2. Add replacement coverage for:
   - exact direct-pane routing;
   - a stale/foreign pane remaining unmapped;
   - notification activation refusing an unmapped target;
   - protocol mismatch and explicit helper upgrade;
   - persisted old agent state loading without stale nested routing;
   - local and delayed-SSH external tmux create/rename/split/reorder/close synchronization.
3. Remove `docs/nested-tmux.md`. Update the PRD, technical plan, setup/release docs, traceability, and implementation ledger to say only that nested tmux is unsupported; do not retain implementation guidance for it.
4. Require a final source scan showing no production nested-tmux identifiers or marker injection remains. Historical evidence may retain old wording because it is immutable evidence.

### Phase 9 verification and acceptance

Must pass:

1. Protocol generation, Rust format/Clippy/all-target tests, frontend typecheck/tests/build.
2. Focused protocol compatibility and persisted-state migration tests.
3. Local and 100 ms delayed-SSH tmux lifecycle tests, including bidirectional session/window rename synchronization.
4. Codex and Claude fixture flows for launch, hook state, attention, notification activation, reconnect, and unmapped-route failure.
5. Phase 0–8 bounded regression suite, excluding exact 5 GiB transfer lanes because transfer code is not in scope.
6. Packaged Linux smoke through `cua-virtual-driver`: attach, create/rename/split, launch an agent fixture, route a notification, restart, and reconnect.
7. No unexplained nested-tmux identifiers in current production code or current-scope documentation.

Phase 9 is complete when the old helper is safely rejected/read-only, explicit upgrade succeeds locally and remotely, ordinary topology/agent behavior matches Phase 8, and nested-specific implementation has been removed rather than merely hidden.

## Phase 10 — macOS readiness and QA

### Goal and scope

Deliver an internally distributable macOS desktop that is genuinely usable in
both supported topologies:

- **A — Local Mac:** the IDE, host daemon, tmux server, projects, Git, and agents
  all run on the physical Mac.
- **B — Mac to Linux over SSH:** the IDE runs on the physical Mac and connects
  through the user's existing OpenSSH configuration to a Linux host where the
  helper, tmux, projects, Git, and agents run.

Phase 10 owns macOS readiness, platform fixes, native integrations, packaging,
and real-Mac QA. It must preserve the completed Phase 9 Linux behavior. It does
not include UI redesign/polish, nested tmux, additional Git network/history
features, or any Phase 11 work.

### Known starting state and risks

A fresh Phase 10 agent must begin from the current tree and read
`plan/product-requirements.md`, this section, `implementation.md`, and
`tests/acceptance/linux/qa-linux-2026-08-11.md`. The Phase 9 Linux release is the
behavioral baseline; its recorded source digest is
`c930d49ae6d3707150778900fcaa9b05faedcb311eede9d085612370261417e8`, but the
agent must capture a new digest if the handed-off tree differs.

The current branch has already been attempted on a Mac and is known to contain
multiple macOS bugs. No repository artifact contains a complete failure list.
Do not assume it is nearly ready and do not code around remembered symptoms.
First reproduce every observable build/test/runtime failure on the target Mac,
retain exact logs, and maintain a failure ledger through closure.

Concrete known gaps visible in the current source include:

- `.github/workflows/ci.yml` has a generic `macos-15` compile/test lane, but no
  retained successful result and no native/package/user-journey coverage.
- `apps/desktop/src-tauri/src/notifications/mod.rs` selects the unsupported
  backend on macOS; notification delivery and activation are absent.
- `apps/desktop/src-tauri/src/power_events.rs` is a no-op outside Linux.
- `apps/desktop/src-tauri/src/connection/files/native_clipboard.rs` returns no
  native clipboard content outside Linux.
- `apps/desktop/src-tauri/tauri.conf.json` has bundling disabled and there is no
  `release/macos/` packaging/install/verification path.
- Non-Linux agent fallback has no Linux-style descendant process discovery;
  tmux command/start-command detection alone may miss wrapped Codex/Claude
  processes.
- Apple filesystem branches exist for `/dev/fd` and `renameatx_np`, but the
  relevant no-follow, overwrite, crash-recovery, staging, Git, and download
  tests are primarily Linux-proven rather than APFS-proven.
- Remote-helper lifecycle scripts intentionally contain Linux `/proc` logic.
  That is valid only for the remote Linux helper and must not leak into local
  macOS daemon lifecycle or cause the Mac desktop to upload a Mach-O helper to
  Linux.
- WKWebView, Finder paste/drop, macOS IME, native menus/shortcuts, Retina and
  display movement, accessibility, sleep/wake, Gatekeeper, and packaged-app
  lifecycle have no physical Mac evidence.

This list is a starting hypothesis, not an exhaustive bug inventory.

### Required machines, tools, and permissions

Phase 10 cannot start its authoritative work or close without:

1. A physical Mac running macOS 14 or newer, preferably Apple Silicon, with
   enough disk space for clean Rust/Tauri builds and disposable fixtures.
2. Xcode Command Line Tools; the pinned Rust toolchain from
   `rust-toolchain.toml`; Node 24; pnpm 11.18; Git; and tmux 3.3 or newer.
   Test Homebrew discovery under `/opt/homebrew` and, through a controlled
   fixture or Intel/Rosetta machine, `/usr/local`.
3. Quad Driver installed and authorized for Accessibility and Screen Recording,
   with screenshots, input, window detection, and AX inspection working against
   a packaged Tauri app.
4. Notification permission for the test bundle. Reset/retest allowed and denied
   states without changing unrelated applications' permissions.
5. Docker Desktop or another disposable Linux/OpenSSH fixture reachable from
   the Mac for repeatable topology B automation, latency shaping, helper
   replacement, fault injection, transfers, and destructive scenarios.
6. A normal saved SSH profile from the Mac to the current remote Linux host machine
   for a narrow manual compatibility pass. Use the user's existing SSH config,
   known-hosts, keys, and ssh-agent; never copy or persist secrets in fixtures or
   evidence.
7. Signing/notarization credentials only if the user has already configured
   them for this project. Their absence permits an explicitly unsigned internal
   artifact but not a false signed/notarized claim.

If the physical Mac, Quad Driver, or disposable SSH target is unavailable,
record the exact blocker and stop. Linux or cross-compilation evidence cannot
substitute for Phase 10 completion.

### Mandatory prerequisite preflight and readiness signals

Before any Phase 10 source edit, dependency change, build fix, or autonomous
implementation, run a read-only/environment-only preflight on the actual Mac.
Report every check as `PASS`, `FAIL`, `LIMITED`, or `NOT_APPLICABLE`; retain the
result as `tests/acceptance/macos/preflight-macos.md` once Phase 10 work is authorized.
The chat/status update must include the same result before implementation
continues.

The preflight must verify:

1. **Source and machine identity**
   - Expected repository, branch/commit, dirty state, and source digest are
     explicit; no unrelated worktree changes will be overwritten.
   - macOS is 14 or newer; CPU architecture, RAM, free disk, power state, and
     test account/home are recorded.
   - Build/cache/Docker/scratch paths are disk-backed. There is enough capacity
     for clean Tauri/Rust builds, Docker images, evidence, and fixtures. Record
     the calculated requirement; use at least 60 GiB free as the normal-phase
     warning threshold and reserve additional space before any exact 5 GiB lane.
2. **Build and runtime toolchain**
   - Xcode Command Line Tools and SDK resolve correctly.
   - The pinned Rust toolchain, required targets/components, Node 24, pnpm
     11.18, Git, tmux 3.3+, OpenSSH, Homebrew, Docker, and required native build
     tools are installed, executable from a non-interactive app context, and
     version-recorded.
   - Homebrew/path discovery is classified for `/opt/homebrew` and `/usr/local`;
     an unavailable Intel/Rosetta path is explicitly `LIMITED`, not silently
     treated as Apple Silicon coverage.
3. **Quad Driver (`cua-driver`) base capability**
   - Accessibility and Screen Recording permissions are granted to the actual
     driver/terminal process that will run QA.
   - Against a harmless native test application, Quad Driver can enumerate and
     focus a window, capture an unredacted screenshot, inspect the AX tree,
     click a control, type text including a `Cmd` shortcut, and observe the
     resulting state.
   - The Mac is unlocked and the driver session survives an application relaunch.
     A later project-specific check against the packaged Tauri app is still
     mandatory; this base check only proves the automation environment works.
4. **macOS permission and native-QA readiness**
   - The tester can inspect/reset the project bundle's Notification permission,
     and no MDM/policy prevents testing both denied and allowed flows. The
     project-specific permission cannot be claimed until a package exists.
   - Finder access/file selection, clipboard file/image contents, Automation
     prompts required by the driver, VoiceOver/AX inspection, at least one
     representative non-Latin IME, Retina scaling, and reduced-motion settings
     are available for controlled tests.
   - Record whether a second display or a second scale factor is available.
     Record a user-approved window for the disruptive physical sleep/wake and
     unlock test; do not trigger system sleep during preflight.
5. **Disposable remote target**
   - Docker can start, stop, and remove an isolated Linux/OpenSSH target from the
     Mac; architecture is recorded; host keys/config remain fixture-local.
   - The Mac can connect non-interactively to that disposable target, run tmux
     and Git, install an app-owned helper, apply 100 ms network shaping inside
     the disposable environment, and clean all created resources.
6. **Real remote Linux host compatibility target**
   - The intended saved SSH alias/config path is named without exposing secrets.
   - A read-only/no-op `BatchMode` connection succeeds using the existing
     ssh-agent/known-hosts policy, and remote OS, shell, tmux, Git, and PATH can
     be inspected without mutation.
   - The preflight records `REMOTE_LINUX_READ_ONLY` unless the user separately grants
     exact authorization for an `ade-phase10-*` scratch session/repository.
7. **Distribution and optional credentials**
   - Decide and record `SIGNED_NOTARIZED` or `UNSIGNED_INTERNAL` before package
     work. Missing Apple credentials do not block an unsigned internal build.
   - Decide and record `UNIVERSAL`, `APPLE_SILICON_ONLY`, or the exact pending
     Intel/Rosetta prerequisite. Never infer Intel runtime support from an
     arm64 build alone.
8. **Safety and cleanup authority**
   - App-owned home/config/cache prefixes, named local tmux fixtures, Docker
     names, scratch repositories, and cleanup targets are explicit and narrow.
   - Real user tmux sessions, repositories, SSH files, agent credentials/config,
     and remote Linux host services are outside autonomous mutation scope.

Emit exactly one implementation signal after preflight:

- `PHASE10_IMPLEMENTATION_READY` — all prerequisites needed for Stages 10.0–10.2
  pass. Optional limitations such as unsigned packaging, Apple-Silicon-only
  output, no second display, or read-only remote Linux host are enumerated explicitly.
- `PHASE10_BLOCKED` — one or more required prerequisites fail. List the failed
  check, evidence, exact remediation/user action, and the work it blocks. Stop
  before source edits; do not replace the missing prerequisite with Linux,
  mocks, or assumptions.

After the first packaged Tauri build and before Stage 10.3, run a second
project-specific permission check. Quad Driver must capture, address, click,
type into, and inspect the AX tree of that exact packaged app; the app must
successfully request Notification permission; both allowed and denied behavior
must be controllable; Finder/clipboard/picker access must work; and the approved
sleep/wake window must still be available. Emit:

- `PHASE10_NATIVE_QA_READY` when those checks pass; or
- `PHASE10_NATIVE_QA_BLOCKED` with exact System Settings/user remediation and
  stop before exploratory QA.

Do not repeatedly poll a failed permission. Signal it once and wait for the
external/user action that macOS requires.

### Test-target policy and safest remote split

Use three clearly separated targets:

- **A / Local Mac (authoritative native target):** use an isolated app profile,
  app-owned scratch directories/repositories, and a named disposable tmux server
  or sessions. This target is authoritative for macOS host behavior and all
  Apple-native surfaces.
- **B-Docker / Mac→disposable Linux (authoritative repeatable remote target):**
  automate remote helper probe/install/upgrade/rollback, topology mutations,
  filesystem/Git/agent fixtures, transfers, 100 ms latency, connection loss,
  container/helper/tmux restart, security races, and cleanup. CI and regression
  scripts may depend on this target because it is created and destroyed by the
  test.
- **B-Remote-Linux / Mac→real remote Linux host (manual compatibility target only):** use Quad
  Driver/manual observation to validate the actual SSH config/ssh-agent/
  known-hosts path, actual distro/shell/tmux/PATH behavior, packaged connection,
  read-only topology discovery, terminal rendering, and app-side disconnect/
  reconnect. Automated tests must never depend on this machine. Never shape its
  network, restart its host/helper/tmux, install or upgrade hooks/helper, mutate
  an existing repository, rename/kill existing tmux objects, or write outside
  app-owned staging. If the user explicitly approves a disposable remote Linux host
  fixture, confine mutations to a clearly named `ade-phase10-*` tmux session and
  scratch repository, verify exact targets before each destructive action, and
  clean only those objects. Otherwise keep the remote Linux host pass non-destructive.

Full remote correctness is proved against B-Docker. B-Remote-Linux is a required
real-environment compatibility smoke, not a substitute for the disposable
remote acceptance suite and not a production dependency.

### Ordered execution strategy and stop/go gates

Escalate from cheap deterministic work to expensive physical acceptance. Do not
run the full acceptance matrix early.

**Readiness gate R0:** do not enter Stage 10.0 until
`PHASE10_IMPLEMENTATION_READY` has been emitted. Do not enter Stage 10.3 until
`PHASE10_NATIVE_QA_READY` has been emitted. These are real stop/go gates, not
documentation-only checklists.

#### Stage 10.0 — Capture and reproduce the real Mac baseline

1. Record source digest, dirty state, macOS/build number, hardware architecture,
   Xcode/SDK, Rust, Node, pnpm, Tauri, WebKit, tmux, Git, Homebrew, Docker, SSH,
   and Quad Driver versions.
2. On a clean dependency install and isolated app home, run the cheapest gates
   in order: protocol/code generation, Rust compile, Rust tests/Clippy,
   frontend typecheck/tests/build, Tauri development build, then packaged build.
3. Reproduce the known Mac failures before fixing them. For each failure record
   command, exit/result, relevant log, component, architecture, and whether it
   is compile-, link-, package-, launch-, or runtime-level.
4. Audit all `target_os`, Linux `/proc`, ELF, `/dev/fd`, `renameatx_np`, process,
   shell/tool lookup, socket/runtime path, clipboard, notification, power,
   filesystem, and packaging branches. Classify each as portable, local-macOS,
   remote-Linux-only, or unsupported.

**Gate 10.0:** the failure ledger and platform inventory are complete enough to
explain every currently observed Mac build/test/launch failure. No full QA is
allowed yet.

#### Stage 10.1 — Implement known macOS requirements and fix the baseline

Fix root causes, not test skips or broad `cfg` exclusions. Add focused tests
alongside each fix.

1. **Build and lifecycle portability**
   - Make the complete workspace compile/test on macOS with warnings denied.
   - Separate local macOS daemon lifecycle from remote Linux helper lifecycle.
   - Resolve runtime/config/cache/socket paths with user-only permissions.
   - Discover Homebrew tmux/Git/SSH without assuming an interactive shell PATH.
   - Preserve protocol and capability negotiation across both platforms.
2. **Native notifications**
   - Add a macOS backend under
     `apps/desktop/src-tauri/src/notifications/` using supported Apple APIs.
   - Handle permission unknown/allowed/denied states, stable action identity,
     foreground suppression, privacy-safe content, sounds, and click activation.
   - A click must focus the exact server/session/window/pane and attention
     generation after rename/reconnect; a missing/foreign target focuses nothing
     and remains unseen.
3. **Clipboard, Finder, picker, and drag/drop**
   - Read Finder file URLs and image clipboard representations (including TIFF)
     through a native macOS bridge and convert bounded images to PNG.
   - Preserve the 25 MiB encoded-image limit, 500 MiB upload confirmation,
     shell escaping, deterministic multi-file order, verified staging, and no
     implicit Enter.
   - Verify Tauri/WKWebView file drop and save/open picker behavior. Directory
     terminal drop and remote drag-out stay excluded.
4. **Power, windows, input, and WebKit**
   - Emit the existing authoritative resume/reconnect event from macOS
     sleep/wake and session activation.
   - Preserve `Cmd` shortcuts, native edit semantics, IME composition, terminal
     mouse/selection/paste, close-vs-detach, minimize, fullscreen, multi-window
     focus, Retina scaling, display movement, and reduced motion.
5. **Local host, process, filesystem, and Git**
   - Add bounded macOS process inspection for wrapped Codex/Claude detection;
     unknown processes remain unknown rather than guessed.
   - Prove APFS case-insensitivity, NFC/NFD names, case-only rename, symlink and
     parent-swap resistance, descriptor-relative access, atomic no-replace and
     overwrite publication, crash journal recovery, disk-full handling, watcher
     overflow/reseed, and private staging.
   - Prove normal/initial/worktree Git repositories, Unicode/case-only paths,
     rename/delete/binary/conflict/hook behavior, stale generations, stage/
     unstage/discard file and hunk, and commit outcomes.
   - Branch scope remains the PRD's existing behavior: display the current
     branch/detached HEAD and refresh correctly after an external CLI branch
     switch; Phase 10 does not add GUI branch creation/checkout/pull/push.
6. **Packaging**
   - Enable Tauri bundling and add `release/macos/` build, verify, install,
     upgrade/rollback, and uninstall tooling that preserves config and tmux.
   - Configure application identifier, version, minimum OS, `.icns`, helper
     placement, architecture checks, narrow hardened-runtime entitlements, and
     DMG or zipped `.app` output.
   - Verify the packaged application launches outside the build tree. Inspect
     signatures, entitlements, quarantine, Gatekeeper, and notarization status;
     label unsigned artifacts honestly.
   - Prefer a universal `arm64`/`x86_64` artifact. If an Intel build/runtime gate
     is unavailable, ship only the physically tested Apple Silicon internal
     artifact and mark Intel limited.
7. **Repeatable harnesses**
   - Add `tests/acceptance/macos/` local-Mac and Mac→Docker drivers using isolated homes,
     named tmux fixtures, scratch repos, bounded payloads, cleanup traps, and
     compact evidence.
   - Add macOS CI compile/test/package smoke without pretending hosted CI proves
     native UI behavior.

Critical implementation areas:

- `apps/desktop/src-tauri/src/notifications/`
- `apps/desktop/src-tauri/src/power_events.rs`
- `apps/desktop/src-tauri/src/connection/` and
  `apps/desktop/src-tauri/src/connection/files/`
- `apps/desktop/src-tauri/Cargo.toml` and
  `apps/desktop/src-tauri/tauri.conf.json`
- `apps/desktop/src/commands/registry.ts` and terminal/file/agent shell surfaces
- `apps/host/src/daemon.rs`, `apps/host/src/paths.rs`,
  `apps/host/src/remote_helper.rs`, and `apps/host/src/service/`
- `crates/tmux-control/` and `crates/protocol/`
- new `release/macos/`, `tests/acceptance/macos/`, and macOS CI workflow changes

**Gate 10.1:** every baseline failure has a fixed/limited disposition; macOS
format, warnings-denied Clippy, all-target Rust tests, frontend typecheck/tests/
build, focused platform tests, Tauri development launch, and packaged build are
green on the physical Mac. Targeted local and B-Docker smoke proves connection,
one tmux mutation, one file/Git mutation, and cleanup. Full acceptance remains
deferred.

#### Stage 10.2 — macOS/platform review iterations

Run rigorous independent reviews after the implementation stabilizes. Reviews
must inspect both the diff and affected platform boundaries, with special
attention to:

- unsafe Apple FFI, delegate/thread/lifetime ownership, permission/error paths;
- accidental Linux assumptions and overly broad non-Linux fallbacks;
- local-Mac versus remote-Linux binary/daemon/helper separation;
- APFS race safety, Unicode/case identity, symlink escape, atomic publication,
  cancellation, and crash recovery;
- notification privacy, exact routing, activation after reconnect, and denied
  permissions;
- clipboard/drop payload bounds, path escaping, staging ownership, and Enter/
  bracketed-paste behavior;
- WKWebView/Tauri lifecycle, stale state, sleep/wake, menu/shortcut/IME behavior;
- packaging architecture, entitlements, signing, quarantine, and install/
  uninstall confinement;
- tests that accidentally mock the native boundary they claim to prove.

Fix accepted findings in bounded batches and rerun the smallest relevant Mac
checks after each batch. Add a separate cross-platform invariant audit covering
local Mac, remote Linux, and Linux-regression ownership. Stop reviews only when
fresh rounds converge under `/gdev_cycle`; do not run full acceptance during
review churn.

**Gate 10.2:** reviews and the invariant audit have no unresolved correctness,
security, data-loss, platform-boundary, or maintainability blocker. All targeted
checks affected by review fixes pass on Mac and Linux.

#### Stage 10.3 — Iterative physical-Mac exploration with Quad Driver

Use a packaged build on the physical Mac. Quad Driver is the primary AI-driven
interaction tool; pair every visual assertion with direct tmux, filesystem,
Git, process, digest, or OS-state evidence where possible.

Explore in increasing breadth:

1. Packaged launch, AX discovery, local attach, workspace/tab/pane creation, and
   basic terminal input.
2. Local projects, active-root changes, Explorer operations, Monaco/Markdown,
   Git, agents, and notifications.
3. Finder/file URLs, clipboard image/file paste, drag/drop, picker/download,
   keyboard/IME, windows, Retina/display movement, and accessibility.
4. B-Docker remote workflows, latency, reconnect, transfer, helper, and agent
   routes.
5. The narrow B-Remote-Linux compatibility pass under the safety policy above.

Maintain `tests/acceptance/macos/findings.md` with a stable ID, severity, topology,
package/source digest, exact reproduction, expected/actual result, screenshots/
AX/logs, direct system assertion, fix, and retest status. Earlier failures can
mask later bugs: after each bounded fix batch, rerun the affected scenario and
then repeat the broader exploration from a clean app/tmux/project state. Do not
turn exploratory scripts into dependencies on real remote Linux host.

**Gate 10.3:** all P0/P1 and all release-contract P2 findings are fixed; nothing
is untriaged; affected targeted checks pass; and two consecutive exploration
passes across A and B-Docker reveal no new P0/P1 or release-contract failure.
The B-Remote-Linux compatibility smoke also passes or has an explicit user/environment
blocker. Only then may full acceptance begin.

#### Stage 10.4 — Full macOS acceptance QA (run last)

Run the complete matrix below against the final release-candidate source and
packaged artifact. A failure returns to a bounded fix/review/targeted-exploration
loop; rerun the affected acceptance lanes and any shared invariants before
claiming closure.

### Concrete QA activity matrix

Legend: **A** = physical local Mac; **D** = Mac IDE to disposable Docker Linux
SSH target; **O** = narrow manual/Quad pass against real remote Linux host. “Automated”
means deterministic CLI/protocol/system assertions; all packaged visual/native
flows also receive Quad Driver evidence.

| Activity | A — Local Mac | D — Disposable Linux SSH | O — Real remote Linux host |
| --- | --- | --- | --- |
| Package/install/launch | Clean install, upgrade, rollback, uninstall, Gatekeeper/quarantine, launch packaged `.app`, AX discovery, config preservation | Verify Mac package selects and installs the correct Linux helper architecture/version | Launch packaged app and connect through the real saved SSH profile; no install/upgrade |
| Workspace/project entry | Create a scratch project and initial Git repo; open it by starting/selecting its tmux pane; switch among projects/workspaces; active root follows `cd` without closing app tabs | Same against provisioned scratch projects and multiple sessions | Discover actual workspaces read-only; use only an approved `ade-phase10-*` scratch project for mutations |
| tmux topology | Discover existing server; create/select/rename/reorder/close sessions and windows; split/focus/resize/zoom/close panes; external CLI→UI and UI→CLI sync; Unicode/spaces; second normal client; app exit detaches only | Repeat deterministically at 100 ms; external mutations, disconnect, helper/tmux restart, stale-ID rejection, no queued mutations | Validate discovery/rendering and app-side reconnect against actual tmux/PATH; never rename/kill existing objects |
| Terminal | Shell and agent TUI output, colors, Unicode, wraps, cursor, alternate screen, mouse, selection override, copy, paste, search, links, scrollback/new-output indicator, flood recovery, resize | Same replay/control contracts over SSH; no duplicated bytes/input through flap | Basic real shell render/input only in an approved scratch pane; otherwise read-only observation |
| Explorer/root/files | Lazy tree, dotfiles/ignored files, protected `.git`/`node_modules`, watcher updates, create/rename/move/duplicate/delete, confirmations, symlink display/non-follow, external/agent changes, rapid root switching | Repeat all mutations and watcher overflow/reseed on disposable repo | Read-only browse an approved path; mutations only inside explicit scratch fixture |
| Editor/Markdown/media | Open/edit/autosave/external reload, last-writer-wins, restart restore, limits/errors, image/binary handling, Markdown source/preview/split and sanitization | Same over latency/reconnect and agent file changes | Read-only/open representative files unless scratch mutation is approved |
| Git and branch awareness | Initial repo, worktree, status groups, staged/unstaged/untracked/rename/delete/binary/conflict, diffs, file/hunk stage/unstage/discard confirmations, commit/hooks/errors; show branch/detached HEAD and refresh after external CLI branch switch | Repeat mutations, cancellation, stale generation, reconnect, large/bounded status, and branch refresh in disposable repos | Read-only status/diff/branch display on a user-selected repo; no stage/discard/commit/checkout outside approved scratch |
| Agents/hooks/process | Detect manually started and app-launched Codex/Claude fixtures, including wrapped descendants; new window/split; hook review/install/uninstall in isolated config; working/blocked/idle/done/unseen rollups; reconnect continuity | Linux fixture/real-binary version smoke, remote hook tunnel, no public port, helper reconnect and exact server/pane route | Detect existing agents read-only; no hook/config install unless separately approved |
| Native notifications | Permission allow/deny, blocked/done/suppression/sound/privacy; click exact pane after rename/reconnect; missing/foreign destination focuses nothing and stays unseen; foreground/background windows | Remote lifecycle triggers the same Mac notification and exact route after reconnect | One safe remote attention event only if an approved scratch agent fixture exists; otherwise route compatibility is covered by D |
| Finder/clipboard/drop/picker | Finder file URLs, spaces/quotes/NFC/NFD, multi-file order, PNG/TIFF image clipboard, 25 MiB rejection, local path paste, drag/drop, open/save destination picker, cancel/overwrite, no implicit Enter | File/image paste uploads to private Linux staging, digest verified before escaped path paste; failure/cancel/collision/500 MiB confirmation; picker download byte equality | Optional small app-owned staging upload/download only; never overwrite user files |
| Transfers | Bounded-memory upload/download, two-worker queue, progress/ETA/cancel/error, disk-full/collision/partial cleanup, local destination publication and overwrite identity | Same over 100 ms and disconnect with independent control/bulk lanes and correct u64 accounting | Small compatibility transfer to app-owned staging only; destructive/large/fault cases stay on D |
| macOS filesystem semantics | APFS case-insensitive and case-sensitive volume when available, NFC/NFD, case-only rename, symlink/parent swap, descriptor safety, atomic no-replace/overwrite, crash recovery, disk full, watcher overflow | Linux semantics remain covered independently; verify no Mac path/format leaks into remote requests | Not applicable beyond read-only compatibility |
| Recovery/persistence | Quit/reopen preserves tmux and app tabs; daemon/app crash; Mac sleep/wake and unlock; authoritative reseed; no stale writes/duplicate output/notifications | App/helper/container/network interruption and explicit helper upgrade; authoritative recovery and lane isolation | App-side disconnect/reconnect and restart only; do not disrupt remote services |
| WKWebView/input/windows/accessibility | `Cmd` shortcuts and native edit behavior, IME candidate/commit, menus, close/minimize/fullscreen, focus/modal traps, VoiceOver/AX labels, reduced motion, narrow window, Retina, multi-display scaling/movement | Verify remote state does not alter these local UI guarantees | Connection profile and remote surfaces remain keyboard/AX operable |
| Scale/performance | 20 sessions, 100 windows, 50 live panes, 250k-file fixture; target topology/root/Explorer/Git latency and terminal responsiveness | Same at 100 ms with bounded queues and interactive control during transfer | No load/scale testing on production remote Linux host |
| Security/privacy/diagnostics | User-only sockets/staging, no secrets/content in logs or notifications, sanitized Markdown/links, confined installer/uninstaller, redacted diagnostics | Same plus OpenSSH ownership, no public listener, path/archive/symlink confinement, helper mismatch read-only | Inspect generated evidence for secrets; do not capture unrelated terminal/project content |

### Full acceptance commands and activities

The fresh agent must create concrete `tests/acceptance/macos/` entry points, but the final
gate must include all of the following rather than a nominal wrapper:

1. macOS format, warnings-denied workspace Clippy, all-target Rust tests,
   protocol drivers, frontend typecheck/tests/production build, and Tauri
   packaged build on a clean checkout.
2. Deterministic A local integration suite for daemon/protocol/tmux/filesystem/
   Git/agents/transfers using isolated fixtures.
3. Deterministic D suite from the Mac to a freshly created Linux/OpenSSH target,
   including 100 ms latency, helper mismatch/explicit upgrade, faults, and
   cleanup.
4. Packaged Quad Driver journey covering every user-visible matrix row that is
   applicable to A and D, plus the narrow O compatibility journey.
5. Physical notification activation, Finder/clipboard/drop/picker, sleep/wake,
   IME, Retina/display, accessibility, and window lifecycle. Deterministic mocks
   may supplement but never replace these physical checks.
6. Package reproduction from independent checkout/target paths where Apple
   tooling permits it, architecture inspection, helper-format verification,
   install/upgrade/rollback/uninstall, entitlements/signature/Gatekeeper status,
   and packaged launch outside the build tree.
7. Linux bounded Phase 0–9 regressions after platform refactoring. Do not rerun
   exact Linux 5 GiB lanes unless transfer-contract code changed.
8. Small representative transfers during normal development. For the final Mac
   release candidate, run exact 5 GiB upload and download through the Mac
   desktop manager in A and D if transfer/storage/publication code changed or
   lacks source-identical prior Mac evidence. Bind results to the transfer
   component digest; run this expensive lane only after all other acceptance
   work is stable.
9. Performance/scale measurements against the product targets, with observable
   event/render completion rather than RPC timing alone.
10. Final clean-state rerun of affected nondeterministic suites, followed by
    cleanup verification for apps, daemons, tmux fixtures, Docker containers,
    SSH masters, Quad Driver sessions, scratch repos, package mounts, and large
    payloads.

### Evidence requirements

Retain compact, source-bound evidence under `tests/acceptance/macos/`:

- `README.md` with commands, prerequisites, target naming, and cleanup;
- `preflight-macos.md` with both readiness signals, permission states, explicit
  limitations, and any user-provided authorization boundaries;
- `baseline-macos.md` with the initial known-failure ledger and exact fixes;
- `findings.md` with all Quad Driver exploration rounds and dispositions;
- `qa-macos-YYYY-MM-DD.md` with the final matrix, direct observations, limits,
  and A/D/O distinction;
- machine-readable results for compile/tests, local integration, Docker SSH,
  package, performance, transfer, and cleanup;
- selected screenshots/AX trees and notification/picker/permission evidence;
- source-tree and package/component digests, OS/hardware/tool versions,
  architecture, signature, entitlements, Gatekeeper, and notarization results;
- proof that Docker/remote Linux host evidence contains no SSH secrets, unrelated terminal
  contents, prompts, file content, or credentials.

Append an authoritative Phase 10 entry to `implementation.md`; update README,
setup, release, troubleshooting, CI, and traceability documents. Historical
Linux evidence remains immutable. Evidence must distinguish `PASS`, `LIMITED`,
`BLOCKED`, and deterministic substitute; absence of a physical result is never
reported as a pass.

### Safety and storage constraints

- Preserve the user's SSH configuration, keys, agent, known-hosts policy, tmux
  configuration, Codex/Claude credentials/configs, projects, and existing tmux
  sessions. Automated QA uses isolated homes/configs and disposable fixtures.
- Never make the real remote Linux host machine a CI/test dependency. Do not automate
  destructive operations there. Obtain explicit target-specific authorization
  before creating its optional scratch fixture; otherwise remain read-only.
- Use existing OpenSSH authentication. Do not collect passwords/private keys or
  weaken host-key checking.
- Confirm every destructive UI action and verify exact fixture identity before
  direct CLI cleanup. Never clean broad home, `/tmp`, project, or tmux scopes.
- Keep notification/log/evidence content privacy-safe. Do not exercise real
  credentialed agents unless separately authorized; fixtures and version/help
  smoke are the default.
- Use disk-backed repository `tmp/work`, shared bounded caches, compact
  `tmp/evidence`, non-incremental release builds, and cleanup traps. Never place
  large payloads on RAM-backed `/tmp`. Exact 5 GiB work is sequential,
  release-only, and removed after digest/result publication.
- Do not add Phase 11 visual redesign while correcting platform usability.

### Deliverables

1. Working native macOS implementations for notifications, clipboard/Finder,
   power/resume, process discovery, host/filesystem behavior, and packaging.
2. A verified internal macOS `.app` plus DMG or zip, with honest architecture,
   signing/notarization, and minimum-OS metadata.
3. Rootless macOS install/upgrade/rollback/uninstall and verification tooling in
   `release/macos/`.
4. Repeatable local-Mac and Mac→Docker integration/acceptance harnesses under
   `tests/acceptance/macos/`, plus macOS CI compile/test/package smoke.
5. Completed Quad Driver exploratory and final packaged QA evidence for A, D,
   and the narrow O compatibility pass.
6. Updated setup/release/troubleshooting/traceability documentation and an
   append-only Phase 10 implementation ledger.
7. A clean environment with all disposable app, daemon, tmux, Docker, SSH,
   package, Quad Driver, scratch, and large-transfer artifacts removed.

### Unambiguous exit criteria

Phase 10 is complete only when all of the following are true:

1. `PHASE10_IMPLEMENTATION_READY` and `PHASE10_NATIVE_QA_READY` were emitted
   from the physical Mac with retained evidence; no required prerequisite or
   permission silently remained unresolved.
2. Every reproduced Mac baseline failure and every review/exploration finding
   has a fixed, verified, or explicitly accepted limited disposition; no P0/P1,
   release-contract P2, or untriaged item remains.
3. Review iterations and the cross-platform invariant audit have converged.
4. Two clean Quad Driver exploration passes satisfy Gate 10.3.
5. The entire final acceptance matrix passes on the same source-bound packaged
   release candidate in A and D; the narrow real-remote Linux host compatibility pass is
   recorded without unsafe mutation.
6. Native notification click, Finder/clipboard/drop/picker, physical sleep/wake,
   IME, Retina/display, accessibility, and window behavior have real-Mac
   evidence rather than Linux, cross-build, or mock substitution.
7. Local Mac tmux and remote Linux tmux remain authoritative through external
   mutations, app restart, disconnect/reconnect, and a second normal client.
8. Package architecture/helper formats, install lifecycle, Gatekeeper,
   entitlements, and signing/notarization state are verified and documented.
   Intel may remain `LIMITED` only if the released artifact and docs clearly
   restrict support to the physically tested Apple Silicon build.
9. Linux bounded regressions pass after all shared/platform changes.
10. Required evidence, digests, docs, implementation ledger, storage accounting,
   privacy audit, and cleanup are complete.

If any physical Mac prerequisite or required native activity cannot be run,
Phase 10 remains incomplete or blocked. A hosted macOS compile, Linux result,
cross-compilation, deterministic mock, or Docker-only pass cannot close it.

## Phase 11 — UI polish (intentionally open)

### Current status

**Superseded on 2026-08-13.** The user tested the post-Phase-10 build and
delivered the critique this section was gated on. The approved brief, the
visual mock (`plan/phase11-ui-mock.html`), and the full Phase 11 plan — plus
the new Phase 12 performance plan — now live in
[`plan/phases-11-12.md`](./phases-11-12.md). The input/planning-gate text
below is retained for history only.

Phase 11 is a placeholder, not an implementation plan. No UI redesign or polish work starts before the user tests the app after Phases 9 and 10.

### Input gate

After Phase 10, prepare a critique build and a compact visual inventory of the actual macOS and Linux UI:

- workspace rail;
- terminal/editor/diff tab strip and splits;
- Explorer/Git sidebar;
- agent sidebar and notifications;
- command palette, dialogs, confirmations, empty/loading/error/disconnected states;
- transfer UI;
- narrow, full-screen, and high-density layouts.

The user will critique visual hierarchy, density, spacing, typography, colors, icons, discoverability, interaction flow, and platform feel. Record their approved feedback without broadening or rewriting it.

### Planning gate

Only after that critique:

1. Translate approved feedback into screen-by-screen changes and explicit acceptance screenshots/interactions.
2. Identify shared design tokens and component boundaries before editing individual screens.
3. Separate visual polish from any requested workflow or product change; protocol/backend changes require explicit scope.
4. Preserve terminal performance, accessibility, keyboard behavior, responsive layouts, and all Phase 9–10 functionality.
5. Present the detailed Phase 11 plan for approval before implementation.

Phase 11 remains open until the user supplies and approves that critique. There is no assumed visual direction, completion date, or acceptance claim in this plan.

## Sequencing and handoff

1. Execute and close Phase 9 first. Its protocol removal and helper upgrade become the baseline for macOS.
2. Execute Phase 10 on that baseline. Linux may be used for preparatory refactoring, but final QA waits for a real Mac.
3. Provide the user with the packaged post-Phase-10 build and concise self-testing instructions.
4. Stop implementation and collect the user's UI critique.
5. Amend only the Phase 11 section with the approved UI brief, show the updated plan, and wait for explicit approval.

For Phases 9 and 10, append durable progress, review count, exact commands/results, source/package digests, limitations, cleanup, and superseded claims to `implementation.md`. Keep current phase evidence under `tests/acceptance/linux/` and `tests/acceptance/macos/`; keep large transient builds under bounded disk-backed `tmp/work` rather than RAM-backed `/tmp`.
