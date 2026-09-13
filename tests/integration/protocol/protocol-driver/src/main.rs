use std::{
    collections::BTreeMap,
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use protocol_driver_support::{Bridge, Hello, local_bridge_command, ssh_bridge_command};
use serde_json::json;
use tmux_agent_protocol::{
    envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
};

#[derive(Clone)]
enum Transport {
    Local {
        host_binary: String,
        runtime: String,
        tmux_socket: String,
    },
    Ssh {
        config: String,
        target: String,
    },
}

impl Transport {
    fn bridge_command(&self) -> Command {
        match self {
            Self::Local {
                host_binary,
                runtime,
                tmux_socket,
            } => {
                let mut command = local_bridge_command(host_binary);
                command
                    .env("ADE_PHASE1_TESTING", "1")
                    .env("ADE_HOST_RUNTIME_DIR", runtime)
                    .env("ADE_TMUX_SOCKET_NAME", tmux_socket);
                command
            }
            Self::Ssh { config, target } => ssh_bridge_command(
                config,
                target,
                "env ADE_PHASE1_TESTING=1 $HOME/.local/bin/muxflow-host bridge --stdio",
                &["-o", "BatchMode=yes"],
            ),
        }
    }

    fn command(&self, program: &str, arguments: &[&str]) -> Result<Output> {
        let output = match self {
            Self::Local { tmux_socket, .. } if program == "tmux" => Command::new("tmux")
                .args(["-L", tmux_socket])
                .args(arguments)
                .output(),
            Self::Local { .. } => Command::new(program).args(arguments).output(),
            Self::Ssh { config, target } => {
                let remote_command = std::iter::once(program)
                    .chain(arguments.iter().copied())
                    .map(shell_quote)
                    .collect::<Vec<_>>()
                    .join(" ");
                Command::new("ssh")
                    .args(["-F", config, target, &remote_command])
                    .output()
            }
        }
        .with_context(|| format!("run external {program} command"))?;
        Ok(output)
    }

    fn tmux(&self, arguments: &[&str]) -> Result<String> {
        let output = self.command("tmux", arguments)?;
        ensure!(
            output.status.success(),
            "external tmux command failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    fn stop_daemon(&self) -> Result<()> {
        let output = match self {
            Self::Local {
                host_binary,
                runtime,
                tmux_socket,
            } => Command::new(host_binary)
                .arg("daemon-stop")
                .env("ADE_HOST_RUNTIME_DIR", runtime)
                .env("ADE_TMUX_SOCKET_NAME", tmux_socket)
                .output(),
            Self::Ssh { config, target } => Command::new("ssh")
                .args([
                    "-F",
                    config,
                    target,
                    "$HOME/.local/bin/muxflow-host daemon-stop",
                ])
                .output(),
        }
        .context("stop host daemon")?;
        ensure!(
            output.status.success(),
            "daemon-stop failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Default)]
struct Observations {
    last_sequence: u64,
    gap: bool,
    event_counts: BTreeMap<String, usize>,
    event_details: Vec<String>,
    terminal_bytes: Vec<u8>,
    resources: Vec<v1::PaneResource>,
    seeded_panes: std::collections::BTreeSet<String>,
    seed_diagnostics: Vec<(String, String)>,
    terminal_generation: u64,
    pane_generations: BTreeMap<String, u64>,
}

impl Observations {
    fn event_count(&self, kind: v1::EventKind) -> usize {
        self.event_counts
            .get(&format!("{kind:?}"))
            .copied()
            .unwrap_or_default()
    }

    fn track(&mut self, frame: v1::Envelope) {
        let Some(Payload::Event(event)) = frame.payload else {
            return;
        };
        if self.last_sequence != 0 && frame.sequence != self.last_sequence.saturating_add(1) {
            self.gap = true;
        }
        self.last_sequence = frame.sequence;
        let kind = v1::EventKind::try_from(event.kind).unwrap_or_default();
        *self.event_counts.entry(format!("{kind:?}")).or_default() += 1;
        self.event_details
            .push(format!("{kind:?}:{}:{}", event.scope, event.detail));
        if self.event_details.len() > 64 {
            self.event_details.remove(0);
        }
        if kind == v1::EventKind::TerminalSeedDiagnostic {
            self.seed_diagnostics
                .push((event.scope.clone(), event.detail.clone()));
        }
        if let Some(terminal) = event.terminal {
            self.terminal_generation = self.terminal_generation.max(terminal.generation);
            self.pane_generations
                .entry(terminal.pane_id.clone())
                .and_modify(|generation| *generation = (*generation).max(terminal.generation))
                .or_insert(terminal.generation);
            if kind == v1::EventKind::TerminalSeed {
                self.seeded_panes.insert(terminal.pane_id.clone());
            }
            self.terminal_bytes.extend_from_slice(&terminal.data);
        }
        if let Some(resource) = event.pane_resource {
            self.pane_generations
                .entry(resource.pane_id.clone())
                .and_modify(|generation| {
                    *generation = (*generation).max(resource.tail_through_generation)
                })
                .or_insert(resource.tail_through_generation);
            self.resources.push(resource);
        }
    }

    fn contains_terminal(&self, needle: &[u8]) -> bool {
        self.terminal_bytes
            .windows(needle.len())
            .any(|window| window == needle)
    }

    fn terminal_occurrences(&self, needle: &[u8]) -> usize {
        self.terminal_bytes
            .windows(needle.len())
            .filter(|window| *window == needle)
            .count()
    }
}

struct ProtocolClient {
    bridge: Bridge,
    observations: Observations,
    server_identity: String,
}

impl ProtocolClient {
    fn connect(transport: &Transport) -> Result<Self> {
        let (bridge, hello) =
            Bridge::connect(&mut transport.bridge_command(), Hello::control(), 11)
                .map_err(anyhow::Error::msg)
                .context("start Phase 2 protocol bridge")?;
        Ok(Self {
            bridge,
            observations: Observations::default(),
            server_identity: hello.server_identity,
        })
    }

    fn request(
        &mut self,
        operation: v1::Operation,
        mut request: v1::Request,
    ) -> Result<v1::Response> {
        let request_id = self.bridge.next_request_id().map_err(anyhow::Error::msg)?;
        request.operation = operation.into();
        write_frame_sync(
            &mut self.bridge.stdin,
            &envelope(request_id, 0, Payload::Request(request)),
        )?;
        loop {
            let frame = read_frame_sync(&mut self.bridge.reader)?
                .context("bridge closed while awaiting Phase 2 response")?;
            if frame.request_id == request_id
                && let Some(Payload::Response(response)) = frame.payload.clone()
            {
                return Ok(response);
            }
            self.observations.track(frame);
        }
    }

    fn subscribe(&mut self) -> Result<v1::Snapshot> {
        let response = self.request(v1::Operation::Subscribe, v1::Request::default())?;
        ensure!(
            response.ok,
            "subscribe failed: {}",
            response.display_message
        );
        self.observations.last_sequence = response.accepted_sequence;
        response.snapshot.context("subscribe omitted snapshot")
    }

    fn snapshot(&mut self) -> Result<v1::Snapshot> {
        let response = self.request(
            v1::Operation::FullSnapshot,
            v1::Request {
                scope: "topology".into(),
                ..Default::default()
            },
        )?;
        ensure!(response.ok, "snapshot failed: {}", response.display_message);
        response
            .snapshot
            .context("snapshot response omitted topology")
    }

    fn wait_snapshot(
        &mut self,
        mut predicate: impl FnMut(&v1::Snapshot) -> bool,
    ) -> Result<v1::Snapshot> {
        for _ in 0..80 {
            let snapshot = self.snapshot()?;
            if predicate(&snapshot) {
                return Ok(snapshot);
            }
            thread::sleep(Duration::from_millis(50));
        }
        bail!("timed out waiting for authoritative topology convergence")
    }

    fn action_with_snapshot(
        &mut self,
        snapshot: &v1::Snapshot,
        mut action: v1::TmuxAction,
    ) -> Result<v1::Response> {
        action.expected_server_identity = self.server_identity.clone();
        action.expected_generation = snapshot.generation;
        self.request(
            v1::Operation::TmuxAction,
            v1::Request {
                tmux_action: Some(action),
                ..Default::default()
            },
        )
    }

    fn action(&mut self, action: v1::TmuxAction) -> Result<v1::TmuxActionResult> {
        let snapshot = self.snapshot()?;
        let response = self.action_with_snapshot(&snapshot, action)?;
        ensure!(
            response.ok,
            "protocol tmux action failed ({}): {}",
            response.error_code,
            response.display_message
        );
        response
            .tmux_action_result
            .context("tmux action omitted result")
    }

    fn reject_unconfirmed(&mut self, action: v1::TmuxAction) -> Result<()> {
        let snapshot = self.snapshot()?;
        let response = self.action_with_snapshot(&snapshot, action)?;
        ensure!(
            !response.ok,
            "unconfirmed destructive action unexpectedly succeeded"
        );
        ensure!(
            response.error_code == "confirmation_required",
            "unexpected destructive rejection: {} ({})",
            response.error_code,
            response.display_message
        );
        Ok(())
    }

    fn attach(&mut self, snapshot: &v1::Snapshot, session_id: &str) -> Result<()> {
        let pane_ids = snapshot
            .panes
            .iter()
            .filter(|pane| pane.session_id == session_id)
            .map(|pane| pane.id.clone())
            .collect();
        let response = self.request(
            v1::Operation::AttachTerminal,
            v1::Request {
                session_id: session_id.into(),
                pane_ids,
                ..Default::default()
            },
        )?;
        ensure!(
            response.ok,
            "terminal attach failed: {}",
            response.display_message
        );
        Ok(())
    }

    fn input(&mut self, pane_id: &str, data: &[u8]) -> Result<()> {
        let response = self.request(
            v1::Operation::TerminalInput,
            v1::Request {
                scope: pane_id.into(),
                data: data.to_vec(),
                ..Default::default()
            },
        )?;
        ensure!(
            response.ok,
            "terminal input failed: {}",
            response.display_message
        );
        Ok(())
    }

    fn pipeline_input(&mut self, pane_id: &str, chunks: &[&[u8]]) -> Result<Duration> {
        let started = Instant::now();
        let mut pending = BTreeMap::new();
        for data in chunks {
            let request_id = self.bridge.next_request_id().map_err(anyhow::Error::msg)?;
            pending.insert(request_id, ());
            write_frame_sync(
                &mut self.bridge.stdin,
                &envelope(
                    request_id,
                    0,
                    Payload::Request(v1::Request {
                        operation: v1::Operation::TerminalInput.into(),
                        scope: pane_id.into(),
                        data: data.to_vec(),
                        ..Default::default()
                    }),
                ),
            )?;
        }
        while !pending.is_empty() {
            let frame = read_frame_sync(&mut self.bridge.reader)?
                .context("bridge closed while draining pipelined input")?;
            if pending.contains_key(&frame.request_id)
                && let Some(Payload::Response(response)) = frame.payload.clone()
            {
                ensure!(
                    response.ok,
                    "pipelined input was rejected: {}",
                    response.display_message
                );
                pending.remove(&frame.request_id);
            } else {
                self.observations.track(frame);
            }
        }
        Ok(started.elapsed())
    }

    fn request_seed(&mut self, pane_id: &str) -> Result<()> {
        let response = self.request(
            v1::Operation::RequestTerminalSeed,
            v1::Request {
                scope: pane_id.into(),
                ..Default::default()
            },
        )?;
        ensure!(
            response.ok,
            "terminal seed request failed: {}",
            response.display_message
        );
        Ok(())
    }

    fn set_visible(&mut self, pane_id: &str, visible: bool) -> Result<()> {
        let response = self.request(
            v1::Operation::SetTerminalVisibility,
            v1::Request {
                scope: pane_id.into(),
                visible,
                ..Default::default()
            },
        )?;
        ensure!(
            response.ok,
            "terminal visibility failed: {}",
            response.display_message
        );
        Ok(())
    }

    // How long to wait for an expected event, not a bound the product must meet:
    // all twelve call sites wait for something to become true, and none treats
    // expiry as a passing result, so a longer budget cannot weaken an assertion
    // — the predicate still has to hold. The former 100 iterations gave five
    // seconds, which is calibrated for a runner several times quicker on this
    // path than an Apple Silicon Mac; three different waits timed out at
    // different points across runs here. Twenty seconds keeps every wait
    // decisive without making a slow machine look like a broken one.
    fn pump_until(&mut self, mut predicate: impl FnMut(&Observations) -> bool) -> Result<()> {
        for _ in 0..400 {
            let _ = self.snapshot()?;
            if predicate(&self.observations) {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }
        bail!(
            "timed out waiting for protocol event; counts={:?}; seeded={:?}; recent={:?}",
            self.observations.event_counts,
            self.observations.seeded_panes,
            self.observations.event_details
        )
    }

    fn close(self) {
        self.bridge.close();
    }
}

fn session<'a>(snapshot: &'a v1::Snapshot, name: &str) -> Result<&'a v1::Session> {
    snapshot
        .sessions
        .iter()
        .find(|session| session.name == name)
        .with_context(|| format!("session {name:?} missing from snapshot"))
}

fn window<'a>(snapshot: &'a v1::Snapshot, id: &str) -> Result<&'a v1::Window> {
    snapshot
        .windows
        .iter()
        .find(|window| window.id == id)
        .with_context(|| format!("window {id} missing from snapshot"))
}

fn pane<'a>(snapshot: &'a v1::Snapshot, id: &str) -> Result<&'a v1::Pane> {
    snapshot
        .panes
        .iter()
        .find(|pane| pane.id == id)
        .with_context(|| format!("pane {id} missing from snapshot"))
}

fn action(kind: v1::TmuxActionKind) -> v1::TmuxAction {
    v1::TmuxAction {
        kind: kind.into(),
        ..Default::default()
    }
}

fn assert_layout_matches(
    transport: &Transport,
    snapshot: &v1::Snapshot,
    window_id: &str,
) -> Result<()> {
    let expected =
        transport.tmux(&["display-message", "-p", "-t", window_id, "#{window_layout}"])?;
    ensure!(
        window(snapshot, window_id)?.layout == expected,
        "protocol layout diverged from authoritative tmux layout"
    );
    Ok(())
}

fn run_matrix(transport: Transport, primary_name: &str, ordinary_client: &str) -> Result<()> {
    let mut client = ProtocolClient::connect(&transport)?;
    let initial = client.subscribe()?;
    let primary = session(&initial, primary_name)?.clone();
    let original_window = initial
        .windows
        .iter()
        .find(|window| window.session_id == primary.id)
        .context("primary session has no window")?
        .clone();
    let original_pane = initial
        .panes
        .iter()
        .find(|pane| pane.window_id == original_window.id)
        .context("primary window has no pane")?
        .clone();

    // Input acceptance is bounded and pipelined: a key burst does not pay one
    // control round trip per byte, and a following topology action observes an
    // input barrier without the former 50 ms per-key delay.
    //
    // The bound guards against the 50 ms-per-key regression, which would put
    // this burst above 6.4 s. It was widened from 2 s to 4 s under M10-E044,
    // when an Apple Silicon Mac missed the original bound consistently at
    // roughly 16 ms per operation — three tmux client forks per keystroke.
    // Phase 12 writes input in band through the open control client, so that
    // cost is gone and the bound is back at its intended 2 s.
    client.attach(&initial, &primary.id)?;
    let mut chunks: Vec<&[u8]> = vec![b"#"; 128];
    chunks.push(b"\r");
    let input_latency = client.pipeline_input(&original_pane.id, &chunks)?;
    ensure!(
        input_latency < Duration::from_secs(2),
        "pipelined terminal input took {input_latency:?}"
    );
    let wrap_marker = format!("PHASE2_ALT_WRAP_{}", "W".repeat(160));
    let enter_alt = format!(
        "printf '\\033[?1049h\\033[?2004h\\033[?1004h{}\\n'\r",
        wrap_marker
    );
    let response = client.request(
        v1::Operation::TerminalInput,
        v1::Request {
            scope: original_pane.id.clone(),
            data: enter_alt.into_bytes(),
            ..Default::default()
        },
    )?;
    ensure!(
        response.ok,
        "failed to enter alternate-screen parity fixture"
    );
    thread::sleep(Duration::from_millis(100));
    client.request_seed(&original_pane.id)?;
    client.pump_until(|observations| observations.contains_terminal(wrap_marker.as_bytes()))?;
    let exit_alt = client.request(
        v1::Operation::TerminalInput,
        v1::Request {
            scope: original_pane.id.clone(),
            data: b"printf '\\x1b[?1004l\\x1b[?2004l\\x1b[?1049l'\r".to_vec(),
            ..Default::default()
        },
    )?;
    ensure!(
        exit_alt.ok,
        "failed to exit alternate-screen parity fixture"
    );
    let control_started = Instant::now();
    let mut latency_focus = action(v1::TmuxActionKind::FocusPane);
    latency_focus.pane_id = original_pane.id.clone();
    client.action(latency_focus)?;
    let control_latency = control_started.elapsed();
    ensure!(
        control_latency < Duration::from_secs(2),
        "control action behind terminal input took {control_latency:?}"
    );

    // Protocol-originated session lifecycle and the private sidecar order.
    let mut create_session = action(v1::TmuxActionKind::CreateSession);
    create_session.name = "p2-protocol-created".into();
    let created_session = client.action(create_session)?.session_id;
    let mut reorder_session = action(v1::TmuxActionKind::ReorderSession);
    reorder_session.session_id = created_session.clone();
    reorder_session.index = 0;
    client.action(reorder_session)?;
    let reordered = client.snapshot()?;
    ensure!(
        reordered.sessions.first().map(|item| item.id.as_str()) == Some(created_session.as_str()),
        "session sidecar reorder did not become snapshot presentation order"
    );
    let mut rename_session = action(v1::TmuxActionKind::RenameSession);
    rename_session.session_id = created_session.clone();
    rename_session.name = "p2-protocol-renamed".into();
    client.action(rename_session)?;
    let mut select_session = action(v1::TmuxActionKind::SelectSession);
    select_session.session_id = primary.id.clone();
    client.action(select_session)?;

    // Protocol-originated window lifecycle.
    let mut create_window = action(v1::TmuxActionKind::CreateWindow);
    create_window.session_id = primary.id.clone();
    create_window.name = "p2-protocol-window".into();
    let created_window = client.action(create_window)?.window_id;
    let mut rename_window = action(v1::TmuxActionKind::RenameWindow);
    rename_window.window_id = created_window.clone();
    rename_window.name = "p2-protocol-window-renamed".into();
    client.action(rename_window)?;
    let mut reorder_window = action(v1::TmuxActionKind::ReorderWindow);
    reorder_window.session_id = primary.id.clone();
    reorder_window.window_id = created_window.clone();
    reorder_window.target_window_id = original_window.id.clone();
    reorder_window.relative_position = v1::WindowRelativePosition::Before.into();
    client.action(reorder_window)?;
    let mut select_window = action(v1::TmuxActionKind::SelectWindow);
    select_window.window_id = original_window.id.clone();
    client.action(select_window)?;

    // Protocol-originated right/down split, focus, four resize directions, zoom, and close.
    let mut split_right = action(v1::TmuxActionKind::SplitPaneRight);
    split_right.pane_id = original_pane.id.clone();
    split_right.split_size = 40;
    let right_pane = client.action(split_right)?.pane_id;
    let mut split_down = action(v1::TmuxActionKind::SplitPaneDown);
    split_down.pane_id = original_pane.id.clone();
    split_down.split_size = 40;
    let down_pane = client.action(split_down)?.pane_id;
    let mut focus = action(v1::TmuxActionKind::FocusPane);
    focus.pane_id = down_pane.clone();
    client.action(focus)?;
    for kind in [
        v1::TmuxActionKind::ResizePaneLeft,
        v1::TmuxActionKind::ResizePaneRight,
        v1::TmuxActionKind::ResizePaneUp,
        v1::TmuxActionKind::ResizePaneDown,
    ] {
        let mut resize = action(kind);
        resize.pane_id = down_pane.clone();
        resize.resize_cells = 1;
        client.action(resize)?;
    }
    for zoomed in [true, false] {
        let mut zoom = action(v1::TmuxActionKind::ZoomPane);
        zoom.pane_id = down_pane.clone();
        zoom.zoomed = zoomed;
        client.action(zoom)?;
        let snapshot = client.snapshot()?;
        ensure!(
            window(&snapshot, &original_window.id)?.zoomed == zoomed,
            "protocol zoom did not converge"
        );
    }
    let layout_snapshot = client.snapshot()?;
    assert_layout_matches(&transport, &layout_snapshot, &original_window.id)?;

    // Every destructive type is rejected without confirmation and leaves its target alive.
    let mut close_pane = action(v1::TmuxActionKind::ClosePane);
    close_pane.pane_id = right_pane.clone();
    client.reject_unconfirmed(close_pane.clone())?;
    ensure!(
        pane(&client.snapshot()?, &right_pane).is_ok(),
        "rejected pane close mutated tmux"
    );
    close_pane.confirmed = true;
    client.action(close_pane)?;

    let mut close_window = action(v1::TmuxActionKind::CloseWindow);
    close_window.window_id = created_window.clone();
    client.reject_unconfirmed(close_window.clone())?;
    ensure!(
        window(&client.snapshot()?, &created_window).is_ok(),
        "rejected window close mutated tmux"
    );
    close_window.confirmed = true;
    client.action(close_window)?;

    let mut close_session = action(v1::TmuxActionKind::CloseSession);
    close_session.session_id = created_session.clone();
    client.reject_unconfirmed(close_session.clone())?;
    ensure!(
        client
            .snapshot()?
            .sessions
            .iter()
            .any(|item| item.id == created_session),
        "rejected session close mutated tmux"
    );
    close_session.confirmed = true;
    client.action(close_session)?;

    let mut close_down = action(v1::TmuxActionKind::ClosePane);
    close_down.pane_id = down_pane;
    close_down.confirmed = true;
    client.action(close_down)?;

    // A normal tmux client selects a session while the protocol bridge stays attached.
    transport.tmux(&["switch-client", "-c", ordinary_client, "-t", primary_name])?;
    wait_for_client_session(&transport, ordinary_client, primary_name)?;

    // External session/window/pane lifecycle and topology event reconciliation.
    let external_session = transport.tmux(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{session_id}",
        "-s",
        "p2-external-created",
        "exec bash",
    ])?;
    client.wait_snapshot(|snapshot| {
        snapshot
            .sessions
            .iter()
            .any(|item| item.id == external_session)
    })?;
    transport.tmux(&[
        "rename-session",
        "-t",
        &external_session,
        "p2-external-renamed",
    ])?;
    client.wait_snapshot(|snapshot| {
        snapshot
            .sessions
            .iter()
            .any(|item| item.id == external_session && item.name == "p2-external-renamed")
    })?;
    transport.tmux(&[
        "switch-client",
        "-c",
        ordinary_client,
        "-t",
        "p2-external-renamed",
    ])?;
    wait_for_client_session(&transport, ordinary_client, "p2-external-renamed")?;
    transport.tmux(&["switch-client", "-c", ordinary_client, "-t", primary_name])?;

    let external_window = transport.tmux(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}",
        "-t",
        &external_session,
        "-n",
        "p2-external-window",
        "exec bash",
    ])?;
    client.wait_snapshot(|snapshot| {
        snapshot
            .windows
            .iter()
            .any(|item| item.id == external_window)
    })?;
    transport.tmux(&[
        "rename-window",
        "-t",
        &external_window,
        "p2-external-window-renamed",
    ])?;
    transport.tmux(&["select-window", "-t", &external_window])?;
    client.wait_snapshot(|snapshot| {
        snapshot.windows.iter().any(|item| {
            item.id == external_window && item.name == "p2-external-window-renamed" && item.active
        })
    })?;
    let external_initial_window = client
        .snapshot()?
        .windows
        .into_iter()
        .find(|item| item.session_id == external_session && item.id != external_window)
        .context("external session initial window missing")?;
    transport.tmux(&[
        "swap-window",
        "-d",
        "-s",
        &external_window,
        "-t",
        &external_initial_window.id,
    ])?;
    client.wait_snapshot(|snapshot| {
        window(snapshot, &external_window)
            .is_ok_and(|item| item.index == external_initial_window.index)
    })?;

    let external_base_pane = client
        .snapshot()?
        .panes
        .into_iter()
        .find(|item| item.window_id == external_window)
        .context("external window pane missing")?;
    let external_right = transport.tmux(&[
        "split-window",
        "-d",
        "-h",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &external_base_pane.id,
    ])?;
    let external_down = transport.tmux(&[
        "split-window",
        "-d",
        "-v",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &external_base_pane.id,
    ])?;
    client.wait_snapshot(|snapshot| {
        snapshot.panes.iter().any(|item| item.id == external_right)
            && snapshot.panes.iter().any(|item| item.id == external_down)
    })?;
    transport.tmux(&["select-pane", "-t", &external_down])?;
    transport.tmux(&["resize-pane", "-t", &external_down, "-R", "1"])?;
    transport.tmux(&["resize-pane", "-t", &external_down, "-D", "1"])?;
    transport.tmux(&["resize-pane", "-Z", "-t", &external_down])?;
    client.wait_snapshot(|snapshot| {
        pane(snapshot, &external_down).is_ok_and(|item| item.active)
            && window(snapshot, &external_window).is_ok_and(|item| item.zoomed)
    })?;
    transport.tmux(&["resize-pane", "-Z", "-t", &external_down])?;

    // A stale protocol layout command is rejected after the external generation wins;
    // resnapshot plus retry then converges exactly to tmux's layout.
    let stale = client.snapshot()?;
    transport.tmux(&["resize-pane", "-t", &external_down, "-L", "1"])?;
    client.wait_snapshot(|snapshot| {
        snapshot.generation > stale.generation
            && assert_layout_matches(&transport, snapshot, &external_window).is_ok()
    })?;
    let mut stale_resize = action(v1::TmuxActionKind::ResizePaneLeft);
    stale_resize.pane_id = external_down.clone();
    let rejection = client.action_with_snapshot(&stale, stale_resize)?;
    ensure!(
        !rejection.ok && rejection.error_code == "stale_topology",
        "stale layout action was not rejected"
    );
    let mut fresh_resize = action(v1::TmuxActionKind::ResizePaneLeft);
    fresh_resize.pane_id = external_down.clone();
    let fresh = client.snapshot()?;
    let fresh_response = client.action_with_snapshot(&fresh, fresh_resize)?;
    ensure!(
        fresh_response.ok,
        "fresh layout retry failed: {} ({})",
        fresh_response.error_code,
        fresh_response.display_message
    );
    let final_layout = client.snapshot()?;
    assert_layout_matches(&transport, &final_layout, &external_window)?;

    transport.tmux(&["kill-pane", "-t", &external_right])?;
    transport.tmux(&["kill-pane", "-t", &external_down])?;
    transport.tmux(&["kill-window", "-t", &external_window])?;
    transport.tmux(&["kill-session", "-t", &external_session])?;
    client.wait_snapshot(|snapshot| {
        !snapshot
            .sessions
            .iter()
            .any(|item| item.id == external_session)
    })?;

    // Changing the already attached session membership must not replace the
    // reader or duplicate output.
    let mut create_exact_window = action(v1::TmuxActionKind::CreateWindow);
    create_exact_window.session_id = primary.id.clone();
    create_exact_window.name = "p2-exact-window".into();
    let exact_window = client.action(create_exact_window)?.window_id;
    let exact_snapshot = client.wait_snapshot(|snapshot| {
        snapshot
            .panes
            .iter()
            .any(|pane| pane.window_id == exact_window)
    })?;
    let exact_pane = exact_snapshot
        .panes
        .iter()
        .find(|pane| pane.window_id == exact_window)
        .context("app-created exact test pane missing")?
        .id
        .clone();
    client.set_visible(&exact_pane, true)?;
    client.request_seed(&exact_pane)?;
    client.pump_until(|observations| observations.seeded_panes.contains(&exact_pane))?;
    let process_id = std::process::id();
    let exact_once_marker = format!("PHASE2_EXACT_ONCE_{process_id}");
    client.input(
        &exact_pane,
        format!("printf 'PHASE2_EXACT_%s_%s\\n' ONCE {process_id}\r").as_bytes(),
    )?;
    client
        .pump_until(|observations| observations.contains_terminal(exact_once_marker.as_bytes()))
        .context("wait for exact-once pane-membership marker")?;
    thread::sleep(Duration::from_millis(100));
    client.snapshot()?;
    ensure!(
        client
            .observations
            .terminal_occurrences(exact_once_marker.as_bytes())
            == 1,
        "pane membership replacement duplicated terminal output"
    );
    let mut close_exact = action(v1::TmuxActionKind::CloseWindow);
    close_exact.window_id = exact_window;
    close_exact.confirmed = true;
    client.action(close_exact)?;

    // Hidden renderer material is buffered, bounded, released, and reseeded on reattach.
    let topology = client.snapshot()?;
    client.attach(&topology, &primary.id)?;
    let initial_seeds = client.observations.event_count(v1::EventKind::TerminalSeed);
    client.request_seed(&original_pane.id)?;
    client
        .pump_until(|observations| {
            observations.event_count(v1::EventKind::TerminalSeed) > initial_seeds
        })
        .context("wait for initial terminal seed")?;
    let resources_before = client.observations.resources.len();
    let visibility_epoch = 1;
    let hide_cutoff = client
        .observations
        .pane_generations
        .get(&original_pane.id)
        .copied()
        .unwrap_or_default();
    let response = client.request(
        v1::Operation::SetTerminalVisibility,
        v1::Request {
            scope: original_pane.id.clone(),
            visible: false,
            terminal_epoch: visibility_epoch,
            terminal_generation_cutoff: hide_cutoff,
            ..Default::default()
        },
    )?;
    ensure!(response.ok, "hide pane failed");
    client
        .pump_until(|observations| observations.resources.len() > resources_before)
        .context("wait for hidden pane resource event")?;
    let hidden = client
        .observations
        .resources
        .last()
        .context("hidden resource event missing")?;
    ensure!(
        v1::PaneResourceState::try_from(hidden.state).unwrap_or_default()
            == v1::PaneResourceState::HiddenBuffered,
        "pane did not enter hidden-buffered state"
    );
    ensure!(
        !hidden.requires_seed && hidden.recovery_reason.is_empty(),
        "healthy hidden pane incorrectly required seed recovery"
    );
    ensure!(
        hidden.snapshot_generation == hide_cutoff
            && hidden.tail_through_generation >= hidden.snapshot_generation,
        "hidden resource generation boundaries were not authoritative"
    );
    client.input(&original_pane.id, b"printf 'PHASE2_HIDDEN_OUTPUT\\n'\r")?;
    thread::sleep(Duration::from_millis(200));
    let resource_count = client.observations.resources.len();
    let response = client.request(
        v1::Operation::SetTerminalVisibility,
        v1::Request {
            scope: original_pane.id.clone(),
            visible: false,
            terminal_epoch: visibility_epoch,
            terminal_generation_cutoff: hide_cutoff,
            ..Default::default()
        },
    )?;
    ensure!(response.ok, "hidden refresh failed");
    client
        .pump_until(|observations| observations.resources.len() > resource_count)
        .context("wait for buffered-tail pane resource event")?;
    let buffered = client
        .observations
        .resources
        .last()
        .context("buffered resource event missing")?;
    ensure!(
        buffered
            .raw_tail
            .windows(b"PHASE2_HIDDEN_OUTPUT".len())
            .any(|part| part == b"PHASE2_HIDDEN_OUTPUT"),
        "hidden output was not retained in the bounded raw tail"
    );
    ensure!(
        buffered.snapshot_generation == hide_cutoff
            && buffered.tail_through_generation > hide_cutoff,
        "hidden raw tail did not expose an exact deferred-output boundary"
    );
    ensure!(
        !client
            .observations
            .contains_terminal(b"PHASE2_HIDDEN_OUTPUT"),
        "hidden output was duplicated onto the terminal IPC stream"
    );
    let response = client.request(
        v1::Operation::SetTerminalVisibility,
        v1::Request {
            scope: original_pane.id.clone(),
            visible: true,
            ..Default::default()
        },
    )?;
    ensure!(response.ok, "show pane before second handoff failed");
    let resource_count = client.observations.resources.len();
    let oversized_cutoff = client
        .observations
        .pane_generations
        .get(&original_pane.id)
        .copied()
        .unwrap_or_default();
    let response = client.request(
        v1::Operation::SetTerminalVisibility,
        v1::Request {
            scope: original_pane.id.clone(),
            visible: false,
            data: vec![b'x'; 4 * 1024 * 1024 + 1],
            terminal_epoch: visibility_epoch,
            terminal_generation_cutoff: oversized_cutoff,
            ..Default::default()
        },
    )?;
    ensure!(response.ok, "oversized hidden snapshot request failed");
    client
        .pump_until(|observations| observations.resources.len() > resource_count)
        .context("wait for released pane resource event")?;
    let released = client
        .observations
        .resources
        .last()
        .context("released resource event missing")?;
    ensure!(
        v1::PaneResourceState::try_from(released.state).unwrap_or_default()
            == v1::PaneResourceState::Released,
        "oversized hidden resource was not released"
    );
    ensure!(
        released.requires_seed
            && released.recovery_reason == "renderer handoff exceeded the hidden-pane budget",
        "released pane omitted deterministic seed-recovery metadata"
    );
    let seeds_before = client.observations.event_count(v1::EventKind::TerminalSeed);
    let response = client.request(
        v1::Operation::SetTerminalVisibility,
        v1::Request {
            scope: original_pane.id.clone(),
            visible: true,
            ..Default::default()
        },
    )?;
    ensure!(response.ok, "show pane failed");
    client
        .pump_until(|observations| {
            observations.event_count(v1::EventKind::TerminalSeed) > seeds_before
        })
        .context("wait for visible reattach seed")?;

    // Saturate terminal/event output while the consumer pauses, then require an
    // explicit overflow signal, authoritative resync, and fresh screen seed.
    let flood =
        b"yes PHASE2_FLOOD_LINE_0123456789 | head -c 16777216; printf '\\nPHASE2_FLOOD_DONE\\n'\r";
    let flood_request = client
        .bridge
        .next_request_id()
        .map_err(anyhow::Error::msg)?;
    write_frame_sync(
        &mut client.bridge.stdin,
        &envelope(
            flood_request,
            0,
            Payload::Request(v1::Request {
                operation: v1::Operation::TerminalInput.into(),
                scope: original_pane.id.clone(),
                data: flood.to_vec(),
                ..Default::default()
            }),
        ),
    )?;
    thread::sleep(Duration::from_secs(2));
    loop {
        let frame =
            read_frame_sync(&mut client.bridge.reader)?.context("bridge closed during flood")?;
        if frame.request_id == flood_request && matches!(frame.payload, Some(Payload::Response(_)))
        {
            break;
        }
        client.observations.track(frame);
    }
    let overflow = client.request(v1::Operation::TestOverflow, v1::Request::default())?;
    ensure!(
        overflow.ok,
        "deterministic overflow injection failed: {} ({})",
        overflow.error_code,
        overflow.display_message
    );
    client
        .pump_until(|observations| {
            observations.gap || observations.event_count(v1::EventKind::ResyncRequired) > 0
        })
        .context("wait for event overflow recovery signal")?;
    let resync = client.request(v1::Operation::Resync, v1::Request::default())?;
    ensure!(
        resync.ok && resync.snapshot.is_some(),
        "overflow resync omitted authoritative snapshot"
    );
    let seeds_before = client.observations.event_count(v1::EventKind::TerminalSeed);
    client.request_seed(&original_pane.id)?;
    client
        .pump_until(|observations| {
            observations.event_count(v1::EventKind::TerminalSeed) > seeds_before
                && observations.contains_terminal(b"PHASE2_FLOOD_DONE")
        })
        .context("wait for post-backpressure terminal resnapshot")?;

    ensure!(
        client
            .observations
            .event_count(v1::EventKind::TopologySnapshot)
            >= 8,
        "insufficient live topology snapshots observed"
    );
    let tmux_version = transport.tmux(&["-V"])?;
    ensure!(
        client
            .observations
            .seed_diagnostics
            .iter()
            .any(|(pane, detail)| pane == &original_pane.id && detail.contains("focus-reporting")),
        "seed omitted the required unavailable-focus diagnostic"
    );
    if tmux_version.contains("3.3") {
        ensure!(
            client
                .observations
                .seed_diagnostics
                .iter()
                .any(|(pane, detail)| {
                    pane == &original_pane.id && detail.contains("bracketed-paste")
                }),
            "tmux 3.3 seed omitted unavailable bracketed-paste diagnostic"
        );
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "transport": match transport { Transport::Local { .. } => "local", Transport::Ssh { .. } => "ssh" },
            "protocolActions": true,
            "externalActions": true,
            "destructiveConfirmation": true,
            "ordinaryClient": true,
            "sessionSidecarOrder": true,
            "layoutConvergence": true,
            "exactOuterTopology": true,
            "pipelinedInputLatencyMs": input_latency.as_millis(),
            "controlLatencyMs": control_latency.as_millis(),
            "exactOnceMembershipOutput": true,
            "hiddenReleaseReattach": true,
            "backpressureResnapshot": true,
            "seedModeDiagnostics": true,
            "eventCounts": client.observations.event_counts,
            "sequenceGapObserved": client.observations.gap,
            "overflowSignalObserved": client.observations.gap
                || client.observations.event_count(v1::EventKind::ResyncRequired) > 0,
        }))?
    );
    Ok(())
}

fn wait_for_client_session(
    transport: &Transport,
    client: &str,
    expected_session: &str,
) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        // `display-message -c` can retain the command's target-session format
        // context on tmux 3.7. Query the client row itself instead.
        let clients = transport.tmux(&["list-clients", "-F", "#{client_name}|#{session_name}"])?;
        let selected = clients
            .lines()
            .filter_map(|line| line.split_once('|'))
            .find_map(|(name, session)| (name == client).then_some(session))
            .with_context(|| format!("ordinary tmux client {client:?} disappeared"))?;
        if selected == expected_session {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!(
                "ordinary client {client:?} did not select {expected_session:?} (selected {selected:?}; clients {:?})",
                transport.tmux(&[
                    "list-clients",
                    "-F",
                    "#{client_name}:#{session_name}:#{client_control_mode}",
                ])?
            );
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn run_daemon_reconnect(transport: Transport, primary_name: &str) -> Result<()> {
    let marker = format!("PHASE2_DAEMON_OFFLINE_{}", std::process::id());
    let mut first = ProtocolClient::connect(&transport)?;
    let initial = first.subscribe()?;
    let primary = session(&initial, primary_name)?.clone();
    let pane = initial
        .panes
        .iter()
        .find(|item| item.session_id == primary.id)
        .context("primary pane missing")?
        .clone();
    first.attach(&initial, &primary.id)?;
    first.close();
    transport.stop_daemon()?;
    transport.tmux(&[
        "send-keys",
        "-t",
        &pane.id,
        &format!("printf '{marker}\\n'"),
        "Enter",
    ])?;
    thread::sleep(Duration::from_millis(150));

    let mut second = ProtocolClient::connect(&transport)?;
    let recovered = second.subscribe()?;
    ensure!(
        recovered.sessions.iter().any(|item| item.id == primary.id),
        "daemon reconnect did not resnapshot stable session identity"
    );
    second.attach(&recovered, &primary.id)?;
    second
        .pump_until(|observations| observations.contains_terminal(marker.as_bytes()))
        .context("wait for daemon-offline output reseed")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "daemonReconnect": true,
            "stableSessionIdentity": true,
            "offlineOutputReseeded": true,
            "marker": marker,
        }))?
    );
    Ok(())
}

fn value_after(arguments: &[String], name: &str) -> Result<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .with_context(|| format!("missing {name}"))
}

fn main() -> Result<()> {
    let arguments: Vec<_> = std::env::args().skip(1).collect();
    let scenario = arguments
        .first()
        .context("missing scenario (matrix|daemon-reconnect)")?;
    let mode = value_after(&arguments, "--transport")?;
    let transport = match mode.as_str() {
        "local" => Transport::Local {
            host_binary: value_after(&arguments, "--host-binary")?,
            runtime: value_after(&arguments, "--runtime")?,
            tmux_socket: value_after(&arguments, "--tmux-socket")?,
        },
        "ssh" => Transport::Ssh {
            config: value_after(&arguments, "--ssh-config")?,
            target: value_after(&arguments, "--ssh-target")?,
        },
        _ => bail!("transport must be local or ssh"),
    };
    let primary = value_after(&arguments, "--primary")?;
    match scenario.as_str() {
        "matrix" => run_matrix(
            transport,
            &primary,
            &value_after(&arguments, "--ordinary-client")?,
        ),
        "daemon-reconnect" => run_daemon_reconnect(transport, &primary),
        _ => bail!("unknown scenario {scenario:?}"),
    }
}
