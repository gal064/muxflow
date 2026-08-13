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

mod input;
use input::{InputDispatch, run_input_dispatch};
mod seed;
#[cfg(test)]
use seed::{build_seed, parse_capture_metadata};
use seed::{build_seed_with_metadata, capture_metadata, selected_capture_boundary};

pub(super) struct TerminalAttachment {
    pane_ids: HashSet<String>,
    stdin: Arc<Mutex<ChildStdin>>,
    child: Arc<Mutex<Child>>,
    stopped: Arc<AtomicBool>,
    input_tx: std_mpsc::SyncSender<InputDispatch>,
    stream_tx: std_mpsc::Sender<StreamControl>,
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
        let input_stdin = Arc::clone(&stdin);
        // Input is fire-and-forget from the desktop, so a write that fails here
        // has no caller left to tell. Report it on the event stream instead:
        // the user's keystrokes did not reach the pane, and its screen no
        // longer shows what they believe they typed.
        let failure_tx = event_tx.clone();
        let failure_overflowed = Arc::clone(&overflowed);
        std::thread::Builder::new()
            .name(format!("host-tmux-input-{session_id}"))
            .spawn(move || {
                run_input_dispatch(input_rx, input_stdin, |pane_id, error| {
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
                })
            })?;
        let (stream_tx, stream_rx) = std_mpsc::channel();
        let reader_stopped = Arc::clone(&stopped);
        let reader_stop_signal = Arc::clone(&stopped);
        let reader_panes = pane_ids.to_vec();
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
        let (completion_tx, _completion_rx) = std_mpsc::sync_channel(1);
        self.input_tx
            .try_send(InputDispatch::Bytes {
                pane_id: pane_id.to_owned(),
                data: data.to_vec(),
                completion: completion_tx,
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

    pub(super) fn resize(&mut self, columns: u32, rows: u32) -> anyhow::Result<()> {
        if !(2..=1000).contains(&columns) || !(2..=1000).contains(&rows) {
            bail!("terminal dimensions must be between 2 and 1000 cells");
        }
        let mut stdin = self.stdin.lock().unwrap();
        writeln!(stdin, "refresh-client -C {columns},{rows}")?;
        stdin.flush()?;
        Ok(())
    }

    pub(super) fn contains_pane(&self, pane_id: &str) -> bool {
        self.pane_ids.contains(pane_id)
    }

    fn update_panes(
        &mut self,
        pane_ids: &HashSet<String>,
        resources: &mut PaneResourceStore,
    ) -> anyhow::Result<()> {
        let (added, removed) = membership_delta(&self.pane_ids, pane_ids);
        self.pane_ids.clone_from(pane_ids);
        self.stream_tx
            .send(StreamControl::Membership {
                added: added.clone(),
                removed: removed.clone(),
            })
            .map_err(|_| anyhow::anyhow!("terminal stream coordinator is disconnected"))?;
        {
            let mut stdin = self.stdin.lock().unwrap();
            writeln!(stdin, "display-message -p '__ADE_MEMBERSHIP__'")?;
            stdin.flush()?;
        }
        if !added.is_empty() {
            let mut stdin = self.stdin.lock().unwrap();
            for pane_id in &added {
                queue_capture(&mut *stdin, pane_id)?;
            }
            stdin.flush()?;
        }
        for pane_id in removed {
            resources.remove(&pane_id);
        }
        Ok(())
    }

    pub(super) fn request_seed(&mut self, pane_id: &str) -> anyhow::Result<()> {
        validate_tmux_id(pane_id, '%')?;
        if !self.contains_pane(pane_id) {
            bail!("pane is not owned by this session control client");
        }
        write_capture_request(&self.stdin, pane_id)
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
    resources: Arc<Mutex<PaneResourceStore>>,
    generation: Arc<AtomicU64>,
}

impl TerminalClients {
    pub(super) fn new() -> Self {
        Self {
            clients: HashMap::new(),
            visible_session: None,
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
        {
            let generation = self.generation.load(Ordering::Acquire);
            let mut resources = self.resources.lock().unwrap();
            register_mounted_panes(&mut resources, pane_ids, make_visible, generation);
        }
        if let Some(current) = self.clients.get_mut(session_id)
            && !current.stopped.load(Ordering::Acquire)
        {
            current.update_panes(&desired, &mut self.resources.lock().unwrap())?;
            if make_visible {
                self.select_session(session_id)?;
                for pane_id in pane_ids {
                    self.request_seed(pane_id)?;
                }
            }
            return Ok(());
        }
        let attachment = TerminalAttachment::start(
            session_id,
            pane_ids,
            event_tx,
            overflowed,
            Arc::clone(&self.resources),
            Arc::clone(&self.generation),
        )?;
        if let Some(mut old) = self.clients.insert(session_id.to_owned(), attachment) {
            old.stop();
        }
        if make_visible {
            self.select_session(session_id)?;
        } else if self.visible_session.as_deref() == Some(session_id)
            && let Some(client) = self.clients.get_mut(session_id)
        {
            client.set_sizing(true)?;
        }
        Ok(())
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
            .resize(columns, rows)
    }

    pub(super) fn select_session(&mut self, session_id: &str) -> anyhow::Result<()> {
        validate_tmux_id(session_id, '$')?;
        if !self.clients.contains_key(session_id) {
            bail!("session has no control client");
        }
        let previous_id = self.visible_session.clone();
        if let Some(previous) = previous_id.as_deref()
            && previous != session_id
            && let Some(client) = self.clients.get_mut(previous)
        {
            client.set_sizing(false)?;
        }
        self.clients
            .get_mut(session_id)
            .context("selected session control client is detached")?
            .set_sizing(true)?;
        self.visible_session = Some(session_id.to_owned());
        Ok(())
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
        resources.ensure(pane_id, make_visible, generation);
        if make_visible {
            resources.set_visible(pane_id, true, generation);
        }
    }
}

/// Queues a screen capture for one pane.
///
/// INVARIANT: the `__ADE_CAPTURE__` marker and the `capture-pane` command that
/// follows it must reach tmux with nothing written between them. The reader
/// correlates a capture block with a pane purely by "the block after the marker
/// block", so an interleaved write — an in-band keystroke shares this same
/// stdin — would attribute the capture to the wrong pane and corrupt the seed.
/// Every caller therefore writes both lines under a single lock hold; use
/// [`write_capture_request`] rather than taking the lock yourself when only one
/// pane is being captured. `input.rs` upholds the same rule for its own marker,
/// and `capture_marker_and_capture_command_are_never_split_by_concurrent_input`
/// proves it under concurrency.
fn queue_capture(stdin: &mut impl Write, pane_id: &str) -> anyhow::Result<()> {
    validate_tmux_id(pane_id, '%')?;
    writeln!(
        stdin,
        "display-message -p -t {pane_id} '__ADE_CAPTURE__:#{{pane_id}}'"
    )?;
    writeln!(stdin, "{}", capture_command(pane_id))?;
    Ok(())
}

/// Writes one pane's capture marker and capture command under a single lock
/// hold, upholding [`queue_capture`]'s adjacency invariant.
fn write_capture_request<W: Write>(stdin: &Arc<Mutex<W>>, pane_id: &str) -> anyhow::Result<()> {
    write_capture_request_resuming(stdin, pane_id, false)
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
                let _ = write_capture_request_resuming(&stdin, &write.pane_id, write.resume_first);
            }
        })?;
    Ok(sender)
}

/// As [`write_capture_request`], optionally resuming a pane tmux paused first.
/// The resume and the capture share the lock hold so no other writer can land
/// between them.
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
        writeln!(writer, "refresh-client -A {pane_id}:continue")?;
    }
    queue_capture(&mut *writer, pane_id)?;
    writer.flush()?;
    Ok(())
}

/// The correlation marker an in-band input request writes ahead of its
/// `send-keys`. It is deliberately untargeted so it cannot fail when the pane
/// has vanished: the marker's job is to name the pane whose command block is
/// about to fail, which requires the marker itself to always succeed.
///
/// The pane's `%` sigil is stripped rather than written literally. tmux runs a
/// display message through `strftime`, which silently swallows `%0` as an
/// unknown conversion — the marker looked correct and matched nothing. The
/// reader restores the sigil.
pub(super) fn queue_input(pane_id: &str) -> String {
    let digits = pane_id.strip_prefix('%').unwrap_or(pane_id);
    format!("display-message -p '__ADE_INPUT__:{digits}'")
}

fn capture_command(pane_id: &str) -> String {
    format!(
        "capture-pane -p -e -J -S -2000 -t {pane_id} ; capture-pane -p -e -J -a -q -t {pane_id} ; display-message -p -t {pane_id} '__ADE_META__:#{{pane_id}}:#{{cursor_x}}:#{{cursor_y}}:#{{alternate_on}}:#{{bracket_paste_flag}}:#{{mouse_standard_flag}}:#{{mouse_button_flag}}:#{{mouse_any_flag}}:#{{mouse_sgr_flag}}:#{{mouse_utf8_flag}}:#{{cursor_flag}}:#{{keypad_cursor_flag}}:#{{keypad_flag}}:#{{wrap_flag}}:#{{pane_width}}:#{{focus_flag}}'"
    )
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_changes_are_in_place_and_deterministic() {
        let current = HashSet::from(["%3".into(), "%1".into()]);
        let desired = HashSet::from(["%2".into(), "%3".into()]);
        assert_eq!(
            membership_delta(&current, &desired),
            (vec!["%2".into()], vec!["%1".into()])
        );
    }

    #[test]
    fn mounting_one_pane_does_not_reveal_inactive_window_resources() {
        let mut resources = PaneResourceStore::with_total_limit(32, 1024, 4096);
        resources.ensure("%1", false, 1);
        resources.ensure("%2", false, 1);
        register_mounted_panes(&mut resources, &["%1".into()], true, 2);
        assert!(!resources.is_hidden("%1"));
        assert!(resources.is_hidden("%2"));
    }

    #[test]
    fn seed_restores_every_tmux_exposed_terminal_mode() {
        let seed = build_seed(
            "%1",
            vec![b"primary history".to_vec()],
            vec![b"alternate screen".to_vec()],
            &[b"__ADE_META__:%1:4:5:1:1:0:1:0:1:0:0:1:1:0:80:".to_vec()],
        )
        .unwrap();
        for expected in [
            b"\x1b[?1049h".as_slice(),
            b"\x1b[?2004h",
            b"\x1b[?1002h",
            b"\x1b[?1006h",
            b"\x1b[?1004l",
            b"\x1b[?25l",
            b"\x1b[?1h",
            b"\x1b=",
            b"\x1b[?7l",
            b"\x1b[6;5H",
        ] {
            assert!(
                seed.bytes
                    .windows(expected.len())
                    .any(|window| window == expected),
                "missing mode sequence {expected:?}"
            );
        }
    }

    #[test]
    fn incomplete_or_unknown_capture_metadata_requires_resnapshot() {
        assert!(build_seed("%1", vec![b"screen".to_vec()], vec![], &[]).is_none());
        assert!(
            build_seed(
                "%1",
                vec![],
                vec![],
                &[b"__ADE_META__:%1:0:0:0:0:0:0:0:0:0:0:0:0:unknown:80:".to_vec()]
            )
            .is_none()
        );
        let tmux_33_seed = build_seed(
            "%11",
            vec![],
            vec![],
            &[b"__ADE_META__:%11:0:0:0::0:0:0:0:0:1:0:0:1:80:".to_vec()],
        )
        .expect("tmux 3.3a's unavailable bracket-paste flag should use a safe default");
        assert!(
            tmux_33_seed
                .bytes
                .windows(b"\x1b[?2004l".len())
                .any(|window| window == b"\x1b[?2004l")
        );
        assert!(
            tmux_33_seed
                .diagnostics
                .iter()
                .any(|value| value.contains("bracketed-paste"))
        );
        assert!(
            tmux_33_seed
                .diagnostics
                .iter()
                .any(|value| value.contains("focus-reporting"))
        );
        assert!(
            parse_capture_metadata(b"__ADE_META__:%11:0:0:0:0:0:0:0:0:0:1:0:0:1:80:", "%11")
                .is_some()
        );
        assert!(
            parse_capture_metadata(
                b"__ADE_META__:       %11:0:0:0:0:0:0:0:0:0:1:0:0:1:80:",
                "%11"
            )
            .is_none()
        );
    }

    #[test]
    fn reconnect_marks_every_pane_pending_for_a_fresh_seed() {
        let pane_ids: Vec<_> = (0..33).map(|index| format!("%{index}")).collect();
        let state = StreamState::new(&pane_ids);
        assert_eq!(state.pane_states.len(), 33);
        assert!(
            state
                .pane_states
                .values()
                .all(|state| matches!(state, PaneSeedState::Pending { .. }))
        );
    }

    #[test]
    fn capture_and_metadata_are_correlated_across_distinct_tmux_command_blocks() {
        let mut state = StreamState::new(&["%1".into()]);
        state.expected_capture = Some("%1".into());
        let tag = |number| CommandTag {
            timestamp: 1,
            number,
            flags: 1,
        };
        let CommandBlock::CapturePrimary { pane_id, .. } = state.start_block(tag(2)) else {
            panic!("capture-pane block was not correlated with its marker");
        };
        if let Some(PaneSeedState::Pending {
            buffered,
            buffered_bytes,
            ..
        }) = state.pane_states.get_mut("%1")
        {
            buffered.push((1, b"already captured".to_vec()));
            *buffered_bytes = b"already captured".len();
        }
        state.pending_alternate = Some((pane_id, vec![b"captured primary".to_vec()], 1));
        let PaneSeedState::Pending { buffered, .. } = state.pane_states.get("%1").unwrap() else {
            panic!("pane stopped awaiting its seed");
        };
        assert_eq!(buffered.len(), 1);
        let CommandBlock::CaptureAlternate {
            pane_id,
            primary_lines,
            primary_boundary,
            ..
        } = state.start_block(tag(3))
        else {
            panic!("alternate block was not correlated with primary capture");
        };
        state.pending_metadata = Some(PendingCaptureMetadata {
            pane_id: pane_id.clone(),
            primary_lines: primary_lines.clone(),
            alternate_lines: vec![b"captured alternate".to_vec()],
            primary_boundary,
            alternate_boundary: 2,
        });
        let CommandBlock::CaptureMetadata {
            alternate_lines, ..
        } = state.start_block(tag(4))
        else {
            panic!("metadata block was not correlated with both screen captures");
        };
        let seed = build_seed(
            &pane_id,
            primary_lines,
            alternate_lines,
            &[b"__ADE_META__:%1:0:0:1:1:0:0:0:0:0:1:0:0:1:80:".to_vec()],
        )
        .expect("metadata should complete seed");
        assert!(
            seed.bytes
                .windows(b"captured primary".len())
                .any(|window| window == b"captured primary")
        );
        assert!(
            seed.bytes
                .windows(b"captured alternate".len())
                .any(|window| window == b"captured alternate")
        );
    }

    #[test]
    fn capture_boundary_tracks_the_active_screen_without_duplicate_replay() {
        let primary =
            parse_capture_metadata(b"__ADE_META__:%1:0:0:0:0:0:0:0:0:0:1:0:0:1:80:", "%1").unwrap();
        let alternate =
            parse_capture_metadata(b"__ADE_META__:%1:0:0:1:0:0:0:0:0:0:1:0:0:1:80:", "%1").unwrap();
        assert_eq!(selected_capture_boundary(primary, 10, 12), 10);
        assert_eq!(selected_capture_boundary(alternate, 10, 12), 12);

        let mut seeder = ScreenSeeder::default();
        seeder.buffer(10, b"in primary capture".to_vec());
        seeder.buffer(11, b"between captures".to_vec());
        seeder.buffer(13, b"after capture".to_vec());
        let replay = seeder.complete(b"seed".to_vec(), 12).replay;
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].sequence, 13);
        assert_eq!(replay[0].bytes, b"after capture");
    }

    #[test]
    fn joined_capture_reconstructs_soft_wrap_at_authoritative_width() {
        let command = capture_command("%1");
        assert!(command.matches("capture-pane -p -e -J").count() == 2);
        assert!(command.contains("#{pane_width}"));
        let logical_line = vec![b'w'; 160];
        let seed = build_seed(
            "%1",
            vec![logical_line.clone()],
            vec![],
            &[b"__ADE_META__:%1:0:0:0:1:0:0:0:0:0:1:0:0:0:80:".to_vec()],
        )
        .unwrap();
        assert!(
            seed.bytes
                .windows(logical_line.len())
                .any(|window| window == logical_line)
        );
        let wrap_enable = seed
            .bytes
            .windows(b"\x1b[?7h".len())
            .position(|window| window == b"\x1b[?7h")
            .unwrap();
        let line = seed
            .bytes
            .windows(logical_line.len())
            .position(|window| window == logical_line)
            .unwrap();
        assert!(wrap_enable < line);
    }
}
