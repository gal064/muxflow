use std::{
    collections::{HashMap, HashSet},
    io::Write,
    process::{Child, ChildStdin, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc as std_mpsc,
    },
    time::Duration,
};

use anyhow::{Context, bail};
use tmux_agent_protocol::v1;
#[cfg(test)]
use tmux_control::{CommandTag, ScreenSeeder};
use tmux_control::{
    MAX_INPUT_REQUEST_BYTES, PaneResourceState as StoredResourceState, PaneResourceStore,
    VisibilityCheckpoint,
};
use tokio::sync::mpsc;

use super::snapshot::tmux_command;
use super::{SequencerControl, TERMINAL_INPUT_QUEUE, emit_event};

mod flow_control;
use flow_control::{FlowControl, resume_command, take_injected_rejection};
mod input;
use input::{InputDispatch, run_input_dispatch};
mod seed;
#[cfg(test)]
use seed::{build_seed, parse_capture_metadata};
use seed::{build_seed_with_metadata, capture_metadata};

/// Cells a control client may be resized to on either axis. See
/// [`TerminalAttachment::resize`]; the desktop refuses the same range before it
/// asks (`clientSize.ts`), so a request outside it is a defect on one side or
/// the other and never a user's screen.
const TERMINAL_CLIENT_CELL_BOUNDS: std::ops::RangeInclusive<u32> = 2..=500;

pub(super) struct TerminalAttachment {
    pane_ids: HashSet<String>,
    stdin: Arc<Mutex<ChildStdin>>,
    child: Arc<Mutex<Child>>,
    stopped: Arc<AtomicBool>,
    input_tx: std_mpsc::SyncSender<InputDispatch>,
    stream_tx: std_mpsc::Sender<StreamControl>,
    flow: Arc<FlowControl>,
    /// The last size tmux was told for *this* client, so it is not told again.
    ///
    /// `refresh-client -C` is not free — the omarchy lane measured 3 identical
    /// requests costing 15 topology-dirty events on a real link — and the
    /// desktop's own dedupe cannot cover this one, because what re-sends it is
    /// the host carrying a size across to a client that became visible
    /// (M13-E005). A client that has already been told the size still has it:
    /// `ignore-size` decides whether tmux acts on a client's size, not whether
    /// it remembers one. So the carry-across is needed exactly once per client
    /// per size, and a workspace switched away from and back costs nothing.
    last_size: Option<(u32, u32)>,
}

pub(super) struct VisibilityChange {
    pub(super) visible: bool,
    pub(super) serialized_snapshot: Vec<u8>,
    pub(super) checkpoint: VisibilityCheckpoint,
}

impl TerminalAttachment {
    pub(super) fn start(
        session_id: &str,
        pane_ids: &[String],
        event_tx: mpsc::Sender<SequencerControl>,
        overflowed: Arc<AtomicBool>,
        resources: Arc<Mutex<PaneResourceStore>>,
        terminal_generation: Arc<AtomicU64>,
    ) -> anyhow::Result<Self> {
        validate_tmux_id(session_id, '$')?;
        if pane_ids.is_empty() {
            bail!("cannot attach without panes");
        }
        for pane_id in pane_ids {
            validate_tmux_id(pane_id, '%')?;
        }

        let mut child = tmux_command()
            .args([
                // `-CC` asks tmux to disable terminal echo and performs a
                // tcgetattr probe on current tmux releases; a daemon pipe has
                // no controlling TTY. `-C` is the byte-identical control
                // protocol mode that works over local and SSH stdio.
                "-C",
                "attach-session",
                "-f",
                "pause-after=5,ignore-size",
                "-t",
                session_id,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start tmux control client")?;
        let stdout = child
            .stdout
            .take()
            .context("tmux control stdout unavailable")?;
        let stdin = child
            .stdin
            .take()
            .context("tmux control stdin unavailable")?;
        let stdin = Arc::new(Mutex::new(stdin));
        {
            let mut writer = stdin.lock().unwrap();
            for pane_id in pane_ids {
                queue_capture(&mut *writer, pane_id)?;
            }
            writer.flush()?;
        }

        let stopped = Arc::new(AtomicBool::new(false));
        let (input_tx, input_rx) = std_mpsc::sync_channel(TERMINAL_INPUT_QUEUE);
        let (input_completion_tx, input_completion_rx) = std_mpsc::channel();
        let input_stdin = Arc::clone(&stdin);
        // Input admission is fire-and-forget from the desktop. A later action
        // barrier receives the first write failure, while the event stream
        // immediately requests recovery because the user's screen no longer
        // shows what they believe they typed.
        let failure_tx = event_tx.clone();
        let failure_overflowed = Arc::clone(&overflowed);
        std::thread::Builder::new()
            .name(format!("host-tmux-input-{session_id}"))
            .spawn(move || {
                run_input_dispatch(
                    input_rx,
                    input_stdin,
                    input_completion_rx,
                    |pane_id, error| {
                        emit_event(
                            &failure_tx,
                            &failure_overflowed,
                            v1::HostEvent {
                                kind: v1::EventKind::TerminalResnapshotRequired.into(),
                                scope: pane_id.to_owned(),
                                detail: format!("terminal input was not written: {error}"),
                                ..Default::default()
                            },
                        );
                    },
                )
            })?;
        let (stream_tx, stream_rx) = std_mpsc::channel();
        let reader_stopped = Arc::clone(&stopped);
        let reader_stop_signal = Arc::clone(&stopped);
        let reader_panes = pane_ids.to_vec();
        let flow = Arc::new(FlowControl::default());
        let reader_flow = Arc::clone(&flow);
        let reader_writer = spawn_control_writer(session_id, Arc::clone(&stdin))?;
        std::thread::Builder::new()
            .name(format!("host-tmux-control-{session_id}"))
            .spawn(move || {
                read_control_stream(ControlStreamReader {
                    stdout,
                    writer: reader_writer,
                    pane_ids: reader_panes,
                    event_tx,
                    overflowed,
                    resources,
                    terminal_generation,
                    stopped: reader_stop_signal,
                    controls: stream_rx,
                    flow: reader_flow,
                    input_completion: input_completion_tx,
                });
                reader_stopped.store(true, Ordering::Release);
            })?;
        Ok(Self {
            pane_ids: pane_ids.iter().cloned().collect(),
            stdin,
            child: Arc::new(Mutex::new(child)),
            stopped,
            input_tx,
            stream_tx,
            flow,
            last_size: None,
        })
    }

    /// Queues one input request. Returning `Ok` means the bytes are ordered
    /// behind everything already queued for this client, not that tmux has
    /// accepted them; the input barrier taken before every tmux action and
    /// resize is the point at which that becomes true.
    pub(super) fn send_input(&mut self, pane_id: &str, data: &[u8]) -> anyhow::Result<()> {
        validate_tmux_id(pane_id, '%')?;
        if self.stopped.load(Ordering::Acquire) {
            bail!("terminal control stream is disconnected");
        }
        if data.len() > MAX_INPUT_REQUEST_BYTES {
            bail!("terminal input request exceeds the 1 MiB atomic commit limit");
        }
        if data.is_empty() {
            return Ok(());
        }
        self.input_tx
            .try_send(InputDispatch::Bytes {
                pane_id: pane_id.to_owned(),
                data: data.to_vec(),
            })
            .map_err(|error| match error {
                std_mpsc::TrySendError::Full(_) => {
                    crate::diagnostics::record_terminal_input_backpressure();
                    anyhow::anyhow!(
                        "terminal input queue is full; caller must retry instead of dropping bytes"
                    )
                }
                std_mpsc::TrySendError::Disconnected(_) => {
                    anyhow::anyhow!("terminal input dispatcher is disconnected")
                }
            })?;
        Ok(())
    }

    fn flush_input(&mut self) -> anyhow::Result<()> {
        let (sender, receiver) = std_mpsc::sync_channel(1);
        self.input_tx
            .send(InputDispatch::Barrier(sender))
            .map_err(|_| anyhow::anyhow!("terminal input dispatcher is disconnected"))?;
        receiver
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| anyhow::anyhow!("terminal input flush timed out"))?
            .map_err(anyhow::Error::msg)
    }

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
    /// session on every switch and every reconnect, and the omarchy lane
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
            apply_membership_update(&mut self.pane_ids, pane_ids, &self.stream_tx, &mut *stdin)
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
    pub(super) fn request_seed(&mut self, pane_id: &str) -> anyhow::Result<()> {
        validate_tmux_id(pane_id, '%')?;
        if !self.contains_pane(pane_id) {
            bail!("pane is not owned by this session control client");
        }
        write_capture_request_resuming(
            &self.stdin,
            pane_id,
            self.flow.resume_before_capture(pane_id),
        )
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
        self.stopped.store(true, Ordering::Release);
        let _ = self.input_tx.try_send(InputDispatch::Stop);
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn apply_membership_update(
    current: &mut HashSet<String>,
    desired: &HashSet<String>,
    stream_tx: &std_mpsc::Sender<StreamControl>,
    stdin: &mut impl Write,
) -> anyhow::Result<Vec<String>> {
    let (added, removed) = membership_delta(current, desired);
    if added.is_empty() && removed.is_empty() {
        return Ok(Vec::new());
    }
    send_authoritative_membership(stream_tx, desired)
        .map_err(|_| anyhow::anyhow!("terminal stream coordinator is disconnected"))?;
    let write_result = (|| -> anyhow::Result<()> {
        writeln!(stdin, "display-message -p '__ADE_MEMBERSHIP__'")?;
        for pane_id in &added {
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
    // Commit only after the reader notification and every correlated tmux
    // command were accepted. On a partial write the caller can retry the same
    // delta; treating it as an exact no-op would strand panes without seeds.
    current.clone_from(desired);
    Ok(removed)
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
    resources: Arc<Mutex<PaneResourceStore>>,
    generation: Arc<AtomicU64>,
}

impl TerminalClients {
    pub(super) fn new() -> Self {
        Self {
            clients: HashMap::new(),
            visible_session: None,
            last_size: None,
            resources: Arc::new(Mutex::new(PaneResourceStore::with_total_limit(
                32,
                4 * 1024 * 1024,
                16 * 1024 * 1024,
            ))),
            generation: Arc::new(AtomicU64::new(0)),
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
        if committed.is_some() {
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
                for pane_id in pane_ids {
                    self.request_seed(pane_id)?;
                }
            }
            return Ok(());
        }
        let attachment = match TerminalAttachment::start(
            session_id,
            pane_ids,
            event_tx,
            overflowed,
            Arc::clone(&self.resources),
            Arc::clone(&self.generation),
        ) {
            Ok(attachment) => attachment,
            Err(error) => {
                remove_pane_resources(&mut self.resources.lock().unwrap(), newly_created.iter());
                return Err(error);
            }
        };
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
        Ok(())
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
            self.clients
                .get_mut(session_id)
                .context("selected session control client is detached")?
                .ensure_size(columns, rows)
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
            .context("pane has no attached session control client")?
            .send_input(pane_id, data)
    }

    pub(super) fn flush_input(&mut self) -> anyhow::Result<()> {
        for client in self.clients.values_mut() {
            client.flush_input()?;
        }
        Ok(())
    }

    pub(super) fn resize(&mut self, columns: u32, rows: u32) -> anyhow::Result<()> {
        let session_id = self
            .visible_session
            .as_deref()
            .context("no visible session control client")?;
        self.clients
            .get_mut(session_id)
            .context("visible session control client is detached")?
            .resize(columns, rows)?;
        // Remembered only once tmux has actually been told, so a refused or
        // failed size is never replayed onto the next client as if it were the
        // surface's real geometry.
        self.last_size = Some((columns, rows));
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

    pub(super) fn set_visibility(
        &mut self,
        pane_id: &str,
        change: VisibilityChange,
        sender: &mpsc::Sender<SequencerControl>,
        overflowed: &AtomicBool,
    ) -> anyhow::Result<()> {
        let VisibilityChange {
            visible,
            serialized_snapshot,
            checkpoint,
        } = change;
        validate_tmux_id(pane_id, '%')?;
        if !self
            .clients
            .values()
            .any(|client| client.contains_pane(pane_id))
        {
            bail!("pane has no attached session control client");
        }
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        let (mut resource, requires_seed) = {
            let mut resources = self.resources.lock().unwrap();
            let resource = if visible {
                resources
                    .reveal(pane_id, generation)
                    .context("pane resource missing")?
            } else {
                resources
                    .hide_with_checkpoint(pane_id, serialized_snapshot, checkpoint, generation)
                    .map_err(anyhow::Error::msg)?
            };
            let requires_seed =
                resource.state == StoredResourceState::Released || resource.requires_seed;
            (resource, requires_seed)
        };
        if visible {
            resource.state = StoredResourceState::Visible;
        }
        emit_event(
            sender,
            overflowed,
            v1::HostEvent {
                kind: v1::EventKind::PaneResource.into(),
                scope: pane_id.into(),
                pane_resource: Some(v1::PaneResource {
                    pane_id: pane_id.into(),
                    state: match resource.state {
                        StoredResourceState::Visible => v1::PaneResourceState::Visible.into(),
                        StoredResourceState::HiddenBuffered => {
                            v1::PaneResourceState::HiddenBuffered.into()
                        }
                        StoredResourceState::Released => v1::PaneResourceState::Released.into(),
                    },
                    serialized_snapshot: resource.serialized_snapshot,
                    raw_tail: resource.raw_tail,
                    generation: resource.generation,
                    snapshot_generation: resource.snapshot_generation,
                    tail_through_generation: resource.tail_through_generation,
                    requires_seed: resource.requires_seed,
                    recovery_reason: resource.recovery_reason,
                }),
                ..Default::default()
            },
        );
        if visible && requires_seed {
            self.request_seed(pane_id)?;
        }
        Ok(())
    }

    pub(super) fn reconcile(&mut self, snapshot: &tmux_control::TmuxSnapshot) {
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
    }

    pub(super) fn stop(&mut self) {
        for client in self.clients.values_mut() {
            client.stop();
        }
        self.clients.clear();
    }
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

/// Serialises reader-requested writes onto a thread that is allowed to block.
///
/// This is deliberately its own unbounded lane rather than a slot on the input
/// queue. A `refresh-client -A <pane>:continue` is the only thing that ever
/// resumes a pane tmux paused for flow control, so dropping one stalls that
/// pane forever — it cannot share a bound with keystrokes, whose backpressure
/// policy is to refuse. Unbounded is safe because every producer is already
/// rate-limited: tmux pauses a pane at most once per flow-control episode, a
/// discarded seed needs another few megabytes of output before it can recur,
/// and a parse error resnapshots once.
pub(super) fn spawn_control_writer(
    session_id: &str,
    stdin: Arc<Mutex<ChildStdin>>,
) -> anyhow::Result<std_mpsc::Sender<ControlWrite>> {
    let (sender, receiver) = std_mpsc::channel::<ControlWrite>();
    std::thread::Builder::new()
        .name(format!("host-tmux-writer-{session_id}"))
        .spawn(move || {
            while let Ok(write) = receiver.recv() {
                if let Some(delay) = write.delay {
                    std::thread::sleep(delay);
                }
                let _ = write_capture_request_resuming(&stdin, &write.pane_id, write.resume_first);
            }
        })?;
    Ok(sender)
}

/// Writes one pane's capture marker and capture command under a single lock
/// hold, upholding [`queue_capture`]'s adjacency invariant, and resuming a pane
/// tmux paused first. The resume and the capture share the lock hold so no
/// other writer can land between them.
fn write_capture_request_resuming<W: Write>(
    stdin: &Arc<Mutex<W>>,
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
pub(super) fn queue_input(pane_id: &str) -> String {
    queue_marker("__ADE_INPUT__", pane_id)
}

fn capture_command(pane_id: &str) -> String {
    format!(
        "capture-pane -p -e -J -S -2000 -t {pane_id} ; capture-pane -p -e -J -a -q -t {pane_id} ; display-message -p -t {pane_id} '__ADE_META__:#{{pane_id}}:#{{cursor_x}}:#{{cursor_y}}:#{{alternate_on}}:#{{bracket_paste_flag}}:#{{mouse_standard_flag}}:#{{mouse_button_flag}}:#{{mouse_any_flag}}:#{{mouse_sgr_flag}}:#{{mouse_utf8_flag}}:#{{cursor_flag}}:#{{keypad_cursor_flag}}:#{{keypad_flag}}:#{{wrap_flag}}:#{{pane_width}}:#{{focus_flag}}'"
    )
}

mod correlation;
mod stream;
#[cfg(test)]
use stream::{CommandBlock, PaneSeedState, PendingCaptureMetadata, StreamState};
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
