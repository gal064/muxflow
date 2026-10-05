# Port forwarding: design contract

A **Ports** tab in the right panel, beside Files and Git. Type a port and it is
forwarded from the SSH host to the same port on this machine. Only the desktop
(React + Tauri) changes: the wire protocol and the host helper do not, so mobile
is untouched and `PROTOCOL_MAJOR` does not move.

## Decisions

- **Same port by default.** Typing `3000` forwards remote `3000` to local
  `3000`. A second, optional field sets a different local port; empty means
  the same.
- **Not remembered.** Forwards live for the app session. Nothing is persisted;
  after a restart you forward again (auto-detect makes that one click).
- **Stops when Muxflow stops.** Removing a forward, removing its host,
  quitting, crashing and `kill -9` all stop it. No forward outlives the app.
- **Auto-detect on demand.** Listening ports on the host are suggested when the
  tab opens and on ⟳. No background polling, no auto-forwarding.
- **SSH hosts only.** Local has nothing to forward and is not in the picker.

## UI

```
[ Files | Git | Ports ]
 Host: [ devbox ▾ ]            ← only when 2+ SSH hosts are shown; defaults to the active host
 [ 3000 ] → [ same ] [Forward]   ← Enter submits; the local field is optional
 ● 3000 → 127.0.0.1:3000   ✕   ← click opens http://127.0.0.1:3000
 ● 5173 → 127.0.0.1:5173   ✕
 ⚠ 8080 → 127.0.0.1:8080  Retry ✕
     bind [127.0.0.1]:8080: Address already in use
 Detected on host  ⟳
   3000 node   5173 vite   8000 python   5432
```

- A row is `starting`, `active` or `failed`. A failed row shows ssh's own message.
- A forward that dies stays `failed` with a retry; there is no restart loop.
  After a network drop you retry by hand.
- Clicking a detected port forwards it to the same local port.

## Forwarding (Rust)

New module `apps/desktop/src-tauri/src/connection/ports.rs`.

One process per forward, built from `ssh_base` so it shares keepalives,
`BatchMode` and `ConnectTimeout` with every other lane:

```
ssh -v -N -o ControlMaster=no -o ControlPath=none \
    -L 127.0.0.1:<local>:localhost:<remote> <target>
```

- **Own connection, not the control master.** A forward added to the master
  with `-O forward` outlives its client, needs a separate `-O cancel`, and
  vanishes silently when the master resets. A dedicated process is removed by
  ending it and fails visibly. The cost is one extra key-auth handshake per
  forward, the same trade the bulk lane already makes.
- **`127.0.0.1` only on this machine.** Bound as `localhost`, ssh settles for
  `::1` alone when `127.0.0.1` is taken, so the forward looks healthy while the
  browser reaches whatever holds the port (seen in testing). One explicit
  address makes a busy port fail instead, and the tab opens `127.0.0.1`, not
  `localhost`, so a local server on `::1` cannot answer in its place.
- **`localhost` on the remote side** reaches servers bound to `::1` as well as
  `127.0.0.1` (Vite and others often bind `::1` only).
- **Only our own port counts.** The host's ssh config applies, so ssh also
  binds any `LocalForward` it lists, which the control master usually already
  holds. `-v` reports each listener; a forward turns `active` on
  `Local forwarding listening on 127.0.0.1 port <local>` and `failed` on
  `bind [127.0.0.1]:<local>: …`, which also drops the guard to end ssh.
  ssh prints the listening line just before it binds, so a refused bind
  passes through `active` for an instant; the refusal decides.
  `ExitOnForwardFailure` is deliberately off: it would fail a forward over a
  config port nobody typed. Re-requesting the config's forwards is what the
  control master and the bulk lane already do with the same config.

### Lifetime: a pipe guard

Each forward runs inside a POSIX `sh` wrapper whose stdin is a pipe held by
Muxflow:

```sh
ssh -N -L ... <target> & pid=$!
( cat >/dev/null; kill $pid ) &
wait $pid
```

The OS closes the pipe however Muxflow exits — quit, crash or `kill -9` — so
the wrapper kills `ssh` within milliseconds. The ✕ button, host removal and
quit all use the same path: drop the pipe. This is portable across Linux and
macOS, unlike `PR_SET_PDEATHSIG`, and needs no new binary.

Quit additionally stops every forward next to `close_all_control_masters()` in
`lib.rs`, so a graceful exit does not depend on the guard.

### Commands

| Command | Does |
| --- | --- |
| `ports_forward(connection, remotePort, localPort)` | Starts a forward |
| `ports_stop(profileId, remotePort)` | Drops the guard pipe |
| `ports_stop_host(profileId)` | Stops a host's forwards when it is no longer shown |
| `ports_list()` | Every forward and its state, all hosts |
| `ports_detect(connection)` | Listening ports on the host |

State changes are pushed to the renderer as events. A process-wide registry
keyed by profile id owns the children; removing a host stops its forwards.

## Auto-detect (Rust)

`ports_detect` runs `ss -ltnHp` over the host's existing control master (the
same lease the helper probe uses), so there is no new handshake: one round
trip, on its own channel, never in the terminal's path.

Suggestions keep a port only if it is:

- `>= 1024` (drops 22, 53, 631 and other system services),
- bound to loopback or a wildcard address (a forward can reach it),
- not already forwarded.

The user's own processes (those `ss -p` can name) sort first. If `ss` is
missing or fails — macOS hosts, minimal images, a host still reconnecting —
there are simply no suggestions; ⟳ retries and typing a port still works. An
`lsof` fallback can come later.

Cost: zero while the tab is closed. Per click, one round trip plus `ss -p`
walking `/proc`, expected in the tens of milliseconds on a busy host
(unmeasured). Dropping `-p` makes it near free at the price of process names.

## Renderer (TypeScript)

- `features/shell/types.ts`: `PanelSurface` gains `"ports"`; persisted-state
  normalization accepts it.
- `features/shell/RightPanel.tsx`: third tab; arrow keys cycle three tabs.
- `commands/registry.ts`, `features/shell/responsiveShell.ts`,
  `features/shell/useShellCommands.ts`, `app/App.tsx`: `view.showPorts`.
- New `features/ports/`: `api.ts` (invoke + events), `usePortForwards.ts`
  (per-host forward state), `PortsPanel.tsx`.
- `app/AppRightPanel.tsx`: wires `PortsPanel` with the shown SSH hosts and the
  active host as the default selection.

## Tests

Rust:
- ssh argument construction (bind addresses, options, config path).
- `ss` output parsing and filtering, including IPv6 and wildcard addresses.
- Guard: kill the parent with SIGKILL, assert the forward's port is released.
- Registry: a refused bind on our port fails the forward with ssh's message
  and ends ssh; config forwards neither fail nor activate it; a failed
  forward can be retried.

TypeScript:
- Three-tab switching and keyboard cycling; persisted `"ports"` restores.
- Port input defaults local to remote; invalid ports are refused.
- Host picker appears only with two or more shown SSH hosts.

Manual QA against a real host: forward, `curl localhost:<port>`, ✕ frees the
port, `kill -9` Muxflow frees the port, a busy local port shows the error.

## Estimates

Confidence 85, risk 20, complexity 30 (25 for forwarding, +5 auto-detect).
The main unknown is ssh behaviour across sleep and network changes; a short
spike against a real host before building the UI settles it.

## Out of scope

Remembering forwards, background port polling, auto-forwarding, remote (`-R`)
forwards, binding beyond loopback, macOS-host detection, mobile.
