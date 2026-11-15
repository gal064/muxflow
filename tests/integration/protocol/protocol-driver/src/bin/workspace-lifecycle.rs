//! Exercises the empty -> first workspace -> empty boundary on real tmux.
//! Usage: workspace-lifecycle /absolute/path/to/muxflow-host
use std::{path::PathBuf, process::Command, time::SystemTime};

use anyhow::{Context, Result, ensure};
use protocol_driver_support::{Bridge, Hello, local_bridge_command};
use tmux_agent_protocol::v1::{self, envelope::Payload};

struct Fixture {
    helper: String,
    runtime: PathBuf,
    socket: String,
}

impl Fixture {
    fn command(&self) -> Command {
        let mut command = local_bridge_command(&self.helper);
        command
            .env("ADE_HOST_RUNTIME_DIR", &self.runtime)
            .env("ADE_TMUX_SOCKET_NAME", &self.socket);
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = Command::new(&self.helper)
            .args(["daemon-stop", "--force"])
            .env("ADE_HOST_RUNTIME_DIR", &self.runtime)
            .output();
        let _ = Command::new("tmux")
            .args(["-L", &self.socket, "kill-server"])
            .output();
        let _ = std::fs::remove_dir_all(&self.runtime);
    }
}

fn request(
    bridge: &mut Bridge,
    request: v1::Request,
    identity_until_ack: Option<&str>,
) -> Result<v1::Response> {
    let (response, events) = bridge
        .request_with_events(request)
        .map_err(anyhow::Error::msg)?;
    for frame in events {
        let Some(Payload::Event(event)) = frame.payload else {
            continue;
        };
        let kind = v1::EventKind::try_from(event.kind).unwrap_or_default();
        ensure!(
            kind != v1::EventKind::ResyncRequired
                && !(kind == v1::EventKind::TerminalResnapshotRequired
                    && !event.scope.starts_with('%')),
            "lifecycle requested a bridge reset: {kind:?}: {}",
            event.detail
        );
        if let (Some(expected), Some(snapshot)) = (identity_until_ack, event.snapshot) {
            ensure!(
                snapshot.server_identity == expected,
                "identity changed before the action acknowledgment; a desktop would discard the result"
            );
        }
    }
    ensure!(
        response.ok,
        "{}: {}",
        response.error_code,
        response.display_message
    );
    Ok(response)
}

fn snapshot(bridge: &mut Bridge) -> Result<v1::Snapshot> {
    request(
        bridge,
        v1::Request {
            operation: v1::Operation::FullSnapshot.into(),
            scope: "topology".into(),
            ..Default::default()
        },
        None,
    )?
    .snapshot
    .context("snapshot omitted")
}

fn main() -> Result<()> {
    let helper = std::env::args()
        .nth(1)
        .context("expected absolute helper path")?;
    ensure!(
        PathBuf::from(&helper).is_absolute(),
        "helper path must be absolute"
    );
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let socket = format!("muxflow-lifecycle-{}-{stamp}", std::process::id());
    let runtime = std::env::temp_dir().join(&socket);
    std::fs::create_dir(&runtime)?;
    let fixture = Fixture {
        helper,
        runtime,
        socket,
    };
    for cycle in 0..8 {
        let (mut bridge, _) = Bridge::connect(&mut fixture.command(), Hello::control(), 10)
            .map_err(anyhow::Error::msg)?;
        let initial = bridge.subscribe().map_err(anyhow::Error::msg)?;
        ensure!(
            initial.sessions.is_empty() && initial.server_identity == "tmux:none",
            "fixture must start empty"
        );
        if cycle == 0 {
            request(
                &mut bridge,
                v1::Request {
                    operation: v1::Operation::Chat.into(),
                    chat: Some(v1::ChatRequest {
                        action: v1::ChatAction::SetMode.into(),
                        mode: v1::ChatAgentMode::Chat.into(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                None,
            )?;
        } else {
            ensure!(
                initial
                    .agents
                    .as_ref()
                    .is_some_and(|agents| agents.agent_mode == i32::from(v1::ChatAgentMode::Chat)),
                "reconnecting must retain Chat mode"
            );
        }
        let created = request(
            &mut bridge,
            v1::Request {
                operation: v1::Operation::TmuxAction.into(),
                tmux_action: Some(v1::TmuxAction {
                    kind: v1::TmuxActionKind::CreateSession.into(),
                    name: format!("workspace-{cycle}"),
                    expected_server_identity: initial.server_identity.clone(),
                    expected_generation: initial.generation,
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(&initial.server_identity),
        )?
        .tmux_action_result
        .context("create omitted result")?;
        let current = snapshot(&mut bridge)?;
        ensure!(
            current.sessions.len() == 1,
            "create must produce exactly one workspace"
        );
        ensure!(
            current
                .agents
                .as_ref()
                .is_some_and(|agents| agents.agent_mode == i32::from(v1::ChatAgentMode::Chat)),
            "a new workspace must inherit Chat mode"
        );
        request(
            &mut bridge,
            v1::Request {
                operation: v1::Operation::TmuxAction.into(),
                tmux_action: Some(v1::TmuxAction {
                    kind: v1::TmuxActionKind::CloseSession.into(),
                    session_id: created.session_id,
                    confirmed: true,
                    expected_server_identity: current.server_identity.clone(),
                    expected_generation: current.generation,
                    ..Default::default()
                }),
                ..Default::default()
            },
            Some(&current.server_identity),
        )?;
        let empty = snapshot(&mut bridge)?;
        ensure!(
            empty.sessions.is_empty() && empty.server_identity == "tmux:none",
            "close must restore empty state"
        );
        ensure!(
            empty
                .agents
                .as_ref()
                .is_some_and(|agents| agents.agent_mode == i32::from(v1::ChatAgentMode::Chat)),
            "closing every workspace must retain Chat mode"
        );
        println!("PASS lifecycle cycle {}", cycle + 1);
    }
    Ok(())
}
