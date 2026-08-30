use std::{
    collections::{HashMap, HashSet},
    io::Write,
    process::{Child, ChildStdin},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc as std_mpsc,
    },
    time::Duration,
};

use anyhow::{Context, bail};
#[cfg(test)]
use tmux_agent_protocol::v1;
#[cfg(test)]
use tmux_control::{CommandTag, ScreenSeeder};
use tmux_control::{
    PaneResourceState as StoredResourceState, PaneResourceStore, REVEAL_TAIL_BOUND,
    VisibilityCheckpoint,
};
use tokio::sync::mpsc;

use super::SequencerControl;
use super::topology_output_trigger::TopologyOutputTrigger;
mod capabilities;
mod degradation;
use degradation::{pane_resource_event, report_pane_degradations};
mod flow_control;
use flow_control::{FlowControl, resume_command, take_injected_rejection};
mod input;
mod input_client;
use input_client::PersistentInputClient;
mod output_credit;
pub(super) use output_credit::{
    OUTPUT_WINDOW_BYTES, OUTPUT_WINDOW_RECORDS, OutputCharge, OutputCredit,
};
mod seed;
#[cfg(test)]
use seed::{build_seed, build_seed_with_cell_captures, parse_capture_metadata};
use seed::{build_seed_with_metadata, capture_metadata};
mod startup;
use startup::{join_workers, stop_process};
mod attachment_startup;
use attachment_startup::AttachmentRuntime;
#[cfg(test)]
use attachment_startup::tmux_supports_control_color_reports;

/// Cells a control client may be resized to on either axis. See
/// [`TerminalAttachment::resize`]; the desktop refuses the same range before it
/// asks (`clientSize.ts`), so a request outside it is a defect on one side or
/// the other and never a user's screen.
const TERMINAL_CLIENT_CELL_BOUNDS: std::ops::RangeInclusive<u32> = 2..=500;

// tmux's OSC report syntax uses four hexadecimal digits per channel. These
// mirror `--term-fg` and `--term-bg` in the desktop's canonical `tokens.css`;
// `control_client_palette_matches_terminal_theme_tokens` guards that boundary.
const TERMINAL_FOREGROUND_OSC_RGB: &str = "ffff/ffff/ffff";
const TERMINAL_BACKGROUND_OSC_RGB: &str = "2828/2c2c/3434";

pub(super) struct TerminalAttachment {
    pane_ids: HashSet<String>,
    stdin: Arc<Mutex<ChildStdin>>,
    child: Arc<Mutex<Child>>,
    stopped: Arc<AtomicBool>,
    stream_tx: std_mpsc::Sender<StreamControl>,
    flow: Arc<FlowControl>,
    /// Panes whose capture has been written to tmux and not yet answered.
    ///
    /// Shared with the reader, which is the only thing that can observe a
    /// capture finishing. A capture in flight *is* this pane's next seed, so a
    /// second request for the same pane buys nothing and costs the wire another
    /// screen — five independent callers ask for one pane's screen on a single
    /// workspace switch. See [`TerminalAttachment::request_seed`].
    capture_in_flight: Arc<Mutex<HashSet<String>>>,
    /// The connection-wide delivery window, held so [`Self::stop`] can wake
    /// this attachment's workers parked in [`OutputCredit::reserve`].
    output_credit: Arc<OutputCredit>,
    workers: Vec<std::thread::JoinHandle<()>>,
    /// The last size tmux was told for *this* client, so it is not told again.
    ///
    /// `refresh-client -C` is not free — the remote-linux lane measured 3 identical
    /// requests costing 15 topology-dirty events on a real link — and the
    /// desktop's own dedupe cannot cover this one, because what re-sends it is
    /// the host carrying a size across to a client that became visible
    /// (M13-E005). A client that has already been told the size still has it:
    /// `ignore-size` decides whether tmux acts on a client's size, not whether
    /// it remembers one. So the carry-across is needed exactly once per client
    /// per size, and a workspace switched away from and back costs nothing.
    last_size: Option<(u32, u32)>,
    /// Whether this tmux exposes the control-mode OSC-report bridge added after
    /// 3.3. Older supported releases keep their existing fallback instead of
    /// receiving a command-line option they do not understand.
    reports_terminal_colors: bool,
}

pub(super) struct VisibilityChange {
    pub(super) visible: bool,
    /// The renderer's own claim about the screen it is holding: on a hide, that
    /// it kept a copy; on a reveal, that it still has the one the checkpoint
    /// names. A reveal without it is answered with a seed, which is what makes
    /// a desktop too old to send it correct rather than blank.
    pub(super) renderer_holds_snapshot: bool,
    pub(super) checkpoint: VisibilityCheckpoint,
}

impl TerminalAttachment {
    /// Resizes the control client, which resizes the *user's* windows.
    ///
    /// `refresh-client -C` is obeyed by tmux for every client that participates
    /// in sizing, and the windows it sizes are shared with whatever plain
    /// terminals are attached to the same session. A desktop that computes a
    /// nonsense size therefore damages real work — P12-U006 asked for ~300 rows
    /// and tmux complied on four of the user's windows. The bound is the blast
    /// radius: no display is 500 cells on an axis, and a rejection is louder and
    /// cheaper than a repair. The rejected size is named in the error the caller
    /// surfaces and in the daemon log, because "resize failed" without a number
    /// cannot be diagnosed after the fact.
    /// Always writes. The desktop asking for a size it has asked for before is
    /// not a repetition to suppress — it is the *only* signal the desktop has
    /// when something else moved the windows out from under it. Its surface has
    /// not changed, so the size it re-asserts is by construction the size the
    /// host last recorded, and a dedupe here would swallow exactly the request
    /// that exists to un-letterbox the pane. [`Self::ensure_size`] is the one
    /// caller that may skip a write, and it is not this one.
    pub(super) fn resize(&mut self, columns: u32, rows: u32) -> anyhow::Result<()> {
        if let Err(error) = check_client_size(columns, rows) {
            crate::diagnostics::record_rejected_client_resize(columns, rows);
            return Err(error);
        }
        {
            let mut stdin = self.stdin.lock().unwrap();
            writeln!(stdin, "refresh-client -C {columns},{rows}")?;
            stdin.flush()?;
        }
        // Recorded only once the write landed, so a failed one is retried
        // rather than remembered as delivered.
        self.last_size = Some((columns, rows));
        Ok(())
    }

    /// Gives a client a size it has never been given, and otherwise does
    /// nothing.
    ///
    /// The visibility handoff's half of sizing: a client taken out of
    /// `ignore-size` having never been sent a `refresh-client -C` sizes its
    /// windows from tmux's 80x24 default (M13-E005), so whoever clears the flag
    /// owes it a size. A client that has already been told one still has it —
    /// `ignore-size` decides whether tmux *acts* on a client's size, not
    /// whether it remembers one — so a workspace switched away from and back
    /// costs nothing. That matters because the desktop re-states the visible
    /// session on every switch and every reconnect, and the remote-linux lane
    /// measured identical `refresh-client -C` requests at 15 topology-dirty
    /// events each on a real link.
    fn ensure_size(&mut self, columns: u32, rows: u32) -> anyhow::Result<()> {
        if self.last_size == Some((columns, rows)) {
            return Ok(());
        }
        self.resize(columns, rows)
    }

    pub(super) fn contains_pane(&self, pane_id: &str) -> bool {
        self.pane_ids.contains(pane_id)
    }

    fn update_panes(
        &mut self,
        pane_ids: &HashSet<String>,
        resources: &mut PaneResourceStore,
    ) -> anyhow::Result<()> {
        let update = {
            let mut stdin = self.stdin.lock().unwrap();
            apply_membership_update(
                &mut self.pane_ids,
                pane_ids,
                &self.stream_tx,
                &mut *stdin,
                &self.capture_in_flight,
                self.reports_terminal_colors,
            )
        };
        let removed = match update {
            Ok(removed) => removed,
            Err(error) => {
                // Reader membership is delivered before the correlated tmux
                // writes and cannot be rolled back atomically with them. End
                // this attachment so the next reconciliation starts a fresh
                // reader and captures every committed pane from scratch.
                self.stop();
                return Err(error.context("terminal membership update stopped the attachment"));
            }
        };
        for pane_id in removed {
            resources.remove(&pane_id);
        }
        Ok(())
    }

    /// Re-photographs a pane, resuming it first if tmux has it paused.
    ///
    /// The resume is the half that was missing. A seed for a paused pane
    /// restores its screen and then delivers nothing further, because tmux
    /// drops a paused pane's output rather than replaying it — which is exactly
    /// what the user reported: switching to another tab and back refreshes the
    /// pane once, and it freezes again.
    ///
    /// A pane whose capture is already in flight is left alone: that capture is
    /// the seed this request would ask for, and photographing the same screen
    /// twice only puts a second copy of it on the wire ahead of the answer the
    /// user is waiting for. The reader clears the entry on *every* exit from
    /// the capture block, a discarded seed included, and [`Self::stop`] clears
    /// the rest, so a pane cannot be held out of a seed it still needs.
    /// `owed_seeds` is unaffected — it is the ledger for a request that could
    /// not be written at all, which is a different failure.
    pub(super) fn request_seed(&mut self, pane_id: &str) -> anyhow::Result<()> {
        validate_tmux_id(pane_id, '%')?;
        if !self.contains_pane(pane_id) {
            bail!("pane is not owned by this session control client");
        }
        if self.capture_in_flight.lock().unwrap().contains(pane_id) {
            return Ok(());
        }
        write_capture_request_resuming(
            &self.stdin,
            &self.capture_in_flight,
            pane_id,
            self.flow.resume_before_capture(pane_id),
        )
    }

    /// Asks tmux for the scrollback above one pane's screen.
    ///
    /// Deliberately not coalesced against `capture_in_flight`. That ledger
    /// means "a seed is already coming for this pane", and it is read to
    /// *suppress* seeds: a history entry in it would silence the photograph a
    /// reveal needs, which is the blank pane the ledger exists to avoid. The
    /// two captures cannot collide either — tmux answers commands in order and
    /// the reader correlates a block by the marker before it, so a history
    /// block and a seed block are strictly sequential however they were
    /// interleaved at the writer.
    ///
    /// The whole command is one line, so the marker and the capture reach tmux
    /// adjacent by construction rather than by holding the lock across two
    /// writes.
    pub(super) fn request_history(
        &mut self,
        pane_id: &str,
        lines: u32,
        skip: u32,
    ) -> anyhow::Result<()> {
        validate_tmux_id(pane_id, '%')?;
        if !self.contains_pane(pane_id) {
            bail!("pane is not owned by this session control client");
        }
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| anyhow::anyhow!("tmux control stdin is poisoned"))?;
        writeln!(stdin, "{}", capture_history_command(pane_id, lines, skip))?;
        stdin.flush()?;
        Ok(())
    }

    /// Takes back the hidden per-window pointer tmux actually sizes from, and
    /// says whether it did.
    ///
    /// Under `window-size latest` a window's size follows `w->latest`, the last
    /// client tmux considers to have *used* that window. A control client can
    /// never become that client by asserting sizes: only an attach, an
    /// interactive keypress or an interactive `MSG_RESIZE` move the pointer, and
    /// the resize path returns early for a control client. So one keystroke in
    /// the user's own terminal pins every window it shows to that terminal's
    /// size, and from then on this daemon's `refresh-client -C` writes are
    /// computed, delivered, and discarded — the pane letterboxes and nothing in
    /// the log says why. `switch-client -E` issued *by* this client is the one
    /// command reachable from a control client that moves the pointer to it and
    /// recomputes the session's current window from the size this client holds.
    ///
    /// Which is exactly why an unsized client must never issue it: a control
    /// client that has not been sent a `refresh-client -C` is still at tmux's
    /// 80x24 default, and claiming on it would recompute the user's real windows
    /// down to 80x24 — worse than the bug this repairs. [`Self::last_size`] is
    /// that guarantee and this refuses without it, rather than trusting each
    /// call site to order its two writes correctly.
    ///
    /// The caller gates *when* this is worth doing. Each claim emits a
    /// `%session-changed` on this client's own control stream, which
    /// `stream_helpers.rs` classifies as a topology notification, so every claim
    /// costs one topology reconcile; a session no foreign client shares has
    /// nothing to reclaim and should not pay it.
    fn claim_latest(&mut self, session_id: &str) -> anyhow::Result<bool> {
        validate_tmux_id(session_id, '$')?;
        if self.last_size.is_none() {
            return Ok(false);
        }
        let mut stdin = self.stdin.lock().unwrap();
        writeln!(stdin, "switch-client -E -t {session_id}")?;
        stdin.flush()?;
        Ok(true)
    }

    fn set_sizing(&mut self, participates: bool) -> anyhow::Result<()> {
        let mut stdin = self.stdin.lock().unwrap();
        let flag = if participates {
            "!ignore-size"
        } else {
            "ignore-size"
        };
        writeln!(stdin, "refresh-client -f {flag}")?;
        stdin.flush()?;
        Ok(())
    }

    pub(super) fn stop(&mut self) {
        stop_process(&self.stopped, &self.child);
        // Nothing this client wrote will be answered now, and the reader that
        // would have cleared these entries is on its way out. A pane must not
        // inherit a capture that died with its attachment: the next client's
        // seed for it has to be written, not coalesced away.
        self.capture_in_flight.lock().unwrap().clear();
        // The store above is not a wakeup. A worker parked in `reserve`
        // re-checks `stopped` only when the shared credit's condvar fires, and
        // without this its join in `Drop` hangs the service thread.
        self.output_credit.wake_waiters();
    }
}

impl Drop for TerminalAttachment {
    fn drop(&mut self) {
        self.stop();
        join_workers(&mut self.workers);
    }
}

fn apply_membership_update(
    current: &mut HashSet<String>,
    desired: &HashSet<String>,
    stream_tx: &std_mpsc::Sender<StreamControl>,
    stdin: &mut impl Write,
    capture_in_flight: &Mutex<HashSet<String>>,
    reports_terminal_colors: bool,
) -> anyhow::Result<Vec<String>> {
    let (added, removed) = membership_delta(current, desired);
    if added.is_empty() && removed.is_empty() {
        return Ok(Vec::new());
    }
    // Every added ID is interpolated into the control stream below. Validate
    // the whole batch before notifying the reader or writing even a marker so
    // a malformed membership update has no partial effects.
    for pane_id in &added {
        validate_tmux_id(pane_id, '%')?;
    }
    send_authoritative_membership(stream_tx, desired)
        .map_err(|_| anyhow::anyhow!("terminal stream coordinator is disconnected"))?;
    let write_result = (|| -> anyhow::Result<()> {
        writeln!(stdin, "display-message -p '__ADE_MEMBERSHIP__'")?;
        for pane_id in &added {
            if reports_terminal_colors {
                write_terminal_color_reports(stdin, pane_id)?;
            }
            queue_capture(stdin, pane_id)?;
        }
        stdin.flush()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        // The reader may already have applied `desired`. Restore the last
        // committed set before returning so a topology that reverts to
        // `current` cannot hit the no-op fast path while the reader stays on
        // the failed membership.
        let _ = send_authoritative_membership(stream_tx, current);
        return Err(error);
    }
    capture_in_flight
        .lock()
        .unwrap()
        .extend(added.iter().cloned());
    // Commit only after the reader notification and every correlated tmux
    // command were accepted. On a partial write the caller can retry the same
    // delta; treating it as an exact no-op would strand panes without seeds.
    current.clone_from(desired);
    Ok(removed)
}

/// Gives tmux the palette its control client cannot discover from a TTY.
///
/// tmux consumes an application's OSC 10/11 query before pane output reaches
/// xterm. A `-C` client has no terminal of its own, so without these documented
/// `refresh-client -r` reports tmux answers black; Codex then derives its
/// `rgb(30,30,30)` composer from that false background. These values mirror the
/// fixed terminal palette in `apps/desktop/src/features/terminal/theme.ts`.
///
/// The reports must be separate commands: tmux accepts `-r` once per
/// `refresh-client` invocation, so repeating the option retains only the last
/// report.
fn write_terminal_color_reports(stdin: &mut impl Write, pane_id: &str) -> std::io::Result<()> {
    writeln!(
        stdin,
        "refresh-client -r '{pane_id}:\x1b]10;rgb:{TERMINAL_FOREGROUND_OSC_RGB}\x1b\\'"
    )?;
    writeln!(
        stdin,
        "refresh-client -r '{pane_id}:\x1b]11;rgb:{TERMINAL_BACKGROUND_OSC_RGB}\x1b\\'"
    )
}

fn send_authoritative_membership(
    stream_tx: &std_mpsc::Sender<StreamControl>,
    pane_ids: &HashSet<String>,
) -> Result<(), std_mpsc::SendError<StreamControl>> {
    let mut pane_ids: Vec<_> = pane_ids.iter().cloned().collect();
    pane_ids.sort();
    stream_tx.send(StreamControl::Membership { pane_ids })
}

fn membership_delta(
    current: &HashSet<String>,
    desired: &HashSet<String>,
) -> (Vec<String>, Vec<String>) {
    let mut added: Vec<_> = desired.difference(current).cloned().collect();
    let mut removed: Vec<_> = current.difference(desired).cloned().collect();
    added.sort();
    removed.sort();
    (added, removed)
}

pub(super) struct TerminalClients {
    clients: HashMap<String, TerminalAttachment>,
    visible_session: Option<String>,
    /// The last size the desktop asked for, kept so the *next* client to
    /// become visible can be told it.
    ///
    /// M13-E005: every control client is attached with `ignore-size`, and only
    /// the visible one is taken out of it. A session the user selects later
    /// therefore starts participating in sizing having never been sent a
    /// `refresh-client -C`, so tmux sized its windows from the 80x24 default and
    /// the agent inside them rendered into a quarter of the surface. The desktop
    /// could not correct it either: its surface had not moved, so it had no new
    /// size to send. Whoever clears `ignore-size` owns giving that client a
    /// size, and that is this type.
    last_size: Option<(u32, u32)>,
    /// How many clients tmux said were attached to each session, from the last
    /// topology snapshot this connection reconciled.
    ///
    /// The one input to [`Self::foreign_client_shares`], and the reason
    /// [`TerminalAttachment::claim_latest`] stays off the common path. It is
    /// read from the snapshot the daemon already discovers rather than from a
    /// probe of its own: a gate that cost a tmux fork to answer would cost more
    /// than the claim it is protecting against. Empty until the first
    /// reconciliation, which reads as "no foreign client" and is the
    /// conservative direction — a missed claim letterboxes a pane until the next
    /// selection, an unwarranted one churns topology for every connection.
    session_attached: HashMap<String, u32>,
    resources: Arc<Mutex<PaneResourceStore>>,
    generation: Arc<AtomicU64>,
    input: Option<PersistentInputClient>,
    /// The session `input` is attached to, so the gate can tell this daemon's
    /// own second client on that session apart from a foreign one.
    ///
    /// INVARIANT: `Some` exactly while `input` holds a started sidecar. Every
    /// place that takes `input` clears this in the same step, because the gate
    /// reads it as "tmux is counting a client of ours on that session" and a
    /// stale name would hide a real foreign client for as long as it survived.
    input_session: Option<String>,
    /// Panes the host owes a seed and could not ask tmux for.
    ///
    /// A seed request fails when the pane's session control client is not
    /// attached — the ordinary state during a reconnect, and precisely when a
    /// reveal is most likely to be asked for. The reveal event has already been
    /// sent by then, so the desktop is waiting for a screen that nobody is
    /// going to capture, and nothing retried. Recorded here and re-attempted
    /// the moment an attachment for that pane exists again; see
    /// [`TerminalClients::settle_owed_seeds`].
    owed_seeds: HashSet<String>,
    output_credit: Arc<OutputCredit>,
    /// Serializes the resource transition that decides whether bytes are
    /// visible with admission of the resulting ordered terminal event. It is
    /// deliberately separate from `resources`: credit/channel waits hold this
    /// fence, never the pane-resource store lock.
    emission_order: Arc<Mutex<()>>,
    /// Handed to every attachment's reader thread so delivered pane output can
    /// wake topology reconciliation; see
    /// [`crate::service::topology_output_trigger`].
    topology_trigger: TopologyOutputTrigger,
}

impl TerminalClients {
    pub(super) fn new(
        output_credit: Arc<OutputCredit>,
        topology_trigger: TopologyOutputTrigger,
    ) -> Self {
        Self {
            topology_trigger,
            clients: HashMap::new(),
            visible_session: None,
            last_size: None,
            session_attached: HashMap::new(),
            input_session: None,
            // A hidden pane costs its tail and nothing else now, so the whole
            // hidden-pane store is 512 KB instead of 16 MB.
            resources: Arc::new(Mutex::new(PaneResourceStore::with_total_limit(
                32,
                REVEAL_TAIL_BOUND,
                32 * REVEAL_TAIL_BOUND,
            ))),
            generation: Arc::new(AtomicU64::new(0)),
            input: None,
            owed_seeds: HashSet::new(),
            output_credit,
            emission_order: Arc::new(Mutex::new(())),
        }
    }

    pub(super) fn attach(
        &mut self,
        session_id: &str,
        pane_ids: &[String],
        make_visible: bool,
        event_tx: mpsc::Sender<SequencerControl>,
        overflowed: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let desired: HashSet<_> = pane_ids.iter().cloned().collect();
        let committed = self
            .clients
            .get(session_id)
            .filter(|client| !client.stopped.load(Ordering::Acquire))
            .map(|client| client.pane_ids.clone());
        let speculative_added: Vec<_> = committed
            .as_ref()
            .map(|current| desired.difference(current).cloned().collect())
            .unwrap_or_default();
        let newly_created: Vec<_>;
        {
            let generation = self.generation.load(Ordering::Acquire);
            let mut resources = self.resources.lock().unwrap();
            newly_created = pane_ids
                .iter()
                .filter(|pane_id| resources.get(pane_id).is_none())
                .cloned()
                .collect();
            register_mounted_panes(&mut resources, pane_ids, make_visible, generation);
        }
        // Mounting panes can push the store past its global budget, which
        // evicts some other pane's recovery material. Reported here, with the
        // store's lock released, for the same reason the store records instead
        // of emitting.
        report_pane_degradations(&self.resources, &event_tx, &overflowed);
        if committed.is_some() {
            self.ensure_input_client(session_id, &event_tx, &overflowed)?;
            let update = self
                .clients
                .get_mut(session_id)
                .expect("committed attachment disappeared under exclusive access")
                .update_panes(&desired, &mut self.resources.lock().unwrap());
            if let Err(error) = update {
                remove_pane_resources(
                    &mut self.resources.lock().unwrap(),
                    speculative_added.iter(),
                );
                return Err(error);
            }
            if make_visible {
                self.select_session(session_id)?;
            }
            self.settle_owed_seeds();
            return Ok(());
        }
        let mut attachment = match TerminalAttachment::start(
            session_id,
            pane_ids,
            AttachmentRuntime {
                event_tx: event_tx.clone(),
                overflowed: Arc::clone(&overflowed),
                resources: Arc::clone(&self.resources),
                terminal_generation: Arc::clone(&self.generation),
                output_credit: Arc::clone(&self.output_credit),
                emission_order: Arc::clone(&self.emission_order),
                topology_trigger: self.topology_trigger.clone(),
            },
        ) {
            Ok(attachment) => attachment,
            Err(error) => {
                remove_pane_resources(&mut self.resources.lock().unwrap(), newly_created.iter());
                return Err(error);
            }
        };
        if let Err(error) = self.ensure_input_client(session_id, &event_tx, &overflowed) {
            attachment.stop();
            remove_pane_resources(&mut self.resources.lock().unwrap(), newly_created.iter());
            return Err(error);
        }
        if let Some(old) = self.clients.get_mut(session_id) {
            // Stop establishes the reader's ownership fence. Every resource
            // mutation and its derived event publication hold this same lock,
            // so a buffered old record either publishes before replacement or
            // is rejected after stop; it cannot outlive the cleanup.
            let mut resources = self.resources.lock().unwrap();
            old.stop();
            remove_unmounted_pane_resources(&mut resources, &old.pane_ids, &desired);
        }
        self.clients.insert(session_id.to_owned(), attachment);
        if make_visible {
            self.select_session(session_id)?;
        } else if self.visible_session.as_deref() == Some(session_id) {
            // A fresh control client for the session that is already visible —
            // a reconnect, or one whose tmux process died. It re-arms sizing,
            // so it needs the size for the same reason a newly selected one
            // does: it is a client tmux now listens to, and it has been told
            // nothing.
            self.size_visible_client(session_id)?;
        }
        // A fresh control client is exactly what a pane owed a seed was waiting
        // for. Its own startup already queues one capture per mounted pane, so
        // this costs a write only for a pane whose debt outlived that.
        self.settle_owed_seeds();
        Ok(())
    }

    /// Re-attempts every seed the host owes a pane whose control client exists
    /// again.
    ///
    /// The retry is anchored to attachment rather than to a timer because that
    /// is what the failure is about: a seed request fails when there is no
    /// session control client to write it to, and no amount of waiting on the
    /// writer thread produces one. A pane whose resource has since been
    /// unmounted is forgotten rather than retried forever.
    fn settle_owed_seeds(&mut self) {
        if self.owed_seeds.is_empty() {
            return;
        }
        let mut owed: Vec<_> = self.owed_seeds.iter().cloned().collect();
        owed.sort();
        for pane_id in owed {
            if self.resources.lock().unwrap().get(&pane_id).is_none() {
                self.owed_seeds.remove(&pane_id);
                continue;
            }
            let attached = self.clients.values().any(|client| {
                client.contains_pane(&pane_id) && !client.stopped.load(Ordering::Acquire)
            });
            if !attached {
                continue;
            }
            crate::diagnostics::record_seed_request_retry();
            if self.request_seed(&pane_id).is_ok() {
                self.owed_seeds.remove(&pane_id);
            }
        }
    }

    /// Records that a pane is still owed a seed after a request for it failed.
    fn owe_seed(&mut self, pane_id: &str) {
        self.owed_seeds.insert(pane_id.to_owned());
    }

    fn ensure_input_client(
        &mut self,
        session_id: &str,
        event_tx: &mpsc::Sender<SequencerControl>,
        overflowed: &Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        if self
            .input
            .as_ref()
            .is_some_and(PersistentInputClient::is_ready)
        {
            return Ok(());
        }
        if let Some(mut failed) = self.input.take() {
            failed.stop();
            self.input_session = None;
        }
        self.input = Some(PersistentInputClient::start(
            session_id,
            event_tx.clone(),
            Arc::clone(overflowed),
        )?);
        // Recorded only once the sidecar exists, so a failed start cannot make
        // the gate believe this daemon owns a client tmux never counted.
        self.input_session = Some(session_id.to_owned());
        Ok(())
    }

    /// Whether a client this daemon did not attach may be constraining
    /// `session_id`'s windows.
    ///
    /// tmux counts every attached client in `session_attached`, this daemon's
    /// included, so the question is arithmetic: the session's total against the
    /// clients this connection holds on it — its session control client, plus
    /// the input sidecar when that is the session it attached to. Anything above
    /// that is somebody else, and somebody else is who can own `w->latest`; see
    /// [`TerminalAttachment::claim_latest`].
    ///
    /// A second desktop connection counts as foreign here, because from this
    /// connection's `TerminalClients` it is indistinguishable from a plain
    /// terminal. That errs toward claiming, which is the side that costs a
    /// topology reconcile rather than a letterboxed pane.
    fn foreign_client_shares(&self, session_id: &str) -> bool {
        let Some(&attached) = self.session_attached.get(session_id) else {
            return false;
        };
        let control = u32::from(self.clients.contains_key(session_id));
        let input = u32::from(self.input_session.as_deref() == Some(session_id));
        attached > control + input
    }

    /// Makes `session_id`'s client the one tmux sizes from, and tells it what
    /// size that is.
    ///
    /// One function because they are one act. The flag is what makes tmux
    /// listen and the size is what it then hears; a caller that could do the
    /// first without the second is how M13-E005 happened.
    ///
    /// `visible_session` moves the instant the flag is set, before the size is
    /// sent, so a failed *resize* leaves a state that is merely wrong by one
    /// message rather than incoherent: the client that participates in sizing
    /// and the one this type believes is visible are the same client, and the
    /// next `resize` reaches it. Recording it only after the size would leave
    /// every later resize addressed to a client tmux is ignoring, and reported
    /// as success.
    ///
    /// A failed *flag*, though, is the opposite case and gets the opposite
    /// treatment: the caller has already taken the previous client out of
    /// sizing, so nobody participates, and naming a client that is in
    /// `ignore-size` would make every later `resize` a write tmux discards and
    /// this type reports as delivered. `visible_session` is cleared instead, so
    /// `resize` says "no visible session control client" until a selection
    /// lands — which the desktop re-attempts on every topology generation.
    fn size_visible_client(&mut self, session_id: &str) -> anyhow::Result<()> {
        let last_size = self.last_size;
        let previous = self.visible_session.clone();
        let claim = self.foreign_client_shares(session_id);
        let outcome = (|| -> anyhow::Result<()> {
            // Nobody participates until the flag lands. Cleared first rather
            // than on each failure path, so every way out of the two lines
            // below leaves the same coherent state.
            self.visible_session = None;
            let client = self
                .clients
                .get_mut(session_id)
                .context("selected session control client is detached")?;
            client.set_sizing(true)?;
            self.visible_session = Some(session_id.to_owned());
            let Some((columns, rows)) = last_size else {
                return Ok(());
            };
            let client = self
                .clients
                .get_mut(session_id)
                .context("selected session control client is detached")?;
            client.ensure_size(columns, rows)?;
            // Size first, claim second, and only for a session somebody else is
            // in: the claim recomputes the windows from the size this client
            // holds, so it is only ever correct once that size has landed.
            if claim && client.claim_latest(session_id)? {
                crate::diagnostics::write_sizing_latest_claim_log(session_id);
            }
            Ok(())
        })();
        // Both writes go to a pipe, and a pipe write tmux ignores still
        // succeeds, so this is the only record that the handoff happened at all.
        crate::diagnostics::write_terminal_sizing_handoff_log(
            previous.as_deref(),
            session_id,
            last_size,
            outcome.as_ref().err().map(ToString::to_string).as_deref(),
        );
        outcome
    }

    pub(super) fn send_input(&mut self, pane_id: &str, data: &[u8]) -> anyhow::Result<()> {
        self.clients
            .values_mut()
            .find(|client| client.contains_pane(pane_id))
            .context("pane has no attached session control client")?;
        self.input
            .as_mut()
            .context("persistent terminal input client is not attached")?
            .send_input(pane_id, data)
    }

    pub(super) fn flush_input(&mut self) -> anyhow::Result<()> {
        match self.input.as_ref() {
            Some(input) => input.fence(),
            // Before the first CreateSession there are no accepted pane
            // inputs to fence and no session to which a sidecar could attach.
            // This exact empty topology is the bootstrap identity; an absent
            // sidecar with any attached output client remains fail-closed.
            None if self.clients.is_empty() => Ok(()),
            None => bail!("persistent terminal input client is not attached"),
        }
    }

    pub(super) fn resize(&mut self, columns: u32, rows: u32) -> anyhow::Result<()> {
        let session_id = self
            .visible_session
            .as_deref()
            .context("no visible session control client")?
            .to_owned();
        let claim = self.foreign_client_shares(&session_id);
        self.clients
            .get_mut(&session_id)
            .context("visible session control client is detached")?
            .resize(columns, rows)?;
        // Remembered only once tmux has actually been told, so a refused or
        // failed size is never replayed onto the next client as if it were the
        // surface's real geometry.
        self.last_size = Some((columns, rows));
        // A resize is a size the user's own terminal never asked for, so it is
        // the other moment tmux has to be told which client the windows follow.
        // After `last_size` rather than before it: the size itself did land, and
        // a claim that fails must not also cost the next visible client the
        // geometry this one is already showing.
        if claim
            && self
                .clients
                .get_mut(&session_id)
                .context("visible session control client is detached")?
                .claim_latest(&session_id)?
        {
            crate::diagnostics::write_sizing_latest_claim_log(&session_id);
        }
        Ok(())
    }

    /// Names the session the desktop is showing.
    ///
    /// Called on every workspace switch *and* on every (re)connect; the desktop
    /// side owns why, in `useVisibleTerminalSession.ts`. Idempotent by
    /// construction — selecting the session that is already selected re-asserts
    /// the flag, which is what a caller that cannot know whether the client
    /// survived actually wants, and `TerminalAttachment::last_size` is what
    /// keeps that from costing a redundant `refresh-client -C`.
    pub(super) fn select_session(&mut self, session_id: &str) -> anyhow::Result<()> {
        validate_tmux_id(session_id, '$')?;
        if !self.clients.contains_key(session_id) {
            crate::diagnostics::write_terminal_sizing_handoff_log(
                self.visible_session.as_deref(),
                session_id,
                self.last_size,
                Some("session has no control client"),
            );
            bail!("session has no control client");
        }
        let previous_id = self.visible_session.clone();
        if let Some(previous) = previous_id.as_deref()
            && previous != session_id
            && let Some(client) = self.clients.get_mut(previous)
        {
            // Yielding first, because two clients out of `ignore-size` at once
            // means tmux sizes the windows from whichever spoke last, which is
            // the state this whole mechanism exists to never be in.
            //
            // A client that cannot be written to has already stopped
            // participating in anything — its tmux process is gone — so the
            // switch proceeds. Failing it instead would leave `visible_session`
            // naming that dead client, which is where every later resize would
            // then be addressed: a workspace switch blocked by the workspace
            // being switched *away* from. Logged, because a yield that did not
            // happen is exactly the kind of thing the handoff log exists for.
            if let Err(error) = client.set_sizing(false) {
                crate::diagnostics::write_terminal_sizing_handoff_log(
                    previous_id.as_deref(),
                    session_id,
                    self.last_size,
                    Some(&format!("previous client could not yield sizing: {error}")),
                );
            }
        }
        self.size_visible_client(session_id)
    }

    pub(super) fn request_seed(&mut self, pane_id: &str) -> anyhow::Result<()> {
        self.clients
            .values_mut()
            .find(|client| client.contains_pane(pane_id))
            .context("pane has no attached session control client")?
            .request_seed(pane_id)
    }

    pub(super) fn request_history(
        &mut self,
        pane_id: &str,
        lines: u32,
        skip: u32,
    ) -> anyhow::Result<()> {
        self.clients
            .values_mut()
            .find(|client| client.contains_pane(pane_id))
            .context("pane has no attached session control client")?
            .request_history(pane_id, lines, skip)
    }

    /// Seeds a pane because the renderer explicitly asked for its screen.
    ///
    /// The request is itself the statement of visibility — the desktop asks
    /// only for panes it is drawing — so the pane's resource is forced back to
    /// renderer ownership first. Without that, a resource left hidden or
    /// released (by a handoff the desktop has since forgotten, or by an
    /// eviction it was never told about) stores the completed capture and never
    /// emits it: `stream.rs` gates seed emission on `!is_hidden`, so the
    /// request produces nothing at all, and asking again produces nothing
    /// again. That is one of the ways a pane freezes for the rest of a session.
    pub(super) fn request_seed_for_render(
        &mut self,
        pane_id: &str,
        sender: &mpsc::Sender<SequencerControl>,
        overflowed: &AtomicBool,
    ) -> anyhow::Result<()> {
        validate_tmux_id(pane_id, '%')?;
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        // Deliberately not under the emission-order fence. That fence exists so
        // output cannot observe a new visibility before the *event* carrying
        // its recovery material is admitted, and this transition emits no such
        // event: the recovery material is the seed, which is captured later and
        // published through the fence by the reader like any other seed.
        let forced = {
            let mut resources = self.resources.lock().unwrap();
            resources.reveal_for_seed_request(pane_id, generation)
        };
        if forced {
            crate::diagnostics::record_seed_forced_reveal();
        }
        report_pane_degradations(&self.resources, sender, overflowed);
        let requested = self.request_seed(pane_id);
        if requested.is_err() {
            self.owe_seed(pane_id);
        }
        requested
    }

    pub(super) fn set_visibility(
        &mut self,
        pane_id: &str,
        change: VisibilityChange,
        event_permit: mpsc::OwnedPermit<SequencerControl>,
        sender: &mpsc::Sender<SequencerControl>,
        overflowed: &AtomicBool,
    ) -> anyhow::Result<()> {
        let VisibilityChange {
            visible,
            renderer_holds_snapshot,
            checkpoint,
        } = change;
        // Whichever operation owns this fence performs both its resource
        // transition and ordered event admission before a later visible output
        // may observe the new state. Nothing under it waits on a peer: the
        // PaneResourceStore is taken and released inside, and the delivery
        // window is charged without waiting (see below), so the fence is held
        // only for the transition and the queue admission it orders.
        let emission_order = Arc::clone(&self.emission_order);
        let _emission = emission_order.lock().unwrap();
        validate_tmux_id(pane_id, '%')?;
        let stopped = match self
            .clients
            .values()
            .find(|client| client.contains_pane(pane_id))
        {
            Some(client) => Arc::clone(&client.stopped),
            None => bail!("pane has no attached session control client"),
        };
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let (mut resource, requires_seed) = {
            let mut resources = self.resources.lock().unwrap();
            let resource = if visible {
                // The checkpoint is passed only when the renderer says it is
                // still holding that screen; the store answers anything else
                // with a seed, because a tail written onto a screen nobody
                // verified is a defect nothing later repairs.
                resources
                    .reveal(
                        pane_id,
                        generation,
                        renderer_holds_snapshot.then_some(checkpoint),
                    )
                    .context("pane resource missing")?
            } else {
                resources
                    .hide_with_checkpoint(pane_id, checkpoint, generation)
                    .map_err(anyhow::Error::msg)?
            };
            let requires_seed =
                resource.state == StoredResourceState::Released || resource.requires_seed;
            (resource, requires_seed)
        };
        if visible {
            resource.state = StoredResourceState::Visible;
        } else {
            // A hide answers with no bytes at all. The store keeps the tail —
            // this is a clone of it — because the *reveal* is what hands it
            // back, and it is the only side that can. Shipping it here as well
            // paid for the same output twice on the way out of a workspace the
            // user has already left: the renderer ignores a `hiddenBuffered`
            // echo, so those bytes were never drawn, and a second hide against
            // the same checkpoint returned them a third time.
            resource.raw_tail.clear();
        }
        // The tail is the whole payload: the host stores no screen to charge
        // for, a hide carries nothing, and a reveal it cannot verify carries no
        // bytes at all.
        let charge = OutputCharge::terminal(resource.raw_tail.len());
        // Charged, never waited for. This runs as a blocking task that the
        // connection's ordered-operation lane awaits. Waiting for credit here
        // would wait on an acknowledgement consumed by that connection's frame
        // reader, while holding both the terminal mutex and the emission fence:
        // the closed cycle captured in production. The control readers repay
        // any excess this admits by waiting before their next emission.
        let reservation = match self.output_credit.admit(charge, &stopped) {
            Ok(reservation) => reservation,
            Err(error) => {
                self.resources.lock().unwrap().require_seed(
                    pane_id,
                    "visibility recovery could not be admitted for ordered delivery",
                );
                return Err(anyhow::Error::msg(error));
            }
        };
        let event = pane_resource_event(pane_id, resource, charge);
        // The async request lane reserved this slot before taking either the
        // terminal mutex or this emission fence. Sending through the permit is
        // immediate, cannot wait on the socket writer, and preserves the
        // recovery-before-visible-output ordering the fence exists to enforce.
        event_permit.send(SequencerControl::OrderedEvent(event));
        reservation.commit();
        drop(_emission);
        // Both the hide and the reveal path can push the store past its global
        // budget and evict some *other* pane. Reported after the fence, holding
        // neither the store nor the emission order.
        report_pane_degradations(&self.resources, sender, overflowed);
        if visible
            && requires_seed
            && let Err(error) = self.request_seed(pane_id)
        {
            // The recovery event is already on the wire, so the desktop is now
            // waiting for a seed. Failing here without recording that debt is
            // what left the pane waiting forever: the usual cause is a session
            // control client that has not re-attached yet, and the attachment
            // that replaces it is what settles this.
            self.owe_seed(pane_id);
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn reconcile(&mut self, snapshot: &tmux_control::TmuxSnapshot) {
        // Replaced wholesale from the same snapshot that decides which
        // attachments survive, so the gate can never answer from a count
        // belonging to a session that is gone.
        self.session_attached = snapshot
            .sessions
            .iter()
            .map(|item| (item.id.clone(), item.attached_clients))
            .collect();
        let sessions: HashSet<_> = snapshot
            .sessions
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        let stale: Vec<_> = self
            .clients
            .keys()
            .filter(|id| !sessions.contains(id.as_str()))
            .cloned()
            .collect();
        for session_id in stale {
            if let Some(mut client) = self.clients.remove(&session_id) {
                client.stop();
            }
            if self.visible_session.as_deref() == Some(&session_id) {
                self.visible_session = None;
            }
        }
        if self.clients.is_empty()
            && let Some(mut input) = self.input.take()
        {
            input.stop();
            self.input_session = None;
        }
    }

    /// Stops every attachment but keeps their worker threads unjoined.
    ///
    /// A reader parked in the ordered-event channel only unblocks once the
    /// connection's sequencer receiver is gone, so the joins — which live in
    /// the returned value's drop — must run after the writer task is torn
    /// down, and never under the mutex this method is called through.
    pub(super) fn signal_stop(&mut self) -> TerminalTeardown {
        self.output_credit.close();
        // Nothing this connection owed a pane survives it: the next connection
        // reseeds every pane it mounts.
        self.owed_seeds.clear();
        let mut input = self.input.take();
        if let Some(input) = input.as_mut() {
            input.stop();
        }
        self.input_session = None;
        for client in self.clients.values_mut() {
            client.stop();
        }
        TerminalTeardown {
            _clients: std::mem::take(&mut self.clients),
            _input: input,
        }
    }

    pub(super) fn stop(&mut self) {
        drop(self.signal_stop());
    }
}

/// The joinable remains of a stopped connection's terminal clients.
///
/// Every child process is already dead and every credit waiter woken when this
/// value exists; dropping it joins the worker threads through the attachments'
/// own `Drop` impls.
pub(super) struct TerminalTeardown {
    _clients: HashMap<String, TerminalAttachment>,
    _input: Option<PersistentInputClient>,
}

impl Drop for TerminalClients {
    fn drop(&mut self) {
        // This is a last-resort ownership guarantee for connection tasks that
        // are cancelled while a background topology pass is winding down.
        self.stop();
    }
}

fn register_mounted_panes(
    resources: &mut PaneResourceStore,
    pane_ids: &[String],
    make_visible: bool,
    generation: u64,
) {
    for pane_id in pane_ids {
        if make_visible {
            resources.set_visible(pane_id, true, generation);
        } else {
            resources.ensure(pane_id, false, generation);
        }
    }
}

fn remove_pane_resources<'a>(
    resources: &mut PaneResourceStore,
    pane_ids: impl IntoIterator<Item = &'a String>,
) {
    for pane_id in pane_ids {
        resources.remove(pane_id);
    }
}

fn remove_unmounted_pane_resources(
    resources: &mut PaneResourceStore,
    previous: &HashSet<String>,
    desired: &HashSet<String>,
) {
    remove_pane_resources(resources, previous.difference(desired));
}

/// Queues a screen capture for one pane.
///
/// INVARIANT: the `__ADE_CAPTURE__` marker and the `capture-pane` command that
/// follows it must reach tmux with nothing written between them. The reader
/// correlates a capture block with a pane purely by "the block after the marker
/// block", so an interleaved write — an in-band keystroke shares this same
/// stdin — would attribute the capture to the wrong pane and corrupt the seed.
/// Every caller therefore writes both lines under a single lock hold; use
/// [`write_capture_request_resuming`] rather than taking the lock yourself when
/// only one pane is being captured. `input.rs` upholds the same rule for its
/// own marker,
/// and `capture_marker_and_capture_command_are_never_split_by_concurrent_input`
/// proves it under concurrency.
///
/// The marker is untargeted, for the same reason `queue_input`'s is: a marker
/// targeted at a pane that has just vanished fails, and a failed marker block
/// carries no pane, so its `%error` would be attributed to the whole connection
/// and resnapshot every pane in every workspace. The pane the capture belongs to
/// is still verified authoritatively — the `__ADE_META__` line inside the
/// capture carries tmux's own `#{pane_id}` and `capture_metadata` refuses a
/// capture whose metadata names a different pane.
///
/// The pane is recorded in `capture_in_flight` by the caller, and only once the
/// write has been *flushed*: the ledger's one job is to suppress a second seed
/// for a pane a photograph is already coming for, so an entry for a capture
/// tmux never received would coalesce away the request that replaces it and
/// leave the pane waiting for a screen nobody is taking. Recording late risks
/// one redundant photograph; recording early risks none at all.
fn queue_capture(stdin: &mut impl Write, pane_id: &str) -> anyhow::Result<()> {
    validate_tmux_id(pane_id, '%')?;
    writeln!(stdin, "{}", queue_marker("__ADE_CAPTURE__", pane_id))?;
    writeln!(stdin, "{}", capture_command(pane_id))?;
    Ok(())
}

/// One write the control-stream reader needs performed on its behalf.
///
/// The reader must never write to tmux's stdin itself: tmux stops reading its
/// stdin while it is blocked writing output to us, and the reader is the only
/// thing draining that output, so a write from the reader can deadlock the pair
/// and silence the pane permanently.
pub(super) struct ControlWrite {
    pub(super) pane_id: String,
    /// Whether tmux paused this pane and is waiting to be told to continue.
    pub(super) resume_first: bool,
    /// How long to wait before writing.
    ///
    /// Only a retried resume sets this, and it is the difference between a
    /// second attempt and the same attempt twice: an immediate identical
    /// rewrite can only clear a rejection that was already over, which is a
    /// narrower class than "transient". A rejection worth one retry is one
    /// where something in flight has to finish first.
    ///
    /// Delaying here rather than on the reader is the whole reason this lane
    /// exists — the reader must never block, because it is the only thing
    /// draining tmux's output and tmux stops reading its stdin while blocked
    /// writing to us. This thread may block; the cost is that a capture queued
    /// behind a retry waits for it, which is bounded and rare.
    pub(super) delay: Option<Duration>,
}

/// Writes one pane's capture marker and capture command under a single lock
/// hold, upholding [`queue_capture`]'s adjacency invariant, and resuming a pane
/// tmux paused first. The resume and the capture share the lock hold so no
/// other writer can land between them.
fn write_capture_request_resuming<W: Write>(
    stdin: &Arc<Mutex<W>>,
    capture_in_flight: &Mutex<HashSet<String>>,
    pane_id: &str,
    resume_first: bool,
) -> anyhow::Result<()> {
    validate_tmux_id(pane_id, '%')?;
    let mut writer = stdin
        .lock()
        .map_err(|_| anyhow::anyhow!("tmux control stdin is poisoned"))?;
    if resume_first {
        // The marker names the pane this resume belongs to, so a rejected
        // resume resnapshots one pane instead of the whole connection.
        writeln!(writer, "{}", queue_marker("__ADE_RESUME__", pane_id))?;
        writeln!(
            writer,
            "{}",
            resume_command(pane_id, take_injected_rejection())
        )?;
    }
    queue_capture(&mut *writer, pane_id)?;
    writer.flush()?;
    capture_in_flight.lock().unwrap().insert(pane_id.to_owned());
    Ok(())
}

/// The correlation marker written ahead of a command whose `%error` has to be
/// attributed to one pane: an in-band `send-keys`, a flow-control resume, or a
/// capture. It is deliberately untargeted so it cannot fail when the pane has
/// vanished — the marker's job is to name the pane whose command block is about
/// to fail, which requires the marker itself to always succeed.
///
/// The pane's `%` sigil is stripped rather than written literally. tmux runs a
/// display message through `strftime`, which silently swallows `%0` as an
/// unknown conversion — the marker looked correct and matched nothing. The
/// reader restores the sigil.
fn queue_marker(kind: &str, pane_id: &str) -> String {
    let digits = pane_id.strip_prefix('%').unwrap_or(pane_id);
    format!("display-message -p '{kind}:{digits}'")
}

/// The marker an in-band input request writes ahead of its `send-keys`.
pub(super) fn queue_input(input_id: u64, pane_id: &str) -> String {
    let digits = pane_id.strip_prefix('%').unwrap_or(pane_id);
    format!("display-message -p '__ADE_INPUT__:{input_id}:{digits}'")
}

/// Photographs one pane's *screen*.
///
/// No history range: the first capture takes what tmux is displaying and
/// nothing above it. A 200x50 screen is ~10 KB where `-S -2000` was ~191 KB,
/// and on a slow link that difference is the answer to a workspace switch
/// arriving behind a quarter of a megabyte of scrollback the user cannot see.
/// The scrollback is not lost — it is still in tmux, and it is fetched on
/// demand rather than pushed on every reveal.
fn capture_command(pane_id: &str) -> String {
    format!(
        "capture-pane -p -e -J -t {pane_id} ; capture-pane -p -e -N -t {pane_id} ; capture-pane -p -e -J -a -q -t {pane_id} ; capture-pane -p -e -N -a -q -t {pane_id} ; display-message -p -t {pane_id} '__ADE_META__:#{{pane_id}}:#{{cursor_x}}:#{{cursor_y}}:#{{alternate_on}}:#{{bracket_paste_flag}}:#{{mouse_standard_flag}}:#{{mouse_button_flag}}:#{{mouse_any_flag}}:#{{mouse_sgr_flag}}:#{{mouse_utf8_flag}}:#{{cursor_flag}}:#{{keypad_cursor_flag}}:#{{keypad_flag}}:#{{wrap_flag}}:#{{pane_width}}:#{{focus_flag}}'"
    )
}

/// The most scrollback one request may photograph.
///
/// The renderer's own serialization keeps 10,000 lines, so anything above this
/// could not be spliced in anyway, and the number bounds a capture the user is
/// waiting on: it is written straight onto the control lane ahead of their next
/// keystroke.
const MAX_HISTORY_LINES: u32 = 10_000;

/// The furthest above the display one request may start.
///
/// The renderer reports how much scrollback it is already holding, and a number
/// larger than any buffer it could have is not a claim this host acts on: an
/// unclamped one would put both bounds arbitrarily far past the top of tmux's
/// history, and the capture command carrying them is written straight onto the
/// control lane. The renderer mirrors this number and stops paging on it, which
/// is what keeps a clamp from becoming a pane that asks for the same clamped
/// rows on every wheel-up.
const MAX_HISTORY_SKIP_LINES: u32 = MAX_HISTORY_LINES;

/// Photographs the scrollback *above* one pane's screen.
///
/// The counterpart to [`capture_command`], which takes the screen and nothing
/// above it. Both bounds count rows upwards from the display, `-1` being the
/// row immediately above it, so `-S -(skip+lines) -E -(skip+1)` is exactly the
/// `lines` rows above the `skip` the renderer already holds. The two ranges
/// meet exactly once: no row appears in both and none is missing between them.
///
/// `skip` is what makes this true of a pane that has printed since it was
/// seeded. tmux measures from the *current* display, so the rows that scrolled
/// off in the meantime are above it — already in the renderer's own scrollback,
/// and handed back a second time by an unskipped capture. The renderer counts
/// what it holds and says so; the host does not guess.
///
/// The marker carries the line count as well as the pane, so the request is
/// legible in a tmux log beside the answer it produced; the reader needs only
/// the pane. There is no `__ADE_META__` leg — this is not a screen, and nothing
/// in the answer may be mistaken for a seed the reader has to store.
///
/// No `-J`, and that is the whole reason this command is not the seed's.
///
/// `-J` joins each wrapped line back into one output line, which makes the
/// answer's line count say nothing about how many *rows* it covers — a measured
/// 60 rows of wrapped output came back as 20 lines. Every other number in this
/// protocol is a row: `-S`/`-E` count rows, `skip` is the rows the renderer
/// holds, `#{history_size}` is rows. A page whose own unit is lines cannot be
/// lined up against any of them, and the renderer trimming a row-sized overlap
/// off a line-sized page removes up to three times too much.
///
/// Without `-J` every captured line is one physical row at the width the pane
/// had, which is the width the renderer had too — so the page, the skip and the
/// size are all in the same unit and the arithmetic closes.
///
/// The old objection to dropping it was that scrollback spliced in hard-broken
/// never reflows and copies back as separate lines. That is answered on the
/// desktop rather than here: it composes the page so that a row filling the
/// grid is written *without* a line break, xterm auto-wraps it, and the
/// continuation carries xterm's own `isWrapped` — the same reflow and the same
/// copy as a line the pane printed live. See `composeHistoryPage`.
///
/// The trailing `__ADE_HISTORY_META__` leg is how the answer says whether it
/// reached the top. The rows it carries cannot say: tmux clamps a range that
/// runs past the top of its history and answers one entirely above it with a
/// single row rather than with nothing, so an emptier answer is not the signal.
/// `#{history_size}` is the fact itself, and the renderer compares it against
/// the rows it asked for.
///
/// It is a third command, and targeted, for the same reason the seed's
/// `__ADE_META__` leg is: a pane-scoped format can only be expanded by a
/// targeted `display-message`, and the leading marker has to stay untargeted so
/// that it always succeeds. A targeted probe against a pane that has gone away
/// is rejected — the reader answers that page without a size, and the renderer
/// asks again.
fn capture_history_command(pane_id: &str, lines: u32, skip: u32) -> String {
    let lines = lines.clamp(1, MAX_HISTORY_LINES);
    let skip = skip.min(MAX_HISTORY_SKIP_LINES);
    let start = skip.saturating_add(lines);
    let end = skip.saturating_add(1);
    let digits = pane_id.strip_prefix('%').unwrap_or(pane_id);
    format!(
        "display-message -p '__ADE_HISTORY__:{lines}:{digits}' ; capture-pane -p -e -S -{start} -E -{end} -t {pane_id} ; display-message -p -t {pane_id} '__ADE_HISTORY_META__:#{{history_size}}'"
    )
}

mod correlation;
mod stream;
#[cfg(test)]
use stream::TestOutputEmission;
#[cfg(test)]
use stream::{
    CommandBlock, PaneSeedState, PendingAlternateCapture, PendingCaptureMetadata,
    PendingSavedNormalCells, StreamState,
};
use stream::{ControlStreamReader, StreamControl, read_control_stream};

pub(super) fn validate_tmux_id(value: &str, prefix: char) -> anyhow::Result<()> {
    if value.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        Ok(())
    } else {
        bail!("invalid tmux identifier: {value}")
    }
}

/// Rejects a client size outside [`TERMINAL_CLIENT_CELL_BOUNDS`], naming the
/// size in the error. Pure: the caller records and logs the rejection, so this
/// stays a predicate and the test that exercises it has no side effects.
fn check_client_size(columns: u32, rows: u32) -> anyhow::Result<()> {
    if TERMINAL_CLIENT_CELL_BOUNDS.contains(&columns) && TERMINAL_CLIENT_CELL_BOUNDS.contains(&rows)
    {
        return Ok(());
    }
    bail!(
        "refusing a {columns}x{rows} tmux client size: terminal dimensions must be between {} and {} cells",
        TERMINAL_CLIENT_CELL_BOUNDS.start(),
        TERMINAL_CLIENT_CELL_BOUNDS.end()
    )
}

#[cfg(test)]
#[path = "terminal_tests.rs"]
mod tests;
