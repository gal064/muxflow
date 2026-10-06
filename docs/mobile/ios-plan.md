# Muxflow iOS — implementation plan

Status: foreground iOS implementation is on `feat/ios-mobile`, with shared
sources and independent reviews complete. Native SSH contract tests and all
four simulator UI flows pass on macOS. Signed distribution and real-iPhone
lifecycle, Local Network, keyboard, audio and notification gates remain open.
This is ready for device QA, not a completed shipping milestone. See evidence below.

## 1. Objective and scope

Add iOS support to the existing Expo/React Native app in `apps/mobile`.
Share Android and iOS screens and application logic, and move equivalent
desktop/mobile behavior into shared packages so macOS also benefits.

Develop on Linux. Use GitHub Actions macOS runners for native builds and
simulator testing. An Apple Developer account is not required for simulator
builds; signed iPhone distribution and TestFlight require credentials later.

Proposed delivery milestones:

1. **Working iOS client:** existing mobile features, native SSH, shared code,
   reliable foreground reconnect, automated simulator coverage, and a signed
   real-iPhone lifecycle/Local Network permission gate by the end of Phase 2.
2. **Background agent alerts:** host-originated push delivery through APNs.

The second milestone's priority remains open. This staging is a recommendation,
not a decision to drop background notification parity.

The existing Android design remains the starting point for product behavior.
The first implementation commit must update `design.md` and `decisions.md`:
remove iOS from the non-goals, replace D4's verbatim-copy requirement with
shared-source ownership, and distinguish Android's background service from
iOS lifecycle and the proposed push milestone. Update the other Android-only
stack, key-storage, notification and font requirements where they now differ.
Do this alongside the first isolated sharing change, before iOS feature work.
That first commit must also update the comments above `PROTOCOL_MAJOR` in
`crates/protocol/src/lib.rs` and the admission function in
`apps/mobile/src/protocol/contract.ts` to state the bump rule in section 4.
Both comments were updated with the initial shared-source change; major 4
and admission behavior remain unchanged.

## 2. What exists today

| Area | Current implementation | Consequence for iOS |
| --- | --- | --- |
| Mobile frontend | Expo, React Native, TypeScript, Expo Router, Zustand | Extend the same application rather than create a SwiftUI frontend. |
| Mobile connection and protocol | TypeScript framing, generated protobuf bindings, request handling, reconnect, control/bulk lanes | Reuse with a different native SSH adapter and lifecycle policy. |
| Native mobile SSH | Android-only Expo module wrapping SSHJ | Add an Apple implementation of the byte transport and key storage. |
| Android background operation | Foreground service and native wake timers | Keep the Android behavior; implement a different iOS lifecycle policy. |
| Desktop frontend | Tauri, React, TypeScript | Share suitable functions and data through workspace packages. |
| Desktop SSH | Rust invokes system OpenSSH | Keep the existing transport behind desktop's own adapter. |
| Host | Rust host, tmux integration, agent state, filesystem and voice services | Continue using the same host and protocol schema. |
| Terminal interactions | `packages/terminal-interactions` | Already shared; extend only where equivalent behavior exists. |
| Markdown and agent labels | Equivalent code copied between desktop and mobile | Replace copies with shared imports and preserve behavior. |
| Design palette | Mobile TypeScript values copied from desktop CSS | Share palette data while retaining platform layout and typography. |
| CI | Linux mobile checks and macOS desktop checks/releases | Add an explicit iOS build and simulator job. |

Repository access was checked during planning: the existing `gal064` GitHub
authentication has write access to `gal064/muxflow`, and Actions is enabled.
The iOS runner job and UI automation harness are implemented; their observed
results and remaining gates are recorded in section 9.

## 3. Sharing boundaries

### Android and iOS

Keep one mobile application and one copy of:

- Hosts, agents, terminal, files and voice screens.
- Stores, navigation, host-key trust decisions and connection diagnostics.
- Protocol framing, requests, topology reconciliation and terminal credit flow.
- Terminal controllers, xterm WebView content, selection and touch logic.
- Voice controllers, recording coordination, playback state and preferences.
- Notification eligibility, attention state and target routing where the
  platform delivery mechanism allows the same behavior.

Use small platform adapters for SSH, secure key storage, lifecycle and timers,
notification presentation, user messages, audio integration and update links.
Prefer existing owners and interfaces to a general plugin or capability system.

The native SSH interface currently includes foreground-service and wake-timer
methods. Separate those concerns before adding iOS. Do not implement pretend
foreground-service methods on iOS that silently do nothing.

Android keeps its native `scheduleWake` implementation. iOS uses the existing
JavaScript `BackgroundTimer` implementation while active; it does not expose
Android's wake API or promise timer callbacks during suspension. Cancel
connection/reconnect deadlines when their attempt is invalidated and create
fresh deadlines on resume. If measurements justify a shutdown grace period,
a native lifecycle timer owns its expiry; JavaScript timers do not own that
cleanup. The initial teardown-and-reconnect implementation needs no grace timer.

### Mobile and macOS

Proposed package boundaries, to be confirmed against actual dependencies:

- `packages/client-core`: platform-independent agent label functions,
  equivalent notification decision primitives and shared palette data.
- `packages/markdown`: the existing Markdown renderer and sanitization rules,
  consumed by the desktop DOM and mobile WebView. Keep its DOM dependency out
  of the React Native runtime.
- `packages/terminal-interactions`: retain the existing shared selection/link
  behavior; move additional terminal helpers only when their contracts match.

Keep platform projections small: desktop CSS versus React Native styles,
desktop agent records versus mobile topology records, and native notification
presentation versus mobile notification presentation.

Do not assume the two notification policies are identical: mobile's current
policy is a simplified desktop policy. Extract common decisions without
erasing intentional differences. Existing outputs and sanitization must remain
unchanged during extraction.

Retain the shared protocol schema and platform-appropriate bindings. Sharing
the schema does not require replacing the desktop Rust client with the mobile
TypeScript client, or replacing all SSH implementations with a new Rust core.

## 4. iOS lifecycle and notification behavior

iOS has no general equivalent to Android's foreground service for indefinitely
maintaining an ordinary SSH connection. An app normally becomes suspended
after entering the background. Native timers do not remove that limit.

For the first milestone:

1. On background entry, stop recording, prevent background autoplay, yield
   terminal sizing ownership using the existing terminal mechanism, and cancel
   reconnect work that should not run while suspended. Treat transient iOS
   `inactive` states, such as a permission prompt, separately from background.
2. Initially close or invalidate mobile transports through one lifecycle owner
   on background entry. Preserve the selected host and useful in-memory state;
   explicit user disconnect still disables automatic reconnect. Do not add a
   grace timer to this first implementation.
3. On foreground entry, reconnect once if the user still wants a connection.
   Reconcile authoritative topology and agent state, invalidate stale bulk
   channels, reattach terminals and reclaim the measured viewport. Guard
   callbacks so the old connection cannot close or publish into the new one.
4. Restore voice registration through existing reconnect hooks. Received
   messages can remain in memory, but replies sent while disconnected are not
   guaranteed to be recovered: the current voice service pushes replies over
   the live connection and does not provide a durable reply inbox.
5. Keep Android's background service, native timer and notification Disconnect
   behavior intact.

Measure foreground-to-usable-terminal reconnect time on a real iPhone in
Phase 2, including brief switches to copy a public key or paste text. If those
switches prove annoying, add the following second step; it is not an initial
acceptance requirement:

- Allow a short, best-effort grace for brief app switches, with a maximum of
  five seconds. A finite iOS background task may protect completion of pending
  user-initiated writes and orderly cleanup; do not manufacture work or hold
  an idle task just to keep SSH alive. End the task when that work finishes.
- The native deadline, task expiration or failed task acquisition must lead
  to bounded cleanup; iOS does not guarantee all five seconds are given.
- If the app returns before teardown and the transport is healthy, reuse it
  and reclaim the viewport without a full terminal reattach. Validate health
  and reconcile state using an existing bounded, read-only snapshot request;
  do not trust cached `connected` state or add a new wire-level health protocol.
  Otherwise use the initial reconnect path.
- Generation guards must prevent a stale shutdown deadline or callback from
  closing a resumed or new connection. Test deadline/foreground and background
  task expiration races on device before enabling this optimization.

Foreground notification policy: retain the mobile eligibility, watermark and
deduplication rules. Suppress notifications for the currently viewed agent or
terminal pane. For another eligible agent, present a banner and keep the
notification in Notification Center, without adding sound or badge behavior
in the first milestone. Request permission through the existing connection
flow and keep in-app agent status usable after denial. Verify identifier-based
replacement and tap routing on iOS rather than assuming Android tag semantics.

Without a push path, do not promise immediate blocked/completed alerts while
the iOS app is suspended. The host's tmux sessions and agents continue running
independently of the phone's connection. Interrupted connections can lose
voice replies, including when the phone is locked; this product limitation
requires an explicit decision before calling the first milestone ready.

### Proposed push milestone

Delivery path:

```text
Muxflow host -> authenticated push relay -> APNs -> iPhone notification
                                                    |
                                               notification tap
                                                    |
                                         reconnect and reconcile via SSH
```

The host observes agent transitions even when the phone is disconnected.
A relay is the proposed sender so Apple provider credentials do not need to
be installed on every user's host. Interactive terminal and file traffic
continues over SSH.

Before implementation, specify:

- Device registration, host pairing, authorization and token revocation.
- Which hosts may notify which devices, and handling of invalid APNs tokens.
- Minimal payload contents and whether agent labels are included.
- Stable event identity, deduplication, expiry, and notification tap routing.
- Interaction with host-owned seen state, desktop/mobile attention, and
  duplicate alerts around background/foreground transitions.
- Relay ownership, hosting and Apple credentials.

Push is an alert channel; it is not an authoritative agent-state store or a
replacement for fetching the current state over SSH. A delivered alert may
already be stale when tapped. APNs delivery is not guaranteed.

Any required host/protocol changes must update every affected client in the
same change and undergo the two-direction review below. Bump the major only
if the change requires it under the bump rule below, not merely because push adds a
field or message. Do not add compatibility arms, legacy fields or
version-dependent branches.

### Release skew and admission policy

Today `validateHostContract` checks only the envelope's `protocolMajor`
(currently 4). `HostConnection.onHelloFrame` logs `helperVersion` and the build
digest but does not compare either with the app version. A newer helper with
the same major is admitted; a different major becomes a terminal
`incompatible` state. The host also refuses a different client major.
The source comment saying "exact current contract" does not mean the code
compares schema fingerprints or release versions. Desktop separately compares
the installed helper's digest with its bundled artifact in
`compare_installed_artifact`; a mismatch is incompatible. Desktop and helper
therefore stay matched, while mobile can update independently. The major's
release-skew role is protecting mobile, not replacing desktop's artifact check.
Existing additive changes in `c5ba15c` and `d378228` shipped with major 4.

Decision: retain strict major equality at admission, but bump
`protocolMajor` only for a real incompatibility: the previous released mobile
app would break or misbehave against the new host, or the new mobile app would
break or misbehave against the previous released host. Both directions matter:
desktop may update the helper before the phone updates, or the phone may
update first. Additive changes that the other side ignores harmlessly do not
bump. Harmless unknown fields rely only on protobuf's normal handling; do not
add compatibility arms, legacy fields or version-dependent branches. Do not
add a semver equality fence or infer mobile compatibility from the helper
digest. Desktop's existing bundled-artifact digest check remains unchanged.

For every wire change, review must identify the last released mobile build
and host release used as baselines and explicitly answer:

1. What does the last released mobile app do with the new host's change?
2. What does the new mobile app do against the last released host, including
   how that host handles any new fields sent by the app?

If either answer is "breaks or misbehaves", bump; otherwise do not. Include
behavioral evidence for the answers. Additive syntax alone is not proof of
safety: an app relying on a new operation, or on a field that an older host
ignores, can still misbehave. Do not work around that with version-dependent
request paths. Change affected host/desktop/mobile implementations together
and regenerate bindings and fixtures whether or not the major changes.

If the host is newer and advertises a different major, iOS must refuse it,
show the app/helper protocol mismatch and the appropriate distribution update
destination, and stop automatic retry. The same applies when the app is newer
than the host. Resume or notification taps must not bypass admission, and
control/bulk lanes must not become usable after refusal. Test:

- An older app against a newer host with a different major.
- A newer app against an older host with a different major.
- An older released app against a newer host with harmless additive fields
  under the same major.
- A newer app against an older released host that harmlessly ignores its new
  fields under the same major; absent new response fields must also be harmless.

Use the released baseline's real decoder/behavior for additive tests, not just
the new decoder with a different version label. Inspect the existing pipelined
Hello/Subscribe path to ensure no state from a refused attempt is published
or accepted as a usable connection.

Apple distribution cannot guarantee that all installed applications update
simultaneously. The desktop/host synchronized-release assumption remains;
iOS users may temporarily be unable to connect after a major bump.
Failing closed addresses that state without supporting old protocol paths.
Do not describe the rollout as guaranteed synchronized installation.

Only for a release that bumps `protocolMajor`, prepare affected artifacts from
the same release source and hold publication of the desktop/host draft until
the matching iOS build is actually available through the selected distribution
channel. Upload success alone is not readiness. Releases without a major bump
do not wait for iOS, including harmless additive wire changes. The major-bump
gate reduces the unavailable-update window, but user-paced adoption still
leaves some old iOS installations unable to connect. Document that consequence
in release notes. Do not quietly implement compatibility arms to avoid it.

## 5. Implementation phases and acceptance gates

### Phase 0 — isolated sharing and documentation

- Update the design and decisions in the first implementation commit as
  described in section 1.
- Extract Markdown, agent labels and suitable palette data into the shared
  packages; change desktop and mobile consumers together. Extract notification
  primitives only where their current behavior is equivalent.
- Land this as a separate change from iOS transport and lifecycle work. It
  does not depend on an Apple account or native iOS build.

Gate: desktop/mobile tests preserve rendering, sanitization, labels and palette
outputs. Type checks and WebView generation checks pass on Linux. Any desktop
regression is resolved before starting the iOS lifecycle change.

### Phase 1 — native feasibility and CI

- Separate SSH transport from Android service/timer concerns before adding the
  iOS adapter. Verify the separation on Linux and Android, preserving the
  existing foreground service, native wake timers and Disconnect action.
- Add iOS Expo configuration, provisional bundle ID `dev.muxflow.mobile`,
  native-module registration and reproducible native project generation.
- Select a supported deployment target and pin a compatible GitHub macOS
  image/Xcode combination based on the installed Expo/RN versions.
- Add a simulator build containing its JavaScript bundle so QA does not rely
  on a development server outside the runner.
- Prototype the SSH implementation before committing to a library. Check
  maintenance, licensing, supported algorithms and native build integration.
- Start Apple account/signing setup in parallel so device validation can run
  in Phase 2. Use internal TestFlight for the first device build; final
  TestFlight-only versus App Store distribution remains an open decision.
- Include `NSLocalNetworkUsageDescription` in the first LAN-capable build,
  not only after the permission is encountered during later feature QA.

Library spike gate: a simulator prototype demonstrates `none` authentication
with no phone key, Ed25519 public-key authentication, a host-key verification
callback, two independent exec channels on one authenticated transport, and
keepalive-based loss detection. Use this small gate to select the library
before implementing the entire module and application integration.

Full contract gate: a real simulator app builds, installs and launches without
an Apple account; the native SSH adapter meets the transport/event contract
exposed by `MuxflowSsh.ts` after separating Android service/timer methods, with
Android's `SshSession.kt` as the behavior reference:

- Offer SSH `none` authentication first, then Ed25519 public-key authentication
  when a key exists and the server offers it. A server accepting `none`, such
  as a Tailscale SSH host, must work with no phone key. Do not force key
  generation before such a connection, or add password authentication.
- Validate SHA-256 host-key fingerprints, including initial trust,
  rejection and changed-key refusal; `none` auth does not bypass host trust.
- Detect lost links using keepalives equivalent to the current 15-second
  interval and three unanswered probes, without timing out a healthy idle
  bridge. Treat this as a detection policy, not an exact 45-second wall clock.
- Emit all seven close reasons accurately: `hostKeyNotTrusted`,
  `hostKeyMismatch`, `authFailed`, `connectFailed`, `exited`,
  `closedByClient` and `networkLost`.
- Deliver stderr diagnostics and exit status, including command-not-found
  status 127 and a null status when none was obtained, without mixing stderr
  into the binary stdout stream or dropping it before the close event.
- Preserve per-channel ordered writes and the facade's chunking contract:
  at most 65,536 base64 characters (48 KiB decoded) per native write. A failed
  write must not wedge later writes or corrupt another channel.
- Run the host bridge and keep control/bulk exec channels independent on one
  authenticated transport, including independent channel close and cleanup.

Reject the transport choice if it cannot meet the existing contract. Do not
weaken authentication or host-key verification to make the prototype pass.

### Phase 2 — lifecycle and early physical-device validation

- Implement iOS key generation and Keychain storage with
  `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, no iCloud synchronization,
  and stable app-owned service/account identifiers. Private key material stays
  native. This accessibility class permits access only while unlocked and
  does not migrate the item to another device; no background reauthentication
  while locked is required by the initial lifecycle policy.
- On reinstall, reuse an existing accessible SSH key under those identifiers;
  do not rotate it just because application files were removed. Keychain
  persistence across uninstall is not a guarantee, so handle an absent item
  as no key without silently generating one. Explicit Delete/Replace removes
  the app-owned item; verify deletion and reinstall/reuse on an iPhone.
- Add the background/foreground behavior from section 4, including prevention
  of duplicate dials, stale channel callbacks and unintended reconnect after
  user disconnect. Start with teardown-and-reconnect, measure time to a usable
  terminal on device, and add the specified grace only if brief switches prove
  annoying.
- Implement the strict admission/update experience from section 4 using the
  existing major check. Add both mismatch-direction and same-major additive
  behavior tests, with released baselines and the two-direction wire review;
  do not add a second compatibility mechanism.
- Produce a signed internal TestFlight build by the end of this phase and
  validate brief app switches, locking/unlocking, longer background periods,
  network loss and explicit disconnect on an iPhone.
- Include LAN permission grant/denial/recovery in this early device pass. Do
  not interpret a generic socket failure as proof of permission denial.

Gate: lifecycle tests cover interrupted dials, host-key prompts, transient
inactive states, user disconnect, reconnect, control epoch changes and
independent bulk-channel failures. A real iPhone demonstrates the lifecycle
behavior and records reconnect timing; simulator switching is insufficient.
Grace/expiration races become an additional gate only if grace is implemented.
If credentials or device access are unavailable, other work can proceed, but
this gate stays open and the lifecycle milestone is not complete. Android and
desktop checks continue to pass.

### Phase 3 — existing mobile features on iOS

- Configure icons and microphone/notification permission text.
- Verify the Local Network system prompt, denial, Settings recovery and
  subsequent reconnect on an iPhone, using the permission text added in
  Phase 1; the simulator does not enforce Local Network privacy.
  Keep the seven SSH close reasons unchanged. When native APIs cannot identify
  denial reliably, show a conditional Local Network Settings hint alongside
  connection diagnostics rather than labeling every `connectFailed` a denial.
- Implement iOS font registration. `expo-font` currently has only Android
  configuration; register bundled JetBrains Mono faces for iOS, determine
  their actual family/PostScript names and map regular/bold text appropriately.
  Verify native Text and WebView fonts separately.
- Replace Android-only toasts and service wiring with platform adapters.
- Adapt notifications, permission handling and update destinations. iOS must
  not offer Android release artifacts as an update path.
- Validate terminal WebView startup, sizing, keyboard insets, safe areas,
  rotation, selection/copy, links, alternate-screen scrolling and input.
- Make the embedded WebView bundles target the supported Safari/WKWebView
  version as well as Android's WebView.
- Validate file listing, plain text and sanitized Markdown rendering.
- Validate voice recording, interruption, playback and foreground re-register
  behavior. Retain platform audio options where required.

Gate: hosts, agents, terminal, files and voice are usable on iOS within the
first milestone's lifecycle limits. No Android or desktop regression is
accepted as the cost of sharing code.

### Phase 4 — end-to-end evidence and distribution readiness

- Complete the simulator harness described below and run it in Actions.
- Save app screenshots, relevant logs and test results as workflow artifacts.
- Run shared/mobile checks, protocol/WebView generation checks and affected
  desktop/Rust tests.
- Extend the signed-device path already used in Phase 2 for the chosen
  distribution channel. Final device QA covers keyboard, fonts, rotation,
  audio interruptions/routes, notification presentation and permission recovery.
- Integrate iOS version/build checks and distribution readiness into release
  preparation as described in section 7. Hold the desktop/host draft only
  when the major bumps; releases without a bump do not wait for iOS. Test
  both mismatch/update directions and same-major additive behavior with actual
  released builds, not just mocked hello frames.

Gate: report exactly which flows ran and passed. Failed, unavailable or
unimplemented checks remain explicit blockers rather than assumed coverage.

### Phase 5 — background push

Implement the specification from section 4 after its scope and infrastructure
are settled. Test host-to-relay authorization, delivery, deduplication and tap
routing, then validate real background alerts on an iPhone.

Gate: an agent transition while the app is backgrounded can produce an alert;
tapping it reconnects and opens the correct current target. Document offline,
stale-event and notification-permission behavior.

## 6. QA that can run from Linux

The proposed simulator activity is **automated end-to-end QA**, not an
interactive manual session with a remote simulator desktop. It does not need
an Apple Developer account.

A GitHub Actions macOS job will:

1. Build the simulator app with Xcode.
2. Use `xcrun simctl` to boot a compatible simulator, install the app and
   launch it.
3. Start an isolated SSH/tmux/Muxflow-host fixture reachable from that
   simulator, with disposable keys and deterministic terminal/agent state.
4. Run a CLI UI driver such as Maestro locally on the runner. It can tap
   controls, enter text, inspect the accessibility hierarchy and capture
   screenshots. No Maestro Cloud account is needed for local CLI execution.
5. Drive host setup, verify the presented host-key fingerprint, connect,
   open a terminal, send a command with a distinctive result, and check
   input/output through the real SSH transport. Use fixture observations as
   well as UI evidence where xterm's canvas has no accessible text nodes.
   Include both a key-based fixture and a server accepting SSH `none` with
   no phone key. The latter verifies the auth path; it does not alone prove
   real Tailscale identity/routing behavior. Validate a real tailnet host in
   device QA where one is available.
6. Exercise files/Markdown, switch away and reopen the app, and confirm
   state reconciliation and terminal reattachment. Force a transport loss
   to test recovery independently of simulator suspension behavior.
7. Collect screenshots, logs and reports for inspection from Linux.

Build and prove a minimal launch/tap/screenshot flow before expanding this
harness. Maestro is a candidate, not a confirmed working integration in this
repository. If it cannot exercise an essential control reliably, evaluate the
smallest suitable XCTest UI flow instead of bypassing the product surface.

Simulator app switching is not proof of real iOS suspension timing, process
termination, lock-screen delivery, hardware keyboard behavior, microphone
quality, Bluetooth routing, Local Network permission or haptic behavior.
Start hands-on device QA in Phase 2 and expand it in later phases. Simulator
artifacts can support visual inspection, but do not replace that device QA.

## 7. Inputs and open decisions

| Input or decision | Needed when |
| --- | --- |
| Whether background alerts are required in the first shipped release | Before fixing release scope; the foreground client can be developed independently. |
| TestFlight-only versus App Store distribution | Decide before release automation is finalized. Recommend staying on internal TestFlight while the protocol changes often, then reassessing broader distribution. It reduces review friction but does not eliminate user-paced adoption or major-mismatch refusal. External TestFlight may require beta review and builds expire after 90 days; an App Store release needs its own review/readiness gate. |
| Accepting missed voice replies during lock/suspension | Decide before the first milestone is called ready. Default scope preserves existing live delivery, without a durable inbox. If missed replies are unacceptable, plan host-side bounded retention/replay as a separate product/protocol change; APNs alerts alone do not recover voice audio. |
| SSH library and supported iOS/Xcode versions | Resolve through the initial feasibility phase; no user decision required unless a material tradeoff appears. |
| Bundle ID | Use `dev.muxflow.mobile` provisionally; confirm availability before Apple registration. |
| Apple Developer membership and team ID | Begin setup during Phase 1; required for the chosen signed TestFlight path and the Phase 2 device gate. |
| Signing certificate/profile and App Store Connect credentials in GitHub secrets | Before the Phase 2 signed build. Do not paste private keys into chat. |
| Encryption export declaration | Resolve before enabling TestFlight installation, including the selected SSH library and third-party crypto. The first upload may remain Missing Compliance until the account holder completes Apple's questionnaire; leave `ITSAppUsesNonExemptEncryption` unset until resolved. See [TestFlight setup](testflight.md). Do not assume an SSH app uses only exempt OS encryption. |
| An iPhone and user participation | By Phase 2 for lifecycle and LAN permission; later for audio and background alerts. |
| Push relay ownership, hosting and APNs credentials | Before the push milestone. |

### Versioning and release integration

- Keep the shared `X.Y.Z` app/helper version managed by
  `release/set-version.sh`. Set the iOS marketing version from that same
  value; do not give iOS a separate product semver.
- Extend that script to accept and persist an explicit Apple-valid,
  monotonically increasing `ios.buildNumber` separately from Android's
  semver-derived `versionCode`. Extend `release/check-version.sh` to validate
  the iOS fields and fail when they are missing or inconsistent.
- Allocate a new build number for each distinct binary uploaded, including
  rebuilt/release-candidate binaries under the same marketing version.
  Re-running an upload of the same artifact must reuse the artifact or skip
  the existing upload, not rebuild different bytes under the same number.
  Do not derive this number solely from `X.Y.Z` or a workflow attempt count.
- Extend `release.yml` or a dedicated iOS workflow to build/upload from the
  release source, validate the recorded versions and capture the App Store
  Connect build identifier. Native prebuild must retain the recorded number.
  Source changes needed for a rejected build require a new release candidate
  for affected artifacts, not a silent iOS-only wire change. Assess any wire
  change in both directions under the bump rule in section 4; a new candidate or new build
  number alone is not a reason to bump the major.
- Record per-channel install/update destinations and readiness separately
  from the existing Android/Desktop GitHub release manifest. Follow the
  major-bump release gate in section 4. A release without a major bump does
  not wait for iOS readiness; App Store processing or review is not a
  synchronous successful GitHub job.

Neither a personal Mac nor an Expo account is required for the proposed
GitHub Actions build path.

## 8. References

- [Existing mobile design](design.md), [decisions](decisions.md), and
  [voice plan](voice-mode-plan.md).
- [Native SSH facade](../../apps/mobile/src/ssh/MuxflowSsh.ts),
  [Android auth/keepalive/channel behavior](../../apps/mobile/modules/muxflow-ssh/android/src/main/java/dev/muxflow/ssh/SshSession.kt),
  [connection manager](../../apps/mobile/src/session/connectionManager.ts),
  and [Android Expo module](../../apps/mobile/modules/muxflow-ssh).
- [Mobile contract validation](../../apps/mobile/src/protocol/contract.ts),
  [mobile handshake](../../apps/mobile/src/protocol/HostConnection.ts), and
  [shared Rust contract](../../crates/protocol/src/lib.rs).
- [Desktop helper artifact comparison](../../apps/desktop/src-tauri/src/connection/helper.rs).
- [Shared terminal interactions](../../packages/terminal-interactions).
- [Current CI](../../.github/workflows/ci.yml) and
  [release workflow](../../.github/workflows/release.yml).
- [Release version writer](../../release/set-version.sh) and
  [release version checks](../../release/check-version.sh).
- [Expo simulator builds: no Apple Developer account required](https://docs.expo.dev/build-reference/simulators/).
- [Expo native modules](https://docs.expo.dev/modules/overview/).
- [GitHub macOS runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
- [Apple Xcode command-line tools](https://developer.apple.com/documentation/xcode/xcode-command-line-tool-reference).
- [Maestro iOS support](https://docs.maestro.dev/get-started/supported-platform/ios)
  and [how it drives UI](https://docs.maestro.dev/get-started/how-maestro-works).
- [Apple background execution limits](https://developer.apple.com/forums/thread/685525).
- [Apple finite background task guidance](https://developer.apple.com/forums/thread/85066/).
- [Keychain accessibility class](https://developer.apple.com/documentation/security/ksecattraccessiblewhenunlockedthisdeviceonly)
  and [uninstall/persistence caveat](https://developer.apple.com/forums/thread/36442).
- [Protobuf unknown-field behavior](https://protobuf.dev/programming-guides/proto3/#unknowns).
- [Apple Local Network privacy, including simulator limitations](https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy).
- [TestFlight review and build expiration](https://developer.apple.com/help/app-store-connect/test-a-beta-version/testflight-overview/).
- [App Store Connect version/build identification](https://developer.apple.com/help/app-store-connect/manage-builds/upload-builds/).
- [Encryption export declarations](https://developer.apple.com/help/app-store-connect/manage-app-information/overview-of-export-compliance).
- [Apple remote notification servers](https://developer.apple.com/documentation/usernotifications/setting-up-a-remote-notification-server).


## 9. Implementation evidence

The automated foreground gate passed at `adfd81f` in
[iOS run 37198926444](https://github.com/gal064/muxflow/actions/runs/37198926444),
using an iPhone 17 Pro simulator on iOS 26.4 under Xcode 26.6. All four reports
have zero failures: keyless `none` auth, forced-loss recovery, key generation,
and generated-key auth after an app process restart. Both authentication flows
verified host trust, a real helper handshake, terminal commands/output,
Markdown and app-switch resume. The script checked host-side marker files and
tmux output; retained screenshots show the rendered document and terminal
results. The copied public key was retained as a canonical OpenSSH line;
private key material stayed native. No bounded navigation or input retry was
needed in this run. This verifies Keychain reuse across that restart, not
reinstall or locked-device behavior.

[Repository CI run 37198926358](https://github.com/gal064/muxflow/actions/runs/37198926358)
also passed on Linux and macOS for that revision. The earlier failures below
record build and harness corrections; they are not outstanding simulator gates.
Their remaining device checks and unconfirmed causes are explicitly noted.

Work is on `feat/ios-mobile`; the shared-source extraction is commit `2773203`.
The implementation keeps the mobile protocol client, screens, terminals,
files and voice in TypeScript. Android service/wake APIs have a separate
adapter; iOS uses JS timers and immediate background teardown with foreground
reconnect. Retained voice conversations survive that reconnect. The optional
five-second grace remains deferred to measured iPhone behavior.

The native candidate is libssh2 1.11.1 through the pinned Apple build
`libssh2-iosx` 1.11.1.0 with OpenSSL `openssl-iosx` 3.5.9.1. Contract patches
preserve exit-status presence, enforce the approved host pin during rekey,
retain/retry keepalive packets and permit channel cleanup without waiting for
a remote close acknowledgement. Pending outbound operations retain their
original arguments until completed; channel-open replies are bounded to ten
seconds after authentication, and EOF/status wait is bounded to five seconds.
The native harness exercises these behaviors against an isolated SSH server;
changing dependency pins requires rerunning that harness and simulator flows.
Private keys remain native in Keychain under the specified accessibility class.

Linux evidence so far: the library spike passed none and Ed25519 auth,
host-key comparison, independent channels, stderr/status presence and
keepalive sends. All 605 mobile tests passed, including the real helper/tmux
terminal and file suites; both desktop/mobile TypeScript checks passed.
Full Linux/macOS repository CI passed in
[run 37198926358](https://github.com/gal064/muxflow/actions/runs/37198926358),
including desktop builds, Rust lint/tests and Linux frontend/mobile/generated
checks. The Android debug APK also builds successfully on Linux.
Additional tests freeze the real generated descriptor from released `v0.1.9`
(`e48f46c`) for same-major unknown-field decoding in both directions. Those
codec tests do not claim a released iPhone binary has been exercised. The
current mobile live terminal/file suites also passed against the actual
SHA-256-verified Linux helper asset from the published `v0.1.9` release. There
are no wire changes in this implementation and the major remains 4.

`.github/workflows/ios.yml` selects Xcode 26.6 on `macos-26`. It runs the native
engine harness, builds a standalone simulator app with local ad hoc signing,
then drives
host setup, host-key trust, terminal input, Markdown and foreground reconnect
with local Maestro 2.11.0 and the real helper/tmux. It includes keyless none
auth and a phone-generated public key, and checks fixture observations where
xterm's canvas has no accessible text. Screenshots, app/build logs and reports
are retained as artifacts. Written flows and Linux prebuild are not evidence
that this native build or simulator QA has passed.

The initial update action opens TestFlight; iOS does not poll the Android
release manifest. The mismatch screen also links to the desktop release so
users can update its matched helper when the phone is ahead. Release scripts
preserve the shared marketing version and
accept an explicit increasing iOS build number; a retry must reuse the binary.
Signed upload automation and publication readiness remain gated on the Apple
team, signing/App Store Connect inputs, export declaration and distribution
choice. The real-iPhone lifecycle/LAN/audio/notification gates are still open.
Push delivery and the missed-voice-reply product decision remain separate open
scope; this work does not claim either milestone is ready to ship.


The native contract suite passed in Actions run
[37176795634](https://github.com/gal064/muxflow/actions/runs/37176795634),
including all seven reasons, delayed auth/cancellation, rekey pins, binary
writes, bounded EOF and cancelled-open cleanup, and healthy-idle/lost-link
keepalives. That run's app build failed in ExpoModulesJSI before simulator
installation, so it supplies no UI evidence. SDK 57 requires Xcode 26.4+;
the job now selects 26.6. A narrow package patch removes invalid ownership
annotations from the pinned ExpoModulesJSI constructors, as described in the
[upstream issue](https://github.com/expo/expo/issues/49214). The next
[run 37179091744](https://github.com/gal064/muxflow/actions/runs/37179091744)
compiled the native Swift/Objective-C module for both simulator architectures,
then failed because Expo's app provider imported the vendored libssh2 pod as
an Expo module. An explicit module podspec path now restricts that registration
to `MuxflowSsh`, verified with Expo's resolver on Linux. Future simulator builds
compile only the runner's architecture. Simulator QA remains open until the
updated standalone app build and real UI flows pass.

[Run 37181184025](https://github.com/gal064/muxflow/actions/runs/37181184025)
built, installed and launched the app without an Apple account. Its first UI
flow stopped at the key screen: the native back button was labeled `Muxflow`
with ID `BackButton`, and the screenshot showed a key-read failure. App logs
reported Keychain error `-34018` while build settings disabled signing. The
simulator script now enables local ad hoc signing, following Xcode's simulator
path, and retains signature/entitlement diagnostics. This uses no distribution
certificate or profile. The earlier missing-key assertion alone was
insufficient evidence.

[Run 37183766233](https://github.com/gal064/muxflow/actions/runs/37183766233)
passed the native contract suite, built an ad hoc signed simulator app, and
installed and launched it. The clean no-key UI assertions passed, including
absence of the key-read error; the screenshot and app logs confirm that the
earlier Keychain failure is absent. Back navigation and host-field input also
passed. The flow stopped before Save because Maestro's iOS swipe-based
`hideKeyboard` did not dismiss the host form's keyboard. The flow now taps
its observed native `Return` key instead.

[Run 37186526164](https://github.com/gal064/muxflow/actions/runs/37186526164)
reused the exact cached app and passed Return dismissal and host Save. It
stopped before connection because iOS groups the host title/address/chevron
into one accessibility label. The harness now matches grouped row labels
and uses Files navigation to dismiss the terminal keyboard. This run did not
exercise host trust or the downstream SSH UI gates.

[Run 37187836341](https://github.com/gal064/muxflow/actions/runs/37187836341)
passed grouped host-row selection, then failed before host trust with native
`E_TARGET`. Expo's pinned dictionary conversion hydrates JavaScript numbers
as Swift `Double`; the adapter's direct `Int` cast rejected valid ports before
opening a socket. The adapter now converts with `Int(exactly:)`, preserving
integer/range validation. This required a fresh app build and full UI rerun;
the engine-only native harness does not exercise the Expo argument bridge.

[Run 37189097150](https://github.com/gal064/muxflow/actions/runs/37189097150)
rebuilt the app and passed clean no-key handling, the exact fixture fingerprint
assertion, host trust, `none` authentication and the real helper protocol
handshake/topology. The port conversion fix is verified through the Expo bridge.
The next action stopped at a redundant `Allow` tap after notification permission
was already granted. The harness now relies on its declared launch permission
and waits for the application UI. This run did not complete terminal,
Markdown, resume, generated-key auth or forced-loss recovery.

[Run 37191748811](https://github.com/gal064/muxflow/actions/runs/37191748811)
passed the full keyless flow: real terminal input/output, rendered Markdown
and foreground resume, corroborated by screenshots, tmux output and marker
files. Forced-loss recovery and phone-generated Ed25519 key/public export also
passed. The key-auth setup then stopped because a reported centre Add host
tap left the app on the empty host list. Its cause is unconfirmed; no navigation
error was logged. The harness now selects the observed floating Add host
control and asserts the form before typing. Verify the centre action after
key copy/back during device QA. This run did not exercise generated-key SSH auth.

[Run 37193438193](https://github.com/gal064/muxflow/actions/runs/37193438193)
repeated the keyless terminal/Markdown/resume, recovery and key-generation
passes. The floating Add host tap also left the separate key-auth flow on the
empty host list, so changing controls did not resolve it. That flow lacked
the explicit app launch used by the others. It now starts a fresh app session
without clearing state or Keychain, which also tests that the generated key
survives a process restart. The final passing flow verifies that restart and
authentication; the earlier missed taps' cause remains unconfirmed.

[Run 37195070411](https://github.com/gal064/muxflow/actions/runs/37195070411)
stopped earlier in the unchanged keyless flow: the connection was healthy,
but a reported Workspaces tap left Agents selected. Maestro considered an
unrelated hierarchy change sufficient to finish the tap. Add host and
Workspaces navigation now wait for animations and assert their destinations
inside a single bounded retry, retaining failed-attempt screenshots. This
does not establish whether the missed taps originate in the driver or app;
verify both interactions on a device. No auth, terminal or reconnect operation
is retried by this harness change. The subsequent
[run 37196285422](https://github.com/gal064/muxflow/actions/runs/37196285422)
passed the full keyless flow, recovery and key-generation UI, then stopped
at clipboard export before key-auth started. App logs confirm a native
pasteboard write; the exported value was not retained, so the failed read or
format check cannot yet be distinguished. The harness now polls the clipboard
read and retains its public value and error. This run did not exercise
generated-key auth. For the earlier Workspaces miss, the logs show iOS becoming
inactive for notification permission immediately before the tap; that
specific miss was an OS prompt timing race.

[Run 37197667874](https://github.com/gal064/muxflow/actions/runs/37197667874)
did not reach clipboard export: the driver reported entering `127.0.0.1`,
but the saved host was `1`, so it dialled the wrong endpoint. The pinned
[iOS text-input helper](https://github.com/mobile-dev-inc/maestro/blob/cli-2.11.0/maestro-ios-xctest-runner/maestro-driver-iosUITests/Routes/Helpers/TextInputHelper.swift)
already slows its first character to work around dropped input. The harness
now enters the initial address through that single-character path and checks
the exact host and port before saving. This isolates scripted input loss;
normal device typing remains part of device QA. The final passing run reached
clipboard export and generated-key authentication; the earlier clipboard
failure's cause remains unconfirmed because its value was not retained.

UI flows now use native back/input identifiers and assert that a missing-key
state has no read error. The workflow caches only an exact app/dependency and
resolved runner/Xcode/Node/CocoaPods match, before installation creates generated
files. Version checks, native contract tests and UI fixtures still run on cache
hits. A successfully built simulator app is saved before UI assertions so
test-flow-only corrections can reuse it. These simulator artifacts are separate
from signed device/distribution builds.

To reproduce the automated checks from a macOS checkout, install the pinned
pnpm dependencies, uv, tmux, ripgrep and Maestro 2.11.0, then run:

```sh
bash tests/mobile/ios/check-native.sh
pnpm mobile:ios:simulator
bash tests/mobile/ios/launch-simulator.sh
bash tests/mobile/ios/test-simulator.sh
```

The UI harness also needs `cargo build --locked -p muxflow-host` first. It
creates an isolated SSH server and tmux socket, cleans them up on exit, and
writes evidence to `tmp/ios-evidence`. The Actions job runs these same steps
and retains its logs, reports, screenshots and simulator app. On Linux, use
the `ios` Actions workflow rather than attempting to run Xcode locally.
