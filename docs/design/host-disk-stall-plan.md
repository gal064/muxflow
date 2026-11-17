# Host daemon under a stalled disk: root cause and fix plan

Written 16 Nov 2026 from the incident on `omarchy` that day. It reviews the
handoff that was pasted with the report, corrects it, and plans the fix.
Evidence: `/tmp/muxflow-1000/{daemon-start.log,daemon.stderr.log,bridge.log}`,
`~/.local/state/muxflow/`, and the host source at `07cd7e5d`.

## 1. What happened

Two `cargo test -p muxflow-gpui` runs linked eleven integration binaries
each, at once. The disk stalled (68–82 % of time waiting on I/O). Then:

| Time | Log | Meaning |
| --- | --- | --- |
| 15:29–15:31 | hook ingests of 8–30 s | first build; the daemon is slow but lives |
| 16:13–16:14 | ingest 16 s; 5 `handshake-failed`; bridge `error` ×2, `daemon-eof` | a bridge killed the daemon (§2.4); a new one starts |
| 16:15–16:18 | ingests 20–64 s; one tmux action times out at 17.5 s; `ordered-lane-stalled` | the desktop's connection is dropped |
| 16:18–16:21 | `daemon-eof` ×2, then 14 bridge `error` exits | killed again; nothing answers for three minutes |
| 16:23–17:05 | 530 `handshake-failed`, 52 `writer-failed`, ingests rising from 60 s to **951 s** | this daemon lives, and is useless: every hook waits in a queue up to 16 minutes long |

Left behind in the state folder: 138 `.hook-fallback-*.tmp` and 4
`.agents-*.tmp` files, 87 of them from that day.

## 2. Root cause

Four things, each making the next worse. The handoff found the first.

### 2.1 Blocking work on the async runtime (handoff: right)

`agent_dispatch::handle` calls `handle_inner`, a plain function, inside the
async task. For a hook that is `ingest_live_hook`, all of it synchronous:

1. `ingest_fallbacks()`: takes `INGEST_LOCK`, takes the mailbox `flock`,
   reads the state folder, and runs a full ingest for every waiting file;
2. `server_identity()` and `discover_authoritative()`: tmux subprocesses;
3. `ingest_order.lock()`, then `state.lock()`, then `store::persist`: write,
   `sync_all`, rename, `sync_all` on the folder. Both locks are held across
   both fsyncs.

On btrfs under load an fsync of the folder waits for a transaction commit
(`wait_for_commit`, as seen), which took tens of seconds.

The same holds for every other agent operation (`mark_seen`, host naming,
hook management, launches): one took 24.6 s at 16:15.

### 2.2 The stall spreads by runtime threads, not by a shared queue (handoff: wrong)

The handoff says tmux actions share the ordered queue with hook ingests.
They do not: `run_ordered_requests` is spawned per connection, and each hook
is its own connection. What is shared is:

- **The runtime's worker threads** (one per core, 16 here). Every hook that
  waits for `INGEST_LOCK` or `ingest_order` parks a worker on a `std` mutex.
  Sixteen waiting hooks and the daemon runs nothing: no accept, no
  handshake, no terminal output. That is the 530 `handshake-failed` lines,
  each a connection whose client had already given up by the time the
  daemon reached it (lifetimes of 2 ms).
- **The `state` mutex**, held across the fsyncs. A desktop's `AgentSnapshot`
  or history request takes it on a worker and waits there.

The handoff is right that `timeout(ORDERED_REQUEST_TIMEOUT, …)` cannot fire
around a call that never yields, which is why the log shows "slow" at 951 s
instead of a timeout at 10 s.

### 2.3 A retry loop that feeds itself (handoff: missed)

The measured "ingest" time is mostly queueing, and the queue grows because
slowness creates work:

- The hook client waits 1 s to connect, 1 s for the hello and 2 s for the
  answer (`hook.rs`). After that it writes the event to the mailbox itself:
  again a write, fsync, rename, fsync, under the mailbox `flock`, **inside
  the agent's hook**. The agent's tool call waits for it.
- If the daemon had already read the request, it still applies it later, and
  the mailbox now holds a second copy.
- Every later live ingest drains the mailbox first, one full ingest with two
  fsyncs per file, holding the `flock` the hook clients are waiting for.
- A client killed by its agent's hook timeout mid-fsync leaves its `.tmp`
  file; nothing ever removes those.

So one slow fsync makes several more, and arrival outruns service. That is
how 20 s fsyncs became a 16-minute queue, and it is why the agents
themselves slowed down: their hooks were blocked on the same disk and lock.

### 2.4 The bridge kills a daemon that is only slow (the handoff's open question)

The daemon does not crash and has no watchdog. `bridge.rs`,
`inspect_existing_daemon`:

```rust
let same_build = matches!(
    timeout(Duration::from_secs(2), daemon_is_current_build(&mut probe)).await,
    Ok(Ok(true))
);
…
daemon::stop_or_retire(path).await?;
```

No hello within 2 s is treated as "a different build". `stop_or_retire`
asks for a shutdown for 2 s and then sends SIGTERM to the recorded pid. The
daemon has no SIGTERM handler, so it dies on the spot. Every desktop
reconnect during the stall did this. The replacement then starts under the
same load, and `daemon::run` drains the whole mailbox synchronously before
its first `accept`: the three minutes with no daemon at 16:18.

After 16:21 no bridge connected (the desktop had stopped trying), so that
daemon survived, starved.

### Not a regression

The fsyncs and the drain-before-live rule are from `ac1c95d5` (16 Sep). The
retire-on-silent-probe is from `d5c0147a` (5 Oct). Today's release changed
none of it; the load did.

## 3. Review of the handoff's plan

| Handoff | Verdict |
| --- | --- |
| 1. Run the persist in `spawn_blocking`, keep `ingest_order` | Right direction, wrong cut. The persist sits inside `try_ingest_hook_with_context` under two `std` guards that cannot be held across an await. Move the whole `handle_inner` call instead (§4, H1). And it does not end the incident alone: the queue of §2.3 and the kill of §2.4 remain. |
| 2. A separate queue for hook ingest | Not needed: it already has one per connection. |
| 3. Batch `agents.json` writes | Replaced by H3: the fsyncs buy nothing here, so remove them instead of batching them. |
| Open: why the daemon exits | Answered in §2.4. |
| Build is wasteful | Agreed, §5. |

## 4. Fix

In this order. H1 and H2 are safe to ship alone and together end the
disconnects. H3 ends the queue.

### H1. No agent operation runs on a runtime worker

`agent_dispatch::handle` runs `handle_inner` through
`tokio::task::spawn_blocking` (its arguments are already `Arc`s or owned;
`cancellation` is passed as the `Arc` the dispatcher holds). Same for the
two other sync callers on async tasks: `history_dispatch.rs`'s
`AgentRuntime::global().snapshot()` and `chat/session.rs`'s `report` if it
runs on the runtime (checked: it is already on the blocking pool; see §7 for
what else was found).

Effects: a stalled disk no longer stops accept, handshake or terminal
traffic; the ordered lane's 10 s bound works again, because the lane now
awaits a join handle. Ordering is unchanged: `ingest_order` still
serialises ingests, and the lane still awaits each request before the next.

Test, in the style of `stall-ordered-lane`: on a two-worker runtime, hold
`ingest_order` from a test thread, send three hook ingests on three
connections, and assert that a fourth connection completes its handshake
and an `AgentDiagnostics` request inside a second.

### H2. The bridge does not kill a daemon for being slow

`inspect_existing_daemon` tells three answers apart: same build, a hello
with a different digest (retire, as today), and silence. On silence it
probes again, up to about 10 s in all, before it retires. With H1 a daemon
answers a hello in milliseconds whatever the disk does, so ten silent
seconds mean it is wedged, and retiring it is then the right recovery.

`bridge.log` gets a line whenever a bridge retires a daemon, with the
reason (`different-build` or `silent`). Today a kill leaves no trace, which
is why the handoff could not tell.

Test: a fake daemon that answers the hello after 3 s is connected to, not
retired; one that never answers is retired after the bound.

### H3. Remove the fsyncs (the owner's call, recommended)

`store::persist` and `hook.rs`'s `persist_latest_fallback` keep the
temp-file-and-rename and drop both `sync_all` calls.

Why this loses nothing that matters: rename is already atomic against a
daemon crash, and the page cache outlives the process. The fsyncs add
protection only against power loss or a kernel crash. After either, the
tmux server is gone, and with it every pane and agent the state describes;
`store::load` already starts empty on a file it cannot parse. What is at
risk is a few seconds of lifecycle state for agents that no longer exist.
What it costs today is this incident, plus two fsyncs inside every agent
hook whenever the daemon is slow.

Before doing it, the implementer confirms one thing: nothing in
`StoredState` must survive a reboot (the session history lives in
`sessions.json` and has its own writer). If something must, that one field
moves to a file that is synced, not the whole state on every hook.

With the fsyncs gone the mailbox drain and the duplicate copies of §2.3
cost microseconds, and need no change.

If the owner keeps the fsyncs, the smaller remedy is: after an ingest
acquires `ingest_order`, skip it when its connection has already closed
(the client then wrote the event to the mailbox, where the drain applies
it once, in order).

### H4. Small

- Delete `.hook-fallback-*.tmp` and `.agents-*.tmp` older than an hour at
  daemon start and in the two-second maintenance pass (already on the
  blocking pool).
- `daemon::run` drains the mailbox after it starts accepting, on the
  blocking pool, instead of before. Live ingests already drain first, so
  order holds.

### Wire

None. Desktop, host and phone protocols are untouched.

## 5. The build that set it off

- Agents: one cargo command at a time, `--lib` or `--test <name>`. Now a
  rule in chat-polish-plan.md §1.
- `[profile.dev]` and `[profile.test]`: `debug = "line-tables-only"`. Most
  of each 1.3 GB test binary is debug info; backtraces keep file and line.
  Measure the size before and after on one binary.
- The eleven files in `apps/desktop-gpui/tests/` become one integration
  target (`tests/it/main.rs` with a module per file): one link instead of
  eleven. Separate change; `target/` is 87 GB today.

## 6. Verify

Unit tests as above. Then on a real host, without another 80 GB build:
hold the disk busy with `stress-ng --hdd 4 --hdd-bytes 4G` (or `fio` with
`fsync=1`) for five minutes while two agents work, and check in
`daemon.stderr.log` that there is no `handshake-failed`, no
`orderedRequestTimeout`, hook ingests stay under a second, and the pid in
`daemon.json` does not change.

## 7. State

Implemented on 16 Nov, uncommitted, under `apps/host/` only:

- **H1.** `agent_dispatch::handle` and `history_dispatch::handle` run on the
  blocking pool. Test:
  `service::tests::hook_ingests_stuck_on_the_disk_leave_runtime_workers_free`
  (two workers, three stuck ingests, a fourth connection answered inside a
  second); it fails with the change reverted.
- **H2.** `bridge.rs`: a silent daemon is probed again for 10 s
  (`SILENT_DAEMON_BOUND`) before it is retired; a different build is retired
  at once, as before; `bridge.log` gets a `daemonRetired` line with
  `different-build` or `silent`. Three new bridge tests, two adjusted.
- **H4, first half.** Temp files older than an hour are removed once a
  minute.

Found while doing it, and still open:

| # | What | Why it matters |
| --- | --- | --- |
| R1 | `daemon::run` still drains the mailbox before its first `accept` (H4, second half) | A daemon started under load is silent for as long as the drain takes, and is now retired after 10 s instead of 2 s. H3 makes the drain cheap; moving it behind `accept` makes it harmless. Do one of them. |
| R2 | §4 H1 was wrong about `snapshot.rs`: `discover_authoritative` is on the blocking pool, but `snapshot_from_identity` → `reconcile_topology` is not. It takes `state` and can persist. It runs inline at `service.rs:1157`, `requests.rs:151`, `tmux_action_dispatch.rs:256` and `:349`, and in the topology actor; the terminal-input path (`dispatcher.rs:399`, `:415`) holds `state` too | Each can park one runtime worker behind an ingest's fsync. Bounded by the number of subscribed desktops and phones, so it cannot starve the runtime the way hooks did, but a desktop's typing can wait on a hook's fsync. H3 removes it; otherwise these move to the blocking pool one by one, with care for ordering. |
| R3 | The handshake runs `server_identity()`, a tmux subprocess, on a worker (`service.rs:390`) | "A hello never waits on the disk" is true only while starting tmux is fast. Not measured under a stalled disk. |
| R4 | Every waiting hook now holds a blocking-pool thread (512 at most) | A queue as long as the incident's would make git, files and topology work wait behind hooks. H3 ends the queue; without it, an async semaphore before `spawn_blocking`. |

All four point the same way: H3 is the real fix, and H1 and H2 are what keep
the daemon alive and reachable until it lands.

One behaviour change to know: the ordered lane's 10 s bound now fires for
agent operations. A desktop whose agent request waits longer is
disconnected and reconnects, where before it hung.

## 8. Next, in order

Nothing here has started.

1. **H3**, once the owner agrees: check that nothing in `StoredState` must
   survive a reboot, then drop the `sync_all` calls in `store::persist` and
   `persist_latest_fallback`. This settles R1, R2 and R4.
2. **R1** if H3 is refused: drain the mailbox after `accept`, on the
   blocking pool.
3. **R3**: measure a handshake under a stalled disk (§6); move
   `server_identity()` off the worker only if it shows.
4. **§5**: `debug = "line-tables-only"` and one integration test target,
   as its own change.
5. **§6** on a real host, then release.
