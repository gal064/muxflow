use std::{
    io::BufReader,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use tmux_agent_protocol::{
    CAP_AGENTS, HELPER_VERSION, envelope, read_frame_sync,
    v1::{self, envelope::Payload},
    write_frame_sync,
};

const REMOTE_ENV: &str = "HOME=$HOME/phase6-home PATH=$HOME/phase6-bin:/usr/local/bin:/usr/bin:/bin ADE_HOST_RUNTIME_DIR=$HOME/phase6-runtime ADE_TMUX_SOCKET_NAME=ade-phase6";

struct Connection {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    epoch: u64,
    next_request: u64,
}

impl Connection {
    fn open(arguments: &[String], epoch: u64) -> Result<Self, String> {
        let mut child = spawn_bridge(arguments)?;
        let mut stdin = child.stdin.take().ok_or("bridge stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("bridge stdout unavailable")?;
        let mut reader = BufReader::new(stdout);
        handshake(&mut stdin, &mut reader, epoch)?;
        Ok(Self {
            child,
            stdin,
            reader,
            epoch,
            next_request: 10,
        })
    }

    fn request(&mut self, request: v1::Request) -> Result<v1::Response, String> {
        self.request_with_expectation(request, true)
    }

    fn request_error(&mut self, request: v1::Request) -> Result<v1::Response, String> {
        self.request_with_expectation(request, false)
    }

    fn request_with_expectation(
        &mut self,
        request: v1::Request,
        ok: bool,
    ) -> Result<v1::Response, String> {
        let id = self.next_request;
        self.next_request = self.next_request.saturating_add(1);
        write_frame_sync(&mut self.stdin, &envelope(id, 0, Payload::Request(request)))
            .map_err(|error| error.to_string())?;
        loop {
            let frame = read_frame_sync(&mut self.reader)
                .map_err(|error| error.to_string())?
                .ok_or("bridge disconnected")?;
            if frame.request_id == id
                && let Some(Payload::Response(response)) = frame.payload
            {
                if response.ok == ok {
                    return Ok(response);
                }
                return Err(format!(
                    "{}: {}",
                    response.error_code, response.display_message
                ));
            }
        }
    }

    fn subscribe(&mut self) -> Result<v1::Snapshot, String> {
        self.request(v1::Request {
            operation: v1::Operation::Subscribe.into(),
            scope: "full".into(),
            ..Default::default()
        })?
        .snapshot
        .ok_or("subscribe omitted snapshot".into())
    }

    fn active_root(
        &mut self,
        snapshot: &v1::Snapshot,
        pane_id: &str,
    ) -> Result<v1::ActiveRoot, String> {
        self.request(v1::Request {
            operation: v1::Operation::ResolveActiveRoot.into(),
            file: Some(v1::FileServiceRequest {
                operation_id: format!("phase6-root-{}", self.next_request),
                pane_id: pane_id.to_owned(),
                expected_server_identity: snapshot.server_identity.clone(),
                expected_topology_generation: snapshot.generation,
                ..Default::default()
            }),
            ..Default::default()
        })?
        .file
        .and_then(|file| file.active_root)
        .ok_or("active root omitted".into())
    }

    fn agent_snapshot(&mut self) -> Result<v1::AgentSnapshot, String> {
        self.request(v1::Request {
            operation: v1::Operation::AgentSnapshot.into(),
            agent: Some(v1::AgentRequest::default()),
            ..Default::default()
        })?
        .agent
        .and_then(|value| value.snapshot)
        .ok_or("agent snapshot omitted".into())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn main() -> Result<(), String> {
    let arguments = std::env::args().collect::<Vec<_>>();
    if arguments.len() != 4 || arguments[1] != "ssh" {
        return Err("usage: agent-test-driver ssh CONFIG TARGET".into());
    }

    let epoch = 6_000_000_000_001;
    let mut connection = Connection::open(&arguments, epoch)?;
    let initial = connection.subscribe()?;
    let pane = initial
        .panes
        .iter()
        .find(|pane| pane.active)
        .or_else(|| initial.panes.first())
        .ok_or("fixture has no pane")?;
    let initial_pane = pane.id.clone();
    let server_identity = initial.server_identity.clone();

    let registry = connection.agent_snapshot()?;
    if registry.adapters.len() != 2
        || !registry.adapters.iter().all(|adapter| {
            adapter.supports_launch
                && adapter.supports_resume
                && adapter.supports_hooks
                && adapter.supports_process_detection
        })
    {
        return Err("adapter registry is incomplete".into());
    }

    let mut hook_evidence = serde_json::Map::new();
    for (adapter_id, adapter_kind) in [
        ("codex", v1::AgentAdapterKind::Codex),
        ("claude-code", v1::AgentAdapterKind::ClaudeCode),
    ] {
        let review = review_hooks(
            &mut connection,
            adapter_id,
            adapter_kind,
            v1::HookManagementAction::Install,
        )?;
        if review.confirmation_token.is_empty()
            || review.diff_preview.contains("phase6-private-value")
            || review.proposed_command.contains("127.0.0.1")
            || review.proposed_command.contains("tcp")
        {
            return Err(format!("{adapter_id} hook review was unsafe or incomplete"));
        }
        let stale_token = review.confirmation_token.clone();
        remote(
            &arguments,
            "printf '\\n' >>\"$HOME/phase6-home/.phase6-stale-marker\"",
        )?;
        // Changing a separate file must not invalidate the exact config review.
        apply_hooks(
            &mut connection,
            adapter_id,
            adapter_kind,
            v1::HookManagementAction::Install,
            &stale_token,
        )?;
        let current = review_hooks(
            &mut connection,
            adapter_id,
            adapter_kind,
            v1::HookManagementAction::Install,
        )?;
        if !current.already_current {
            return Err(format!("{adapter_id} install is not idempotent"));
        }
        apply_hooks(
            &mut connection,
            adapter_id,
            adapter_kind,
            v1::HookManagementAction::Install,
            &current.confirmation_token,
        )?;

        // Mutate the reviewed config itself and prove stale confirmation fails closed.
        let stale = review_hooks(
            &mut connection,
            adapter_id,
            adapter_kind,
            v1::HookManagementAction::Uninstall,
        )?;
        let command = if adapter_id == "codex" {
            "printf ' ' >>\"$HOME/phase6-home/.codex/hooks.json\""
        } else {
            "printf ' ' >>\"$HOME/phase6-home/.claude/settings.json\""
        };
        remote(&arguments, command)?;
        let rejected = apply_hooks_expect_error(
            &mut connection,
            adapter_id,
            adapter_kind,
            v1::HookManagementAction::Uninstall,
            &stale.confirmation_token,
        )?;
        if !rejected.display_message.contains("stale")
            && !rejected.display_message.contains("changed")
        {
            return Err(format!("{adapter_id} stale review was not rejected"));
        }
        let uninstall = review_hooks(
            &mut connection,
            adapter_id,
            adapter_kind,
            v1::HookManagementAction::Uninstall,
        )?;
        apply_hooks(
            &mut connection,
            adapter_id,
            adapter_kind,
            v1::HookManagementAction::Uninstall,
            &uninstall.confirmation_token,
        )?;
        let removed = review_hooks(
            &mut connection,
            adapter_id,
            adapter_kind,
            v1::HookManagementAction::Uninstall,
        )?;
        if !removed.already_current {
            return Err(format!("{adapter_id} uninstall is not idempotent"));
        }
        hook_evidence.insert(
            adapter_id.into(),
            serde_json::json!({
                "reviewed": true, "installed": true, "staleRejected": true,
                "idempotent": true, "uninstalled": true,
                "backup": review.backup_path,
            }),
        );
    }

    let (launched, _) = agent_action_retry(
        &mut connection,
        v1::AgentActionKind::LaunchWindow,
        "codex",
        &initial_pane,
        "",
        v1::AgentPlacementKind::Window,
    )?;
    if launched.pane_id.is_empty() {
        return Err("launch omitted pane identity".into());
    }
    let codex_pane = launched.pane_id;
    drop(connection);
    std::thread::sleep(std::time::Duration::from_millis(800));

    let mut connection = Connection::open(&arguments, epoch + 1)?;
    let detected = wait_for_agent(&mut connection, |agent| {
        agent
            .route
            .as_ref()
            .is_some_and(|route| route.pane_id == codex_pane)
    })?;
    if !detected.detected_manually || detected.lifecycle != v1::AgentLifecycleState::Unknown as i32
    {
        return Err("manual Codex process detection was not conservative".into());
    }

    let working = ingest(
        &mut connection,
        hook(
            ("codex-working", 1, true),
            v1::AgentAdapterKind::Codex,
            "codex",
            "UserPromptSubmit",
            "codex-native-1",
            &codex_pane,
            &server_identity,
        ),
    )?;
    if working.lifecycle != v1::AgentLifecycleState::Working as i32 {
        return Err("working hook not authoritative".into());
    }
    let blocked = ingest(
        &mut connection,
        hook(
            ("codex-blocked", 2, true),
            v1::AgentAdapterKind::Codex,
            "codex",
            "PermissionRequest",
            "codex-native-1",
            &codex_pane,
            &server_identity,
        ),
    )?;
    if blocked.lifecycle != v1::AgentLifecycleState::Blocked as i32
        || blocked.attention_generation == 0
    {
        return Err("blocked attention missing".into());
    }
    let duplicate = connection.request_error(hook_request(hook(
        ("codex-blocked", 2, true),
        v1::AgentAdapterKind::Codex,
        "codex",
        "PermissionRequest",
        "codex-native-1",
        &codex_pane,
        &server_identity,
    )))?;
    if duplicate.hook_ingest_disposition != v1::HookIngestDisposition::Discarded as i32 {
        return Err("duplicate hook accepted".into());
    }
    let out_of_order = connection.request_error(hook_request(hook(
        ("codex-old", 1, true),
        v1::AgentAdapterKind::Codex,
        "codex",
        "Stop",
        "codex-native-1",
        &codex_pane,
        &server_identity,
    )))?;
    if out_of_order.hook_ingest_disposition != v1::HookIngestDisposition::Discarded as i32 {
        return Err("out-of-order hook accepted".into());
    }
    let working = ingest(
        &mut connection,
        hook(
            ("codex-working-after-blocked", 3, true),
            v1::AgentAdapterKind::Codex,
            "codex",
            "UserPromptSubmit",
            "codex-native-1",
            &codex_pane,
            &server_identity,
        ),
    )?;
    if working.lifecycle != v1::AgentLifecycleState::Working as i32 {
        return Err("working phase after blocked was not restored".into());
    }
    let done = ingest(
        &mut connection,
        hook(
            ("codex-done", 4, true),
            v1::AgentAdapterKind::Codex,
            "codex",
            "Stop",
            "codex-native-1",
            &codex_pane,
            &server_identity,
        ),
    )?;
    if done.lifecycle != v1::AgentLifecycleState::Idle as i32 || done.attention_kind != "completed"
    {
        return Err("completion generation missing".into());
    }

    let seen = connection
        .request(v1::Request {
            operation: v1::Operation::AgentMarkSeen.into(),
            agent: Some(v1::AgentRequest {
                agent_id: done.agent_id.clone(),
                attention_generation: done.attention_generation,
                ..Default::default()
            }),
            ..Default::default()
        })?
        .agent
        .and_then(|value| value.agent)
        .ok_or("mark seen omitted agent")?;
    if seen.seen_generation != done.attention_generation {
        return Err("exact seen generation not applied".into());
    }
    let renamed = connection
        .request(v1::Request {
            operation: v1::Operation::AgentAction.into(),
            agent: Some(v1::AgentRequest {
                action: v1::AgentActionKind::Rename.into(),
                agent_id: done.agent_id.clone(),
                display_name: "Remote build agent".into(),
                ..Default::default()
            }),
            ..Default::default()
        })?
        .agent
        .and_then(|value| value.agent)
        .ok_or("rename omitted agent")?;
    if renamed.display_name != "Remote build agent" {
        return Err("rename failed".into());
    }

    let (resumed, _) = agent_action_retry(
        &mut connection,
        v1::AgentActionKind::Resume,
        "codex",
        &initial_pane,
        "codex-native-1",
        v1::AgentPlacementKind::Window,
    )?;
    if resumed.pane_id.is_empty() || resumed.pane_id == codex_pane {
        return Err("resume did not create a distinct pane".into());
    }
    drop(connection);
    std::thread::sleep(std::time::Duration::from_millis(600));

    let mut connection = Connection::open(&arguments, epoch + 2)?;
    let (claude, current) = agent_action_retry(
        &mut connection,
        v1::AgentActionKind::LaunchSplit,
        "claude-code",
        &initial_pane,
        "",
        v1::AgentPlacementKind::Split,
    )?;
    let original_window = current
        .panes
        .iter()
        .find(|pane| pane.id == initial_pane)
        .ok_or("split target disappeared")?
        .window_id
        .clone();
    if claude.pane_id.is_empty() || claude.window_id != original_window {
        return Err("split did not inherit active window/root".into());
    }

    let exact = ingest(
        &mut connection,
        hook(
            ("exact-route", 0, false),
            v1::AgentAdapterKind::ClaudeCode,
            "claude-code",
            "Notification",
            "exact-route",
            &initial_pane,
            &current.server_identity,
        ),
    )?;
    if exact
        .route
        .as_ref()
        .is_none_or(|route| route.pane_id != initial_pane)
    {
        return Err("exact direct route incorrect".into());
    }
    let unmapped = ingest(
        &mut connection,
        hook(
            ("foreign-unmapped", 0, false),
            v1::AgentAdapterKind::ClaudeCode,
            "claude-code",
            "Notification",
            "foreign-unmapped",
            "%902",
            &current.server_identity,
        ),
    )?;
    if unmapped
        .route
        .as_ref()
        .is_none_or(|route| !route.pane_id.is_empty())
    {
        return Err("foreign hook route invented navigation".into());
    }

    println!(
        "{}",
        serde_json::json!({
            "privateUnixSocket": true,
            "adapterRegistry": true,
            "hooks": hook_evidence,
            "manualDetection": true,
            "launchWindow": true,
            "launchSplit": true,
            "activeRootInherited": true,
            "resumeIdentity": "codex-native-1",
            "workingBlockedDone": true,
            "duplicateRejected": true,
            "outOfOrderRejected": true,
            "seenExact": true,
            "rename": renamed.display_name,
            "exactDirectRoute": true,
            "foreignUnmapped": true,
            "serverIdentity": current.server_identity,
            "connectionEpoch": connection.epoch.to_string(),
        })
    );
    Ok(())
}

fn review_hooks(
    connection: &mut Connection,
    adapter_id: &str,
    adapter: v1::AgentAdapterKind,
    target: v1::HookManagementAction,
) -> Result<v1::HookManagementPlan, String> {
    connection
        .request(v1::Request {
            operation: v1::Operation::AgentHookManagement.into(),
            agent: Some(v1::AgentRequest {
                adapter: adapter.into(),
                adapter_id: adapter_id.into(),
                hook_management: v1::HookManagementAction::Review.into(),
                hook_management_target: target.into(),
                ..Default::default()
            }),
            ..Default::default()
        })?
        .agent
        .and_then(|value| value.hook_plan)
        .ok_or("hook review omitted plan".into())
}

fn apply_hooks(
    connection: &mut Connection,
    adapter_id: &str,
    adapter: v1::AgentAdapterKind,
    action: v1::HookManagementAction,
    token: &str,
) -> Result<v1::HookManagementPlan, String> {
    connection
        .request(v1::Request {
            operation: v1::Operation::AgentHookManagement.into(),
            agent: Some(v1::AgentRequest {
                adapter: adapter.into(),
                adapter_id: adapter_id.into(),
                hook_management: action.into(),
                confirmed: true,
                confirmation_token: token.into(),
                ..Default::default()
            }),
            ..Default::default()
        })?
        .agent
        .and_then(|value| value.hook_plan)
        .ok_or("hook apply omitted plan".into())
}

fn apply_hooks_expect_error(
    connection: &mut Connection,
    adapter_id: &str,
    adapter: v1::AgentAdapterKind,
    action: v1::HookManagementAction,
    token: &str,
) -> Result<v1::Response, String> {
    connection.request_error(v1::Request {
        operation: v1::Operation::AgentHookManagement.into(),
        agent: Some(v1::AgentRequest {
            adapter: adapter.into(),
            adapter_id: adapter_id.into(),
            hook_management: action.into(),
            confirmed: true,
            confirmation_token: token.into(),
            ..Default::default()
        }),
        ..Default::default()
    })
}

fn agent_action_retry(
    connection: &mut Connection,
    action: v1::AgentActionKind,
    adapter_id: &str,
    target_pane_id: &str,
    native_session_id: &str,
    placement: v1::AgentPlacementKind,
) -> Result<(v1::AgentResponse, v1::Snapshot), String> {
    for _ in 0..10 {
        let snapshot = connection.subscribe()?;
        let pane = snapshot
            .panes
            .iter()
            .find(|pane| pane.id == target_pane_id)
            .ok_or("agent launch target pane disappeared")?;
        let root = connection.active_root(&snapshot, target_pane_id)?;
        let request = v1::Request {
            operation: v1::Operation::AgentAction.into(),
            agent: Some(v1::AgentRequest {
                action: action.into(),
                adapter_id: adapter_id.into(),
                native_session_id: native_session_id.into(),
                session_id: pane.session_id.clone(),
                window_id: pane.window_id.clone(),
                pane_id: pane.id.clone(),
                active_root: root.root,
                root_token: root.root_token,
                expected_server_identity: snapshot.server_identity.clone(),
                expected_topology_generation: snapshot.generation,
                placement: placement.into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        match connection.request(request) {
            Ok(response) => {
                return Ok((
                    response.agent.ok_or("agent action response omitted")?,
                    snapshot,
                ));
            }
            Err(error) if error.contains("topology generation changed") => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(error) => return Err(error),
        }
    }
    Err("agent action topology never stabilized".into())
}

fn hook(
    (source_id, sequence, authoritative): (&str, u64, bool),
    adapter: v1::AgentAdapterKind,
    adapter_id: &str,
    event: &str,
    native: &str,
    pane: &str,
    origin_server_identity: &str,
) -> v1::AgentHookEvent {
    let mut payload = serde_json::json!({
        "hook_event_name": event,
        "notification_type": "permission_prompt",
        "session_id": native,
    });
    if adapter == v1::AgentAdapterKind::Codex {
        // Sequence 1/2 is one user-reviewed turn; 3/4 is the next. Codex
        // permission events without a turn/reviewer are intentionally
        // conservative and stay Working, so the fixture must carry the same
        // normalized evidence as the installed hook.
        payload["approval_turn_id"] =
            format!("phase6-turn-{}", if sequence <= 2 { 1 } else { 2 }).into();
        payload["approval_reviewer"] = "user".into();
    }
    v1::AgentHookEvent {
        adapter: adapter.into(),
        adapter_id: adapter_id.into(),
        source_event_id: source_id.into(),
        source_generation: sequence,
        source_sequence_authoritative: authoritative,
        native_session_id: native.into(),
        pane_id: pane.into(),
        origin_server_identity: origin_server_identity.into(),
        payload_json: serde_json::to_vec(&payload).unwrap(),
        occurred_at_unix_millis: now(),
    }
}

fn hook_request(event: v1::AgentHookEvent) -> v1::Request {
    v1::Request {
        operation: v1::Operation::AgentHookIngest.into(),
        agent: Some(v1::AgentRequest {
            hook_event: Some(event),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn ingest(
    connection: &mut Connection,
    event: v1::AgentHookEvent,
) -> Result<v1::AgentRecord, String> {
    connection
        .request(hook_request(event))?
        .agent
        .and_then(|value| value.agent)
        .ok_or("hook ingest omitted agent".into())
}

fn wait_for_agent(
    connection: &mut Connection,
    predicate: impl Fn(&v1::AgentRecord) -> bool,
) -> Result<v1::AgentRecord, String> {
    for _ in 0..30 {
        if let Some(agent) = connection
            .agent_snapshot()?
            .agents
            .into_iter()
            .find(&predicate)
        {
            return Ok(agent);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err("timed out waiting for manual agent detection".into())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn remote(arguments: &[String], command: &str) -> Result<(), String> {
    let status = Command::new("ssh")
        .arg("-F")
        .arg(&arguments[2])
        .arg(&arguments[3])
        .arg(command)
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("remote command failed: {command}"))
    }
}

fn spawn_bridge(arguments: &[String]) -> Result<Child, String> {
    Command::new("ssh")
        .arg("-F")
        .arg(&arguments[2])
        .arg("-T")
        .arg(&arguments[3])
        .arg(format!(
            "{REMOTE_ENV} $HOME/.local/bin/muxflow-host bridge --stdio"
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| error.to_string())
}

fn handshake(
    stdin: &mut ChildStdin,
    reader: &mut BufReader<ChildStdout>,
    epoch: u64,
) -> Result<(), String> {
    write_frame_sync(
        stdin,
        &envelope(
            1,
            0,
            Payload::ClientHello(v1::ClientHello {
                desktop_version: "phase6-driver".into(),
                requested_capabilities: CAP_AGENTS,
                expected_helper_version: HELPER_VERSION.into(),
                connection_epoch: epoch,
                ..Default::default()
            }),
        ),
    )
    .map_err(|error| error.to_string())?;
    let frame = read_frame_sync(reader)
        .map_err(|error| error.to_string())?
        .ok_or("bridge closed during handshake")?;
    let Some(Payload::ServerHello(hello)) = frame.payload else {
        return Err("missing ServerHello".into());
    };
    if hello.read_only || hello.connection_epoch != epoch || hello.capabilities & CAP_AGENTS == 0 {
        return Err(format!("handshake rejected: {}", hello.incompatibility));
    }
    Ok(())
}
