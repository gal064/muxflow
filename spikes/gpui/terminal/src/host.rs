//! Disposable S11 protocol consumer. Parses on one reader thread and releases
//! host credit only after admission to the existing alacritty grids.
use crate::{
    control::Signal,
    feed::{PaneRect, Session},
    probe::Probe,
};
use alacritty_terminal::vte::ansi::{Handler, Processor};
use std::{
    collections::HashMap,
    io::BufReader,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use tmux_agent_protocol::{
    envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    validate_host_contract, write_frame_sync,
};

pub struct Host {
    writer: Arc<Mutex<std::process::ChildStdin>>,
    next: Arc<AtomicU64>,
    snapshot: Arc<Mutex<v1::Snapshot>>,
    session: Arc<Session>,
    wake: Arc<Signal>,
    child: Child,
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Host {
    pub fn start(
        session: Arc<Session>,
        probe: Arc<Probe>,
        wake: Arc<Signal>,
    ) -> Result<Self, String> {
        // A fixed fixture alias and explicit config keep this spike away from
        // the owner's real SSH host. Local mode requires a private host runtime.
        let mut command = if let Ok(config) = std::env::var("SPIKE_HOST_SSH_CONFIG") {
            let text = std::fs::read_to_string(&config).map_err(|e| e.to_string())?;
            if !text.contains("Host muxflow-s11-container") {
                return Err("S11 requires the isolated container SSH config".into());
            }
            let mut c = Command::new("ssh");
            c.args([
                "-F",
                &config,
                "-T",
                "-o",
                "Compression=yes",
                "muxflow-s11-container",
                "$HOME/.local/bin/muxflow-host bridge --stdio",
            ]);
            c
        } else {
            if std::env::var("ADE_HOST_RUNTIME_DIR").is_err()
                || !std::env::var("ADE_TMUX_SOCKET_NAME")
                    .unwrap_or_default()
                    .starts_with("muxflow-qa-")
            {
                return Err(
                    "local host requires ADE_HOST_RUNTIME_DIR and private ADE_TMUX_SOCKET_NAME"
                        .into(),
                );
            }
            let helper = std::env::var("SPIKE_HOST_LOCAL_HELPER").map_err(|e| e.to_string())?;
            let mut c = Command::new(helper);
            c.args(["bridge", "--stdio"]);
            c
        };
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| e.to_string())?;
        let writer = Arc::new(Mutex::new(child.stdin.take().unwrap()));
        let mut reader = BufReader::new(child.stdout.take().unwrap());
        let epoch = std::process::id() as u64;
        write_frame_sync(
            &mut *writer.lock().unwrap(),
            &envelope(
                1,
                0,
                Payload::ClientHello(v1::ClientHello {
                    connection_epoch: epoch,
                    ..Default::default()
                }),
            ),
        )
        .map_err(|e| e.to_string())?;
        write_frame_sync(
            &mut *writer.lock().unwrap(),
            &envelope(
                2,
                0,
                Payload::Request(v1::Request {
                    operation: v1::Operation::Subscribe.into(),
                    scope: "full".into(),
                    ..Default::default()
                }),
            ),
        )
        .map_err(|e| e.to_string())?;
        let next = Arc::new(AtomicU64::new(10));
        let snapshot = Arc::new(Mutex::new(v1::Snapshot::default()));
        let host = Self {
            writer: writer.clone(),
            next: next.clone(),
            snapshot: snapshot.clone(),
            session: session.clone(),
            wake: wake.clone(),
            child,
        };
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("spike-host-feed".into())
            .spawn(move || {
                let result = (|| -> Result<(), String> {
                    let mut parsers = HashMap::<String, Processor>::new();
                    let mut generations = HashMap::<String, u64>::new();
                    let mut bytes = 0;
                    let mut records = 0;
                    let mut attached = false;
                    let mut visible = Vec::<String>::new();
                    while let Some(frame) =
                        read_frame_sync(&mut reader).map_err(|e| e.to_string())?
                    {
                        validate_host_contract(frame.protocol_major).map_err(|e| e.to_string())?;
                        match frame.payload {
                            Some(Payload::ServerHello(h)) => {
                                if h.connection_epoch != epoch
                                    || h.terminal_output_window_records == 0
                                {
                                    return Err("invalid host handshake".into());
                                }
                                eprintln!(
                                    "S11 host={} os={} tmux={}",
                                    h.server_identity, h.operating_system, h.tmux_version
                                );
                            }
                            Some(Payload::Response(r)) => {
                                if !r.ok {
                                    return Err(format!(
                                        "request {}: {} {}",
                                        frame.request_id, r.error_code, r.display_message
                                    ));
                                }
                                if frame.request_id == 3 {
                                    let _ = ready_tx.try_send(());
                                }
                                if let Some(s) = r.snapshot {
                                    publish(&s, &session);
                                    *snapshot.lock().unwrap() = s;
                                }
                            }
                            Some(Payload::Event(e)) => {
                                if let Some(s) = e.snapshot {
                                    publish(&s, &session);
                                    *snapshot.lock().unwrap() = s;
                                }
                                let kind = v1::EventKind::try_from(e.kind).unwrap_or_default();
                                if matches!(
                                    kind,
                                    v1::EventKind::TerminalSeed | v1::EventKind::TerminalOutput
                                ) {
                                    let t = e.terminal.ok_or("terminal event missing bytes")?;
                                    let generation =
                                        generations.entry(t.pane_id.clone()).or_default();
                                    if t.generation >= *generation {
                                        let pane = session.pane(&t.pane_id);
                                        let parser = parsers.entry(t.pane_id.clone()).or_default();
                                        if kind == v1::EventKind::TerminalSeed {
                                            *parser = Processor::new();
                                            pane.term.lock().reset_state();
                                        }
                                        probe.on_output(&t.pane_id, &t.data);
                                        let g = pane.apply(parser, &t.data, &session.feed_stats);
                                        probe.after_feed(&t.pane_id, g);
                                        *generation = t.generation;
                                    }
                                }
                                bytes += e.terminal_delivery_bytes;
                                records += e.terminal_delivery_records;
                                if e.terminal_delivery_records > 0 {
                                    write_frame_sync(
                                        &mut *writer.lock().unwrap(),
                                        &envelope(
                                            0,
                                            0,
                                            Payload::TerminalOutputAck(v1::TerminalOutputAck {
                                                connection_epoch: epoch,
                                                cumulative_bytes: bytes,
                                                cumulative_records: records,
                                            }),
                                        ),
                                    )
                                    .map_err(|e| e.to_string())?;
                                }
                                if kind == v1::EventKind::TerminalFlowStalled
                                    || kind == v1::EventKind::TerminalResnapshotRequired
                                {
                                    return Err(format!("host recovery required: {}", e.detail));
                                }
                            }
                            _ => {}
                        }
                        let s = snapshot.lock().unwrap().clone();
                        if !attached && !s.sessions.is_empty() {
                            if s.sessions.len() != 1 {
                                return Err("S11 fixture must have exactly one session".into());
                            }
                            let request = v1::Request {
                                operation: v1::Operation::AttachTerminal.into(),
                                session_id: s.sessions[0].id.clone(),
                                pane_ids: s.panes.iter().map(|p| p.id.clone()).collect(),
                                ..Default::default()
                            };
                            write_frame_sync(
                                &mut *writer.lock().unwrap(),
                                &envelope(3, 0, Payload::Request(request)),
                            )
                            .map_err(|e| e.to_string())?;
                            attached = true;
                        }
                        if attached {
                            let active = session.active_window.lock().unwrap().clone();
                            let desired: Vec<_> = s
                                .panes
                                .iter()
                                .filter(|p| Some(&p.window_id) == active.as_ref())
                                .map(|p| p.id.clone())
                                .collect();
                            if desired != visible {
                                for p in &s.panes {
                                    let request = v1::Request {
                                        operation: v1::Operation::SetTerminalVisibility.into(),
                                        terminal_epoch: epoch,
                                        scope: p.id.clone(),
                                        visible: desired.contains(&p.id),
                                        ..Default::default()
                                    };
                                    write_frame_sync(
                                        &mut *writer.lock().unwrap(),
                                        &envelope(
                                            next.fetch_add(1, Ordering::Relaxed),
                                            0,
                                            Payload::Request(request),
                                        ),
                                    )
                                    .map_err(|e| e.to_string())?;
                                }
                                visible = desired;
                            }
                        }
                        wake.notify();
                    }
                    Err("host stream closed".into())
                })();
                eprintln!("S11 transport ended: {result:?}");
                session.exited.store(true, Ordering::Release);
                wake.notify();
            })
            .map_err(|e| e.to_string())?;
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(15))
            .map_err(|e| format!("host attachment: {e}"))?;
        Ok(host)
    }
    fn request(&self, request: v1::Request) {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        if let Err(e) = write_frame_sync(
            &mut *self.writer.lock().unwrap(),
            &envelope(id, 0, Payload::Request(request)),
        ) {
            eprintln!("S11 write: {e}");
        }
    }
    pub fn send_input(&self, pane: &str, data: Vec<u8>) {
        self.request(v1::Request {
            operation: v1::Operation::TerminalInput.into(),
            scope: pane.into(),
            data,
            ..Default::default()
        });
    }
    pub fn resize_client(&self, columns: u16, rows: u16) {
        let s = self.snapshot.lock().unwrap();
        self.request(v1::Request {
            operation: v1::Operation::ResizeTerminalWindow.into(),
            session_id: s.sessions.first().map(|s| s.id.clone()).unwrap_or_default(),
            scope: self
                .session
                .active_window
                .lock()
                .unwrap()
                .clone()
                .unwrap_or_default(),
            columns: columns.into(),
            rows: rows.into(),
            ..Default::default()
        });
    }
    pub fn select_window(&self, window: &str) {
        self.action(v1::TmuxActionKind::SelectWindow, window, "");
    }
    pub fn select_pane(&self, pane: &str) {
        let window = self
            .session
            .active_window
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_default();
        self.action(v1::TmuxActionKind::FocusPane, &window, pane);
    }
    fn action(&self, kind: v1::TmuxActionKind, window: &str, pane: &str) {
        let s = self.snapshot.lock().unwrap();
        self.request(v1::Request {
            operation: v1::Operation::TmuxAction.into(),
            tmux_action: Some(v1::TmuxAction {
                kind: kind.into(),
                session_id: s.sessions.first().map(|s| s.id.clone()).unwrap_or_default(),
                window_id: window.into(),
                pane_id: pane.into(),
                expected_server_identity: s.server_identity.clone(),
                expected_generation: s.generation,
                ..Default::default()
            }),
            ..Default::default()
        });
        self.wake.notify();
    }
}
fn publish(s: &v1::Snapshot, session: &Session) {
    for w in &s.windows {
        let rects = s
            .panes
            .iter()
            .filter(|p| p.window_id == w.id)
            .map(|p| PaneRect {
                pane: p.id.clone(),
                left: p.left as u16,
                top: p.top as u16,
                cols: p.width as u16,
                rows: p.height as u16,
            })
            .collect();
        session.apply_layout(&w.id, rects);
        if w.active {
            *session.active_window.lock().unwrap() = Some(w.id.clone());
        }
    }
    for p in &s.panes {
        if p.active {
            session
                .active_pane
                .lock()
                .unwrap()
                .insert(p.window_id.clone(), p.id.clone());
        }
    }
}
