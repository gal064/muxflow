use super::*;
use prost::Message;
use std::fs;

#[path = "tests/fallback.rs"]
mod fallback_tests;
mod lifecycle;

fn runtime(name: &str) -> AgentRuntime {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase6-agent-{name}-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    AgentRuntime::isolated(path)
}

fn event(id: &str, generation: u64, name: &str) -> v1::AgentHookEvent {
    let mut payload = serde_json::json!({"hook_event_name": name});
    if name == "PermissionRequest" {
        payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "user".into();
        payload[adapters::CODEX_APPROVAL_TURN_ID_FIELD] = format!("turn-{id}").into();
    }
    v1::AgentHookEvent {
        adapter: v1::AgentAdapterKind::Codex.into(),
        source_event_id: id.into(),
        source_generation: generation,
        native_session_id: "native-1".into(),
        pane_id: "%7".into(),
        payload_json: serde_json::to_vec(&payload).unwrap(),
        occurred_at_unix_millis: now_millis(),
        origin_server_identity: "server-a".into(),
        ..Default::default()
    }
}

fn event_for_turn(id: &str, name: &str, turn_id: &str) -> v1::AgentHookEvent {
    let mut value = event(id, 0, name);
    let mut payload: serde_json::Value = serde_json::from_slice(&value.payload_json).unwrap();
    payload[adapters::CODEX_APPROVAL_TURN_ID_FIELD] = turn_id.into();
    value.payload_json = serde_json::to_vec(&payload).unwrap();
    value
}

fn root_turn(index: u16) -> String {
    format!("00000000-{index:04x}-7000-8000-000000000000")
}

fn child_event_for_turn(id: &str, name: &str, child_id: &str, turn_id: &str) -> v1::AgentHookEvent {
    let mut value = event_for_turn(id, name, turn_id);
    let mut payload: serde_json::Value = serde_json::from_slice(&value.payload_json).unwrap();
    payload[adapters::CODEX_SUBAGENT_ID_FIELD] = child_id.into();
    value.payload_json = serde_json::to_vec(&payload).unwrap();
    value
}

fn topology(command: &str) -> tmux_control::TmuxSnapshot {
    tmux_control::TmuxSnapshot {
        sessions: vec![tmux_control::Session {
            id: "$1".into(),
            name: "workspace".into(),
            window_count: 1,
            attached_clients: 0,
            order: 0,
            pinned: false,
        }],
        windows: vec![tmux_control::Window {
            id: "@2".into(),
            session_id: "$1".into(),
            index: 0,
            name: "agent".into(),
            active: true,
            layout: String::new(),
            zoomed: false,
            pinned: false,
        }],
        panes: vec![tmux_control::Pane {
            id: "%7".into(),
            session_id: "$1".into(),
            window_id: "@2".into(),
            index: 0,
            active: true,
            width: 80,
            height: 24,
            left: 0,
            top: 0,
            current_path: "/work".into(),
            current_command: command.into(),
            pane_pid: 0,
            start_command: String::new(),
        }],
    }
}

fn two_pane_topology() -> tmux_control::TmuxSnapshot {
    let mut value = topology("codex");
    let mut second = value.panes[0].clone();
    second.id = "%8".into();
    second.index = 1;
    second.left = 80;
    value.panes.push(second);
    value
}

#[test]
fn hook_generations_dedupe_and_attention_are_monotonic() {
    let runtime = runtime("transitions");
    let working = runtime
        .ingest_hook(&event("e1", 1, "UserPromptSubmit"))
        .unwrap();
    assert!(!working.notify);
    let done = runtime.ingest_hook(&event("e2", 2, "Stop")).unwrap();
    assert!(done.notify);
    let record = done.agent.unwrap();
    assert_eq!(record.attention_generation, 1);
    assert_eq!(record.attention_kind, "completed");
    assert_eq!(record.seen_generation, 0);
    assert!(matches!(
        runtime.ingest_hook(&event("e2", 2, "Stop")),
        Err(HookIngestFailure::Duplicate)
    ));
    assert!(
        runtime
            .ingest_hook(&event("older-sequence", 1, "Stop"))
            .is_ok()
    );
    let seen = runtime.mark_seen(&record.agent_id, 1).unwrap();
    assert_eq!(seen.reason, "seen");
    assert!(!seen.notify);
    assert!(seen.generation > done.generation);
    let first_seen_at = seen.agent.unwrap().attention_seen_at_unix_millis;
    assert!(first_seen_at > 0);
    let repeated = runtime.mark_seen(&record.agent_id, 1).unwrap();
    assert_eq!(
        repeated.agent.unwrap().attention_seen_at_unix_millis,
        first_seen_at,
        "reading the same generation twice must not restart Recent"
    );
    let snapshot = runtime.snapshot();
    assert!(snapshot.authoritative);
    assert_eq!(snapshot.notification_watermark, snapshot.generation);
    assert_eq!(snapshot.agents[0].seen_generation, 1);
    let next_turn = runtime
        .ingest_hook(&event("e3", 3, "UserPromptSubmit"))
        .unwrap()
        .agent
        .unwrap();
    assert!(next_turn.attention_kind.is_empty());
    assert_eq!(next_turn.attention_seen_at_unix_millis, 0);
    let next_done = runtime.ingest_hook(&event("e4", 4, "Stop")).unwrap();
    assert_eq!(
        next_done.agent.unwrap().attention_seen_at_unix_millis,
        0,
        "a new unread generation must not inherit the prior read time"
    );
}

#[test]
fn lifecycle_change_time_ignores_same_status_hooks_and_advances_on_transition() {
    let runtime = runtime("lifecycle-change-time");
    let topology = topology("codex");
    let working = runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    {
        let mut state = runtime.state.lock().unwrap();
        state
            .agents
            .get_mut(&working.agent_id)
            .unwrap()
            .lifecycle_changed_at_unix_millis = 123;
    }

    let still_working = runtime
        .ingest_hook_with_context(&event("tool", 0, "PreToolUse"), "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(
        still_working.lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    assert_eq!(still_working.lifecycle_changed_at_unix_millis, 123);

    let completed = runtime
        .ingest_hook_with_context(&event("stop", 0, "Stop"), "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(completed.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert!(completed.lifecycle_changed_at_unix_millis > 123);
}

#[test]
fn rename_returns_a_canonical_published_event_generation() {
    let runtime = runtime("rename-event");
    let topology_snapshot = topology("codex");
    runtime
        .reconcile_topology(&topology_snapshot, "server-a")
        .unwrap();
    let before = runtime.snapshot_for("server-a");
    let renamed = runtime
        .rename(&before.agents[0].agent_id, "Build agent")
        .unwrap();
    assert_eq!(renamed.reason, "renamed");
    assert!(!renamed.notify);
    assert!(renamed.generation > before.generation);
    assert_eq!(renamed.agent.unwrap().display_name, "Build agent");
}

#[test]
fn title_animation_does_not_churn_routes_but_process_changes_still_reconcile() {
    let runtime = runtime("title-animation-reconciliation");
    let mut first = topology("codex");
    first.windows[0].name = "✳ Fix tests".into();
    assert!(runtime.reconcile_topology(&first, "server-a").unwrap());
    let initial = runtime.snapshot_for("server-a");
    assert_eq!(initial.agents.len(), 1);
    let initial_generation = initial.generation;
    let initial_route = initial.agents[0].route.as_ref().unwrap().clone();

    let mut animated = first.clone();
    animated.windows[0].name = "⠋ Fix tests".into();
    assert!(!runtime.reconcile_topology(&animated, "server-a").unwrap());
    let after_animation = runtime.snapshot_for("server-a");
    assert_eq!(after_animation.generation, initial_generation);
    assert_eq!(
        after_animation.agents[0].route.as_ref(),
        Some(&initial_route)
    );

    let mut renamed = animated.clone();
    renamed.windows[0].name = "✓ Ship tests".into();
    assert!(runtime.reconcile_topology(&renamed, "server-a").unwrap());
    let after_rename = runtime.snapshot_for("server-a");
    assert!(after_rename.generation > initial_generation);
    assert_eq!(
        after_rename.agents[0]
            .route
            .as_ref()
            .unwrap()
            .window_name_fallback,
        "✓ Ship tests"
    );

    let mut departed = renamed;
    departed.windows[0].name = "⠦ Ship tests".into();
    departed.panes[0].current_command = "bash".into();
    assert!(!runtime.reconcile_topology(&departed, "server-a").unwrap());
    assert_eq!(runtime.snapshot_for("server-a").agents.len(), 1);
    assert!(
        runtime
            .retire_departed_from(&departed, "server-a")
            .is_empty()
    );
    assert!(
        runtime
            .retire_departed_from(&departed, "server-a")
            .is_empty()
    );
    assert_eq!(runtime.retire_departed_from(&departed, "server-a").len(), 1);
    assert!(runtime.snapshot_for("server-a").agents.is_empty());
}

#[test]
fn persist_failure_rolls_back_runtime_mutations() {
    let seeded = runtime("transaction-seed");
    let topology_snapshot = topology("codex");
    seeded
        .reconcile_topology(&topology_snapshot, "server-a")
        .unwrap();
    let baseline_state = seeded.state.lock().unwrap().clone();
    let blocker = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase6-state-blocker-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(blocker.parent().unwrap()).unwrap();
    fs::write(&blocker, b"not a directory").unwrap();
    let failing = || AgentRuntime {
        state_path: blocker.join("agents.json"),
        ingest_order: Mutex::new(()),
        state: Mutex::new(baseline_state.clone()),
        wiring: Mutex::new(hooks::WiringCache::default()),
        departure_misses: Mutex::new(BTreeMap::new()),
        pending_codex_permissions: Mutex::new(BTreeMap::new()),
        codex_child_monitors: Mutex::new(BTreeMap::new()),
        reply_sink: Box::new(|_| {}),
    };
    let agent_id = baseline_state.agents.keys().next().unwrap().clone();

    let runtime = failing();
    assert!(runtime.rename(&agent_id, "must rollback").is_err());
    assert_eq!(
        runtime.state.lock().unwrap().generation,
        baseline_state.generation
    );

    let runtime = failing();
    assert!(runtime.mark_seen(&agent_id, 0).is_err());
    assert_eq!(
        runtime.state.lock().unwrap().generation,
        baseline_state.generation
    );

    let runtime = failing();
    assert!(
        runtime
            .reconcile_topology(&topology("claude"), "server-a")
            .is_err()
    );
    let state = runtime.state.lock().unwrap();
    assert_eq!(state.generation, baseline_state.generation);
    assert_eq!(
        state.agents.keys().collect::<Vec<_>>(),
        baseline_state.agents.keys().collect::<Vec<_>>()
    );
    drop(state);

    let runtime = failing();
    assert!(matches!(
        runtime.ingest_hook_with_context(
            &event("transaction-hook", 0, "PermissionRequest"),
            "server-a",
            Some(&topology_snapshot),
        ),
        Err(HookIngestFailure::Retryable(_))
    ));
    assert_eq!(
        runtime.state.lock().unwrap().generation,
        baseline_state.generation
    );
}

#[test]
fn snapshot_exposes_raw_records_for_frontend_rollups() {
    let runtime = runtime("raw-records");
    let mut topology = topology("codex");
    let mut second_pane = topology.panes[0].clone();
    second_pane.id = "%8".into();
    topology.panes.push(second_pane);
    runtime.reconcile_topology(&topology, "server-a").unwrap();
    runtime
        .ingest_hook_with_context(
            &event("e1", 1, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let mut second = event("e2", 1, "PermissionRequest");
    second.native_session_id = "native-2".into();
    second.pane_id = "%8".into();
    runtime
        .ingest_hook_with_context(&second, "server-a", Some(&topology))
        .unwrap();
    let snapshot = runtime.snapshot_for("server-a");
    assert_eq!(snapshot.agents.len(), 2);
    assert!(
        snapshot
            .agents
            .iter()
            .any(|agent| { agent.lifecycle == v1::AgentLifecycleState::Blocked as i32 })
    );
    assert!(
        snapshot
            .agents
            .iter()
            .any(|agent| { agent.lifecycle == v1::AgentLifecycleState::Working as i32 })
    );
}

/// Process detection proves the agent exists; it never overwrites what a hook
/// said the agent was doing. Reconciliation runs over a pane a hook has already
/// claimed, and the blocked state has to come through it intact.
#[test]
fn process_reconciliation_never_overwrites_a_hook_established_lifecycle() {
    let runtime = runtime("authority");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("e1", 1, "PermissionRequest"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime.reconcile_topology(&topology, "server-a").unwrap();
    let snapshot = runtime.snapshot_for("server-a");
    assert_eq!(snapshot.agents.len(), 1);
    let record = &snapshot.agents[0];
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    assert!(
        record.hook_authority_expires_at_unix_millis > 0,
        "the hook lease is what reconciliation reads to leave the record alone"
    );
}

#[test]
fn process_exit_debounces_while_pane_close_and_adapter_replacement_reconcile() {
    let runtime = runtime("retire");
    runtime
        .reconcile_topology(&topology("codex"), "server-a")
        .unwrap();
    let codex_id = runtime.snapshot_for("server-a").agents[0].agent_id.clone();

    runtime
        .reconcile_topology(&topology("claude"), "server-a")
        .unwrap();
    let replaced = runtime.snapshot_for("server-a");
    assert_eq!(replaced.agents.len(), 1);
    assert_ne!(replaced.agents[0].agent_id, codex_id);
    assert_eq!(replaced.agents[0].adapter_id, "claude-code");

    let departed = topology("bash");
    assert!(!runtime.reconcile_topology(&departed, "server-a").unwrap());
    assert_eq!(runtime.snapshot_for("server-a").agents.len(), 1);
    assert!(
        runtime
            .retire_departed_from(&departed, "server-a")
            .is_empty()
    );
    assert!(
        runtime
            .retire_departed_from(&departed, "server-a")
            .is_empty()
    );
    assert_eq!(runtime.retire_departed_from(&departed, "server-a").len(), 1);
    assert!(runtime.snapshot_for("server-a").agents.is_empty());

    runtime
        .reconcile_topology(&topology("codex"), "server-a")
        .unwrap();
    runtime
        .reconcile_topology(&tmux_control::TmuxSnapshot::default(), "server-a")
        .unwrap();
    assert!(runtime.snapshot_for("server-a").agents.is_empty());
}

#[test]
fn server_replacement_filters_and_retires_foreign_records() {
    let runtime = runtime("server-replace");
    runtime
        .reconcile_topology(&topology("codex"), "server-a")
        .unwrap();
    assert_eq!(runtime.snapshot_for("server-a").agents.len(), 1);
    assert!(runtime.snapshot_for("server-b").agents.is_empty());
    runtime
        .reconcile_topology(&topology("codex"), "server-b")
        .unwrap();
    assert!(runtime.snapshot_for("server-a").agents.is_empty());
    assert_eq!(runtime.snapshot_for("server-b").agents.len(), 1);
}

#[test]
fn hook_atomically_promotes_manual_pane_identity_without_duplicates() {
    let runtime = runtime("promotion");
    let topology = topology("codex");
    runtime.reconcile_topology(&topology, "server-a").unwrap();
    let manual_id = runtime.snapshot_for("server-a").agents[0].agent_id.clone();
    let promoted = runtime
        .ingest_hook_with_context(
            &event("hook-1", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let snapshot = runtime.snapshot_for("server-a");
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].native_session_id, "native-1");
    assert_ne!(snapshot.agents[0].agent_id, manual_id);
    assert_eq!(promoted.retired_agent_ids, [manual_id]);
    assert_eq!(snapshot.agents[0].route.as_ref().unwrap().pane_id, "%7");
}

#[test]
fn native_session_replacement_suspends_displaced_turn_children_and_sidecars() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!(
            "phase6-agent-session-replacement-{}",
            uuid::Uuid::new_v4()
        ))
        .join("agents.json");
    let replies = Arc::new(Mutex::new(Vec::new()));
    let reply_calls = Arc::clone(&replies);
    let runtime = AgentRuntime::isolated_with_sink(
        path,
        Box::new(move |reply| reply_calls.lock().unwrap().push(reply)),
    );
    let topology = topology("codex");
    let turn_a = root_turn(1);
    let child_turn = root_turn(2);
    let root = runtime
        .ingest_hook_with_context(
            &event_for_turn("prompt-a", "UserPromptSubmit", &turn_a),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &child_event_for_turn("child-a", "SubagentStart", "child", &child_turn),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &event_for_turn("stop-a", "Stop", &turn_a),
            "server-a",
            Some(&topology),
        )
        .unwrap();

    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/13");
    fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    fs::write(&transcript, b"\n").unwrap();
    let open_monitor = |turn_id: &str| {
        crate::hook::codex_transcript::TurnMonitor::open(
            &serde_json::json!({
                "turn_id": turn_id,
                "transcript_path": transcript,
            }),
            home.path(),
        )
        .unwrap()
    };
    let child_key = CodexTurnKey {
        agent_id: "child".into(),
        turn_id: child_turn.clone(),
    };
    runtime.codex_child_monitors.lock().unwrap().insert(
        (root.agent_id.clone(), "child".into()),
        CodexChildMonitor {
            turn: child_key,
            monitor: open_monitor(&child_turn),
            terminal: None,
        },
    );
    let changed_at =
        runtime.state.lock().unwrap().agents[&root.agent_id].lifecycle_changed_at_unix_millis;
    let permission_key = CodexTurnKey {
        agent_id: String::new(),
        turn_id: turn_a.clone(),
    };
    runtime.pending_codex_permissions.lock().unwrap().insert(
        PendingCodexPermissionKey {
            record_id: root.agent_id.clone(),
            turn: permission_key,
        },
        PendingCodexPermission {
            monitor: open_monitor(&turn_a),
            terminal: None,
            lifecycle_changed_at_unix_millis: changed_at,
            observed_at_unix_millis: now_millis(),
            root_turn_id: turn_a.clone(),
        },
    );

    let turn_b = root_turn(3);
    let mut replacement = event_for_turn("prompt-b", "UserPromptSubmit", &turn_b);
    replacement.native_session_id = "native-2".into();
    let replaced = runtime
        .ingest_hook_with_context(&replacement, "server-a", None)
        .unwrap();
    assert_eq!(
        replaced.retired_agent_ids.as_slice(),
        std::slice::from_ref(&root.agent_id)
    );
    let replacement_id = replaced.agent.unwrap().agent_id;
    {
        let state = runtime.state.lock().unwrap();
        let suspended = state
            .codex_predecessors
            .get(&replacement_id)
            .expect("the displaced session is retained behind its replacement");
        assert_eq!(suspended.agent_id, root.agent_id);
        assert_eq!(suspended.codex_active_root_turn_id, turn_a);
        assert_eq!(
            suspended.codex_running_subagents.get("child"),
            Some(&child_turn)
        );
    }
    assert!(
        runtime
            .pending_codex_permissions
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.record_id == root.agent_id)
    );
    assert!(
        runtime
            .codex_child_monitors
            .lock()
            .unwrap()
            .keys()
            .any(|(record_id, _)| record_id == &root.agent_id)
    );
    let replacement_generation = runtime.snapshot_for("server-a").generation;

    let mut late_root = event_for_turn("late-stop-a-outage", "Stop", &turn_a);
    let mut late_payload: serde_json::Value =
        serde_json::from_slice(&late_root.payload_json).unwrap();
    late_payload[adapters::LAST_ASSISTANT_MESSAGE_FIELD] = "stale reply".into();
    late_root.payload_json = serde_json::to_vec(&late_payload).unwrap();
    assert!(matches!(
        runtime.ingest_hook_with_context(&late_root, "server-a", None),
        Err(HookIngestFailure::Superseded)
    ));
    let replacement_snapshot = runtime.snapshot_for("server-a");
    assert_eq!(replacement_snapshot.generation, replacement_generation);
    assert_eq!(replacement_snapshot.agents.len(), 1);
    assert_eq!(replacement_snapshot.agents[0].agent_id, replacement_id);
    assert_eq!(
        replacement_snapshot.agents[0].lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    assert_eq!(replies.lock().unwrap()[0].text, "stale reply");

    let mut unmapped_activity = event_for_turn("activity-b-unmapped", "PreToolUse", &turn_b);
    unmapped_activity.native_session_id = "native-2".into();
    let continued = runtime
        .ingest_hook_with_context(&unmapped_activity, "server-a", None)
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(continued.agent_id, replacement_id);
    assert!(continued.route.unwrap().pane_id.is_empty());
    let permission_turn = root_turn(99);
    runtime.pending_codex_permissions.lock().unwrap().insert(
        PendingCodexPermissionKey {
            record_id: replacement_id.clone(),
            turn: CodexTurnKey {
                agent_id: String::new(),
                turn_id: permission_turn.clone(),
            },
        },
        PendingCodexPermission {
            monitor: open_monitor(&permission_turn),
            terminal: None,
            lifecycle_changed_at_unix_millis: changed_at,
            observed_at_unix_millis: now_millis(),
            root_turn_id: permission_turn,
        },
    );
    runtime.codex_child_monitors.lock().unwrap().insert(
        (replacement_id.clone(), "retained-child".into()),
        CodexChildMonitor {
            turn: CodexTurnKey {
                agent_id: "retained-child".into(),
                turn_id: child_turn.clone(),
            },
            monitor: open_monitor(&child_turn),
            terminal: None,
        },
    );

    let mut mapped_activity = event_for_turn("activity-b-mapped", "PostToolUse", &turn_b);
    mapped_activity.native_session_id = "native-2".into();
    let recovered_id = runtime
        .ingest_hook_with_context(&mapped_activity, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap()
        .agent_id;
    assert_eq!(recovered_id, replacement_id);
    assert!(
        runtime
            .pending_codex_permissions
            .lock()
            .unwrap()
            .keys()
            .any(|key| key.record_id == root.agent_id)
    );
    assert!(
        runtime
            .codex_child_monitors
            .lock()
            .unwrap()
            .keys()
            .any(|(record_id, _)| record_id == &root.agent_id)
    );

    let mut replacement_stop = event_for_turn("stop-b", "Stop", &turn_b);
    replacement_stop.native_session_id = "native-2".into();
    let completed = runtime
        .ingest_hook_with_context(&replacement_stop, "server-a", Some(&topology))
        .unwrap();
    assert!(completed.notify);
    assert_eq!(
        completed.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
    let generation = runtime.snapshot_for("server-a").generation;

    let late_child = child_event_for_turn("late-child-a", "SubagentStop", "child", &child_turn);
    assert!(matches!(
        runtime.ingest_hook_with_context(&late_child, "server-a", Some(&topology)),
        Err(HookIngestFailure::Superseded)
    ));
    let snapshot = runtime.snapshot_for("server-a");
    assert_eq!(snapshot.generation, generation);
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].agent_id, replacement_id);
    assert_eq!(
        snapshot.agents[0].lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
    let state = runtime.state.lock().unwrap();
    let record = &state.agents[&replacement_id];
    assert!(record.codex_running_subagents.is_empty());
    assert_eq!(record.codex_active_root_turn_id, turn_b);
    assert_eq!(replies.lock().unwrap().len(), 1);
}

#[test]
fn outage_replacement_keeps_pane_ownership_across_a_daemon_restart() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("pane-binding-reload-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    let topology = topology("codex");
    let turn_a = root_turn(1);
    let turn_b = root_turn(2);
    let runtime = AgentRuntime::isolated(path.clone());
    runtime
        .ingest_hook_with_context(
            &event_for_turn("reload-a", "UserPromptSubmit", &turn_a),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let mut replacement = event_for_turn("reload-b", "UserPromptSubmit", &turn_b);
    replacement.native_session_id = "native-2".into();
    let replacement_id = runtime
        .ingest_hook_with_context(&replacement, "server-a", None)
        .unwrap()
        .agent
        .unwrap()
        .agent_id;
    drop(runtime);

    let reloaded = AgentRuntime::isolated(path);
    reloaded.reconcile_topology(&topology, "server-a").unwrap();
    assert!(matches!(
        reloaded.ingest_hook_with_context(
            &event_for_turn("late-a", "Stop", &turn_a),
            "server-a",
            None,
        ),
        Err(HookIngestFailure::Superseded)
    ));
    let snapshot = reloaded.snapshot_for("server-a");
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].agent_id, replacement_id);
    assert_eq!(
        snapshot.agents[0].lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
}

#[test]
fn a_known_session_move_keeps_its_state_and_retires_the_displaced_session() {
    let runtime = runtime("known-session-move");
    let topology = two_pane_topology();
    let turn_a = root_turn(1);
    let turn_a_next = root_turn(2);
    let turn_b = root_turn(10);
    let child_b = root_turn(11);
    let sequenced = |id: &str,
                     name: &str,
                     turn: &str,
                     native_session_id: &str,
                     pane_id: &str,
                     generation: u64| {
        let mut hook = event_for_turn(id, name, turn);
        hook.native_session_id = native_session_id.into();
        hook.pane_id = pane_id.into();
        hook.source_sequence_authoritative = true;
        hook.source_generation = generation;
        hook
    };
    let session_a = runtime
        .ingest_hook_with_context(
            &sequenced("prompt-a", "UserPromptSubmit", &turn_a, "native-a", "%7", 5),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    let session_b = runtime
        .ingest_hook_with_context(
            &sequenced(
                "prompt-b",
                "UserPromptSubmit",
                &turn_b,
                "native-b",
                "%8",
                100,
            ),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    let mut child = child_event_for_turn("child-b", "SubagentStart", "child", &child_b);
    child.native_session_id = "native-b".into();
    child.pane_id = "%8".into();
    runtime
        .ingest_hook_with_context(&child, "server-a", Some(&topology))
        .unwrap();

    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/13");
    fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    fs::write(&transcript, b"\n").unwrap();
    let open_monitor = |turn_id: &str| {
        crate::hook::codex_transcript::TurnMonitor::open(
            &serde_json::json!({"turn_id": turn_id, "transcript_path": transcript}),
            home.path(),
        )
        .unwrap()
    };
    runtime.codex_child_monitors.lock().unwrap().insert(
        (session_b.agent_id.clone(), "child".into()),
        CodexChildMonitor {
            turn: CodexTurnKey {
                agent_id: "child".into(),
                turn_id: child_b.clone(),
            },
            monitor: open_monitor(&child_b),
            terminal: None,
        },
    );
    let changed_at =
        runtime.state.lock().unwrap().agents[&session_b.agent_id].lifecycle_changed_at_unix_millis;
    runtime.pending_codex_permissions.lock().unwrap().insert(
        PendingCodexPermissionKey {
            record_id: session_b.agent_id.clone(),
            turn: CodexTurnKey {
                agent_id: String::new(),
                turn_id: turn_b.clone(),
            },
        },
        PendingCodexPermission {
            monitor: open_monitor(&turn_b),
            terminal: None,
            lifecycle_changed_at_unix_millis: changed_at,
            observed_at_unix_millis: now_millis(),
            root_turn_id: turn_b,
        },
    );

    let moved = runtime
        .ingest_hook_with_context(
            &sequenced("move-a", "PreToolUse", &turn_a_next, "native-a", "%8", 6),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(moved.retired_agent_ids.contains(&session_b.agent_id));
    let moved = moved.agent.unwrap();
    assert_eq!(moved.agent_id, session_a.agent_id);
    assert_eq!(moved.route.as_ref().unwrap().pane_id, "%8");
    assert!(runtime.pending_codex_permissions.lock().unwrap().is_empty());
    assert!(runtime.codex_child_monitors.lock().unwrap().is_empty());
    let state = runtime.state.lock().unwrap();
    assert_eq!(state.agents.len(), 1);
    let record = &state.agents[&session_a.agent_id];
    assert_eq!(record.native_session_id, "native-a");
    assert_eq!(record.codex_active_root_turn_id, turn_a_next);
    assert_eq!(record.latest_source_generation, 6);
}

#[test]
fn a_verified_session_move_carries_its_predecessor_chain_to_the_new_pane() {
    let runtime = runtime("codex-session-chain-move");
    let topology = two_pane_topology();
    let parent_turn = root_turn(1);
    let mut parent_prompt = event_for_turn("parent-prompt", "UserPromptSubmit", &parent_turn);
    parent_prompt.pane_id = "%7".into();
    let parent = runtime
        .ingest_hook_with_context(&parent_prompt, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();
    let side_turn = root_turn(2);
    let mut side_prompt = event_for_turn("side-prompt", "UserPromptSubmit", &side_turn);
    side_prompt.native_session_id = "native-side".into();
    side_prompt.pane_id = "%7".into();
    runtime
        .ingest_hook_with_context(&side_prompt, "server-a", Some(&topology))
        .unwrap();
    let mut occupant = event_for_turn("occupant-prompt", "UserPromptSubmit", &root_turn(3));
    occupant.native_session_id = "native-occupant".into();
    occupant.pane_id = "%8".into();
    let occupant_id = runtime
        .ingest_hook_with_context(&occupant, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap()
        .agent_id;

    let mut moved = event_for_turn("side-moved", "PreToolUse", &side_turn);
    moved.native_session_id = "native-side".into();
    moved.pane_id = "%8".into();
    let moved = runtime
        .ingest_hook_with_context(&moved, "server-a", Some(&topology))
        .unwrap();
    assert!(moved.retired_agent_ids.contains(&occupant_id));
    let moved = moved.agent.unwrap();
    assert_eq!(moved.route.unwrap().pane_id, "%8");
    let predecessor_route = runtime.state.lock().unwrap().codex_predecessors[&moved.agent_id]
        .route
        .clone();
    assert_eq!(predecessor_route.pane_id, "%8");

    let mut side_stop = event_for_turn("side-stop", "Stop", &side_turn);
    side_stop.native_session_id = "native-side".into();
    side_stop.pane_id = "%8".into();
    runtime
        .ingest_hook_with_context(&side_stop, "server-a", Some(&topology))
        .unwrap();
    let mut parent_returned = event_for_turn("parent-returned", "PostToolUse", &parent_turn);
    parent_returned.pane_id = "%8".into();
    let parent_returned = runtime
        .ingest_hook_with_context(&parent_returned, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(parent_returned.agent_id, parent.agent_id);
    assert_eq!(parent_returned.route.unwrap().pane_id, "%8");
}

#[test]
fn superseded_codex_fork_hooks_do_not_reclaim_the_current_pane() {
    let runtime = runtime("codex-superseded-fork");
    let topology = topology("codex");

    let root = runtime
        .ingest_hook_with_context(
            &event("root-start", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    let mut fork_start = event("fork-start", 0, "UserPromptSubmit");
    fork_start.native_session_id = "native-fork".into();
    runtime
        .ingest_hook_with_context(&fork_start, "server-a", Some(&topology))
        .unwrap();
    let root_resumed = runtime
        .ingest_hook_with_context(
            &event("root-resumed", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(root_resumed.agent_id, root.agent_id);
    let generation = runtime.snapshot_for("server-a").generation;

    for event_name in ["PreToolUse", "Stop", "Interrupt", "SessionEnd"] {
        let mut late = event(&format!("fork-late-{event_name}"), 0, event_name);
        late.native_session_id = "native-fork".into();
        assert!(matches!(
            runtime.ingest_hook_with_context(&late, "server-a", Some(&topology)),
            Err(HookIngestFailure::Superseded)
        ));
    }

    let snapshot = runtime.snapshot_for("server-a");
    assert_eq!(snapshot.generation, generation);
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].agent_id, root.agent_id);
    assert_eq!(
        snapshot.agents[0].lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
}

#[test]
fn codex_btw_completion_returns_ownership_to_its_running_parent() {
    let runtime = runtime("codex-btw-parent-resume");
    let topology = topology("codex");
    let parent_turn = root_turn(1);
    let side_turn = root_turn(2);
    let parent = runtime
        .ingest_hook_with_context(
            &event_for_turn("parent-prompt", "UserPromptSubmit", &parent_turn),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();

    let mut side_prompt = event_for_turn("side-prompt", "UserPromptSubmit", &side_turn);
    side_prompt.native_session_id = "native-side".into();
    let side = runtime
        .ingest_hook_with_context(&side_prompt, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();
    let mut side_stop = event_for_turn("side-stop", "Stop", &side_turn);
    side_stop.native_session_id = "native-side".into();
    let side_done = runtime
        .ingest_hook_with_context(&side_stop, "server-a", Some(&topology))
        .unwrap();
    assert!(side_done.notify);
    assert_eq!(
        side_done.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );

    let resumed = runtime
        .ingest_hook_with_context(
            &event_for_turn("parent-tool-returned", "PostToolUse", &parent_turn),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(resumed.retired_agent_ids.contains(&side.agent_id));
    let resumed = resumed.agent.unwrap();
    assert_eq!(resumed.agent_id, parent.agent_id);
    assert_eq!(resumed.lifecycle, v1::AgentLifecycleState::Working as i32);

    let done = runtime
        .ingest_hook_with_context(
            &event_for_turn("parent-stop", "Stop", &parent_turn),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(done.notify);
    assert_eq!(done.agent.unwrap().agent_id, parent.agent_id);
    let snapshot = runtime.snapshot_for("server-a");
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].agent_id, parent.agent_id);
    assert_eq!(
        snapshot.agents[0].lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn a_still_open_btw_turn_remains_foreground_and_blocks_parent_reclaim() {
    let runtime = runtime("codex-btw-still-open");
    let topology = topology("codex");
    let parent_turn = root_turn(1);
    let side_turn = root_turn(2);
    runtime
        .ingest_hook_with_context(
            &event_for_turn("parent-prompt", "UserPromptSubmit", &parent_turn),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let mut side_prompt = event_for_turn("side-prompt", "UserPromptSubmit", &side_turn);
    side_prompt.native_session_id = "native-side".into();
    runtime
        .ingest_hook_with_context(&side_prompt, "server-a", Some(&topology))
        .unwrap();
    let mut side_stop = event_for_turn("side-stop", "Stop", &side_turn);
    side_stop.native_session_id = "native-side".into();
    runtime
        .ingest_hook_with_context(&side_stop, "server-a", Some(&topology))
        .unwrap();
    let next_side_turn = root_turn(3);
    let mut next_side = event_for_turn("side-next", "UserPromptSubmit", &next_side_turn);
    next_side.native_session_id = "native-side".into();
    runtime
        .ingest_hook_with_context(&next_side, "server-a", Some(&topology))
        .unwrap();

    assert!(matches!(
        runtime.ingest_hook_with_context(
            &event_for_turn("parent-background", "PostToolUse", &parent_turn),
            "server-a",
            Some(&topology),
        ),
        Err(HookIngestFailure::Superseded)
    ));
}

#[test]
fn child_scoped_explicit_hooks_cannot_restore_a_suspended_parent() {
    for event_name in ["SessionStart", "UserPromptSubmit"] {
        let runtime = runtime(&format!("child-explicit-cannot-resume-{event_name}"));
        let topology = topology("codex");
        runtime
            .ingest_hook_with_context(
                &event_for_turn("parent-prompt", "UserPromptSubmit", &root_turn(1)),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        let mut side = event_for_turn("side-prompt", "UserPromptSubmit", &root_turn(2));
        side.native_session_id = "native-side".into();
        let side_id = runtime
            .ingest_hook_with_context(&side, "server-a", Some(&topology))
            .unwrap()
            .agent
            .unwrap()
            .agent_id;
        let child = child_event_for_turn("parent-child", event_name, "child", &root_turn(3));
        assert!(matches!(
            runtime.ingest_hook_with_context(&child, "server-a", Some(&topology)),
            Err(HookIngestFailure::Superseded)
        ));
        assert_eq!(runtime.snapshot_for("server-a").agents[0].agent_id, side_id);
    }
}

#[test]
fn codex_session_end_restores_the_immediate_predecessor() {
    let runtime = runtime("codex-btw-session-end");
    let topology = topology("codex");
    let parent = runtime
        .ingest_hook_with_context(
            &event_for_turn("parent-prompt", "UserPromptSubmit", &root_turn(1)),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    let mut side = event_for_turn("side-prompt", "UserPromptSubmit", &root_turn(2));
    side.native_session_id = "native-side".into();
    let side_id = runtime
        .ingest_hook_with_context(&side, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap()
        .agent_id;
    let mut side_end = event("side-end", 0, "SessionEnd");
    side_end.native_session_id = "native-side".into();
    let restored = runtime
        .ingest_hook_with_context(&side_end, "server-a", Some(&topology))
        .unwrap();
    assert!(!restored.notify);
    assert_eq!(restored.retired_agent_ids, [side_id]);
    let restored = restored.agent.unwrap();
    assert_eq!(restored.agent_id, parent.agent_id);
    assert_eq!(restored.lifecycle, v1::AgentLifecycleState::Working as i32);
}

#[test]
fn topology_move_refreshes_suspended_routes_before_session_end_restores_them() {
    let runtime = runtime("codex-suspended-topology-move");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("parent-prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let mut side = event("side-prompt", 0, "UserPromptSubmit");
    side.native_session_id = "native-side".into();
    runtime
        .ingest_hook_with_context(&side, "server-a", Some(&topology))
        .unwrap();

    let mut moved = topology;
    moved.sessions[0].id = "$9".into();
    moved.sessions[0].name = "moved-session".into();
    moved.windows[0].id = "@9".into();
    moved.windows[0].session_id = "$9".into();
    moved.windows[0].name = "moved-window".into();
    moved.panes[0].session_id = "$9".into();
    moved.panes[0].window_id = "@9".into();
    assert!(runtime.reconcile_topology(&moved, "server-a").unwrap());

    let mut side_end = event("side-end", 0, "SessionEnd");
    side_end.native_session_id = "native-side".into();
    let restored = runtime
        .ingest_hook_with_context(&side_end, "server-a", Some(&moved))
        .unwrap()
        .agent
        .unwrap();
    let route = restored.route.unwrap();
    assert_eq!(route.session_id, "$9");
    assert_eq!(route.window_id, "@9");
    assert_eq!(route.session_name_fallback, "moved-session");
    assert_eq!(route.window_name_fallback, "moved-window");
}

#[test]
fn a_parent_stop_can_return_directly_after_btw_completes() {
    let runtime = runtime("codex-btw-direct-parent-stop");
    let topology = topology("codex");
    let parent_turn = root_turn(1);
    let parent = runtime
        .ingest_hook_with_context(
            &event_for_turn("parent-prompt", "UserPromptSubmit", &parent_turn),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    let side_turn = root_turn(2);
    let mut side_prompt = event_for_turn("side-prompt", "UserPromptSubmit", &side_turn);
    side_prompt.native_session_id = "native-side".into();
    runtime
        .ingest_hook_with_context(&side_prompt, "server-a", Some(&topology))
        .unwrap();
    let mut side_stop = event_for_turn("side-stop", "Stop", &side_turn);
    side_stop.native_session_id = "native-side".into();
    runtime
        .ingest_hook_with_context(&side_stop, "server-a", Some(&topology))
        .unwrap();

    let completed = runtime
        .ingest_hook_with_context(
            &event_for_turn("parent-stop", "Stop", &parent_turn),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(completed.notify);
    let completed = completed.agent.unwrap();
    assert_eq!(completed.agent_id, parent.agent_id);
    assert_eq!(completed.lifecycle, v1::AgentLifecycleState::Idle as i32);
}

#[test]
fn nested_codex_sessions_unwind_in_lifo_order() {
    let runtime = runtime("nested-codex-pane-sessions");
    let topology = topology("codex");
    let root_turn_id = root_turn(1);
    let root = runtime
        .ingest_hook_with_context(
            &event_for_turn("root-prompt", "UserPromptSubmit", &root_turn_id),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    let first_turn = root_turn(2);
    let mut first_prompt = event_for_turn("first-prompt", "UserPromptSubmit", &first_turn);
    first_prompt.native_session_id = "native-first".into();
    let first = runtime
        .ingest_hook_with_context(&first_prompt, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();
    let second_turn = root_turn(3);
    let mut second_prompt = event_for_turn("second-prompt", "UserPromptSubmit", &second_turn);
    second_prompt.native_session_id = "native-second".into();
    runtime
        .ingest_hook_with_context(&second_prompt, "server-a", Some(&topology))
        .unwrap();
    let mut second_stop = event_for_turn("second-stop", "Stop", &second_turn);
    second_stop.native_session_id = "native-second".into();
    runtime
        .ingest_hook_with_context(&second_stop, "server-a", Some(&topology))
        .unwrap();
    let mut first_stop = event_for_turn("first-stop", "Stop", &first_turn);
    first_stop.native_session_id = "native-first".into();
    let first_done = runtime
        .ingest_hook_with_context(&first_stop, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(first_done.agent_id, first.agent_id);
    assert_eq!(first_done.lifecycle, v1::AgentLifecycleState::Idle as i32);

    let root_resumed = runtime
        .ingest_hook_with_context(
            &event_for_turn("root-returned", "PostToolUse", &root_turn_id),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(root_resumed.agent_id, root.agent_id);
    assert_eq!(runtime.state.lock().unwrap().codex_predecessors.len(), 0);
}

#[test]
fn btw_parent_resume_survives_restart_and_topology_loss() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("btw-resume-reload-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    let topology = topology("codex");
    let parent_turn = root_turn(1);
    let runtime = AgentRuntime::isolated(path.clone());
    let parent = runtime
        .ingest_hook_with_context(
            &event_for_turn("parent-prompt", "UserPromptSubmit", &parent_turn),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    let side_turn = root_turn(2);
    let mut side_prompt = event_for_turn("side-prompt", "UserPromptSubmit", &side_turn);
    side_prompt.native_session_id = "native-side".into();
    runtime
        .ingest_hook_with_context(&side_prompt, "server-a", Some(&topology))
        .unwrap();
    let mut side_stop = event_for_turn("side-stop", "Stop", &side_turn);
    side_stop.native_session_id = "native-side".into();
    runtime
        .ingest_hook_with_context(&side_stop, "server-a", Some(&topology))
        .unwrap();
    drop(runtime);

    let reloaded = AgentRuntime::isolated(path);
    let restored = reloaded
        .ingest_hook_with_context(
            &event_for_turn("parent-returned", "PostToolUse", &parent_turn),
            "server-a",
            None,
        )
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(restored.agent_id, parent.agent_id);
    assert_eq!(restored.route.unwrap().pane_id, "%7");
    assert_eq!(restored.lifecycle, v1::AgentLifecycleState::Working as i32);
}

#[test]
fn codex_pane_predecessor_history_is_bounded() {
    let runtime = runtime("bounded-codex-pane-history");
    let topology = topology("codex");
    for index in 0..=MAX_CODEX_PANE_SESSION_DEPTH + 2 {
        let mut prompt = event_for_turn(
            &format!("prompt-{index}"),
            "UserPromptSubmit",
            &root_turn(index as u16 + 1),
        );
        prompt.native_session_id = format!("native-{index}");
        runtime
            .ingest_hook_with_context(&prompt, "server-a", Some(&topology))
            .unwrap();
    }
    let state = runtime.state.lock().unwrap();
    assert_eq!(state.codex_predecessors.len(), MAX_CODEX_PANE_SESSION_DEPTH);
}

#[test]
fn a_departed_codex_pane_discards_its_suspended_session_chain() {
    let runtime = runtime("departed-codex-pane-chain");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("parent-prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let mut side = event("side-prompt", 0, "UserPromptSubmit");
    side.native_session_id = "native-side".into();
    runtime
        .ingest_hook_with_context(&side, "server-a", Some(&topology))
        .unwrap();
    assert_eq!(runtime.state.lock().unwrap().codex_predecessors.len(), 1);

    assert!(
        runtime
            .reconcile_topology(&tmux_control::TmuxSnapshot::default(), "server-a")
            .unwrap()
    );
    let state = runtime.state.lock().unwrap();
    assert!(state.agents.is_empty());
    assert!(state.codex_predecessors.is_empty());
}

#[test]
fn ending_a_suspended_session_removes_it_without_changing_foreground_ownership() {
    let runtime = runtime("dismiss-suspended-codex-session");
    let topology = topology("codex");
    let parent = runtime
        .ingest_hook_with_context(
            &event("parent-prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    let mut side = event("side-prompt", 0, "UserPromptSubmit");
    side.native_session_id = "native-side".into();
    let side_id = runtime
        .ingest_hook_with_context(&side, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap()
        .agent_id;
    let parent_end = event("parent-end", 0, "SessionEnd");
    let unchanged = runtime
        .ingest_hook_with_context(&parent_end, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(unchanged.agent_id, side_id);
    assert_eq!(runtime.state.lock().unwrap().codex_predecessors.len(), 0);

    let mut side_stop = event("side-stop", 0, "Stop");
    side_stop.native_session_id = "native-side".into();
    runtime
        .ingest_hook_with_context(&side_stop, "server-a", Some(&topology))
        .unwrap();
    assert!(matches!(
        runtime.ingest_hook_with_context(
            &event("parent-late", 0, "PostToolUse"),
            "server-a",
            Some(&topology),
        ),
        Err(HookIngestFailure::Superseded)
    ));
    assert_ne!(side_id, parent.agent_id);
}

#[test]
fn an_unmapped_codex_fork_does_not_gain_pane_continuity() {
    let runtime = runtime("codex-unmapped-superseded-fork");
    let topology = topology("codex");
    let root = runtime
        .ingest_hook_with_context(
            &event("root-start", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();

    let mut unmapped_fork_stop = event("fork-stop-unmapped", 0, "Stop");
    unmapped_fork_stop.native_session_id = "native-fork".into();
    assert!(matches!(
        runtime.ingest_hook_with_context(&unmapped_fork_stop, "server-a", None),
        Err(HookIngestFailure::Superseded)
    ));

    let mut mapped_fork_end = event("fork-end-mapped", 0, "SessionEnd");
    mapped_fork_end.native_session_id = "native-fork".into();
    assert!(matches!(
        runtime.ingest_hook_with_context(&mapped_fork_end, "server-a", Some(&topology)),
        Err(HookIngestFailure::Superseded)
    ));

    let snapshot = runtime.snapshot_for("server-a");
    let pane_owner = snapshot
        .agents
        .iter()
        .find(|agent| {
            agent
                .route
                .as_ref()
                .is_some_and(|route| route.pane_id == "%7")
        })
        .unwrap();
    assert_eq!(pane_owner.agent_id, root.agent_id);
    assert_eq!(
        pane_owner.lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    assert_eq!(snapshot.agents.len(), 1);
}

#[test]
fn topology_outage_preserves_current_native_identity_route_and_voice_reply() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!(
            "phase6-agent-outage-continuity-{}",
            uuid::Uuid::new_v4()
        ))
        .join("agents.json");
    let replies = Arc::new(Mutex::new(Vec::new()));
    let reply_calls = Arc::clone(&replies);
    let runtime = AgentRuntime::isolated_with_sink(
        path,
        Box::new(move |reply| reply_calls.lock().unwrap().push(reply)),
    );
    let topology = topology("codex");
    let turn_a = root_turn(1);
    let turn_b = root_turn(2);
    let root = runtime
        .ingest_hook_with_context(
            &event_for_turn("prompt-a", "UserPromptSubmit", &turn_a),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();

    let continued = runtime
        .ingest_hook_with_context(
            &event_for_turn("continue-b", "PreToolUse", &turn_b),
            "server-a",
            None,
        )
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(continued.agent_id, root.agent_id);
    assert_eq!(continued.route.as_ref().unwrap().pane_id, "%7");

    let mut stop = event_for_turn("stop-b", "Stop", &turn_b);
    let mut payload: serde_json::Value = serde_json::from_slice(&stop.payload_json).unwrap();
    payload[adapters::LAST_ASSISTANT_MESSAGE_FIELD] = "current reply".into();
    stop.payload_json = serde_json::to_vec(&payload).unwrap();
    let completed = runtime
        .ingest_hook_with_context(&stop, "server-a", None)
        .unwrap()
        .agent
        .unwrap();

    assert_eq!(completed.agent_id, root.agent_id);
    assert_eq!(completed.route.as_ref().unwrap().pane_id, "%7");
    let snapshot = runtime.snapshot_for("server-a");
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].agent_id, root.agent_id);
    let replies = replies.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].server_identity, "server-a");
    assert_eq!(replies[0].pane_id, "%7");
    assert_eq!(replies[0].text, "current reply");
}

#[test]
fn a_native_session_has_one_identity_before_during_and_after_topology_recovery() {
    let runtime = runtime("stable-native-identity");
    let missing_topology = topology("codex");
    let native_session_id = "native-stable";
    let turn = root_turn(1);
    let mut start = event_for_turn("stable-start", "UserPromptSubmit", &turn);
    start.native_session_id = native_session_id.into();
    start.pane_id = "%99".into();

    let initial = runtime
        .ingest_hook_with_context(&start, "server-a", Some(&missing_topology))
        .unwrap()
        .agent
        .unwrap();
    assert!(initial.route.as_ref().unwrap().pane_id.is_empty());

    let mut outage_activity = event_for_turn("stable-outage", "PreToolUse", &turn);
    outage_activity.native_session_id = native_session_id.into();
    outage_activity.pane_id = "%99".into();
    let during_outage = runtime
        .ingest_hook_with_context(&outage_activity, "server-a", None)
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(during_outage.agent_id, initial.agent_id);

    let mut recovered_topology = missing_topology;
    recovered_topology.panes[0].id = "%99".into();
    let mut recovered = event_for_turn("stable-recovered", "PostToolUse", &turn);
    recovered.native_session_id = native_session_id.into();
    recovered.pane_id = "%99".into();
    let recovered = runtime
        .ingest_hook_with_context(&recovered, "server-a", Some(&recovered_topology))
        .unwrap()
        .agent
        .unwrap();

    assert_eq!(recovered.agent_id, initial.agent_id);
    assert_eq!(recovered.route.as_ref().unwrap().pane_id, "%99");
    assert_eq!(runtime.state.lock().unwrap().agents.len(), 1);
}

#[test]
fn an_unverified_pane_cannot_mutate_a_session_bound_elsewhere() {
    let runtime = runtime("wrong-pane-cannot-mutate");
    let topology = topology("codex");
    let turn = root_turn(1);
    let current = runtime
        .ingest_hook_with_context(
            &event_for_turn("bound-start", "UserPromptSubmit", &turn),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    let before = runtime.snapshot_for("server-a");

    let mut wrong_pane = event_for_turn("wrong-pane", "Stop", &turn);
    wrong_pane.pane_id = "%99".into();
    assert!(matches!(
        runtime.ingest_hook_with_context(&wrong_pane, "server-a", Some(&topology)),
        Err(HookIngestFailure::Superseded)
    ));

    let after = runtime.snapshot_for("server-a");
    assert_eq!(after, before);
    assert_eq!(after.agents.len(), 1);
    assert_eq!(after.agents[0].agent_id, current.agent_id);
}

#[test]
fn an_idless_codex_hook_cannot_demote_a_native_pane_owner() {
    let runtime = runtime("codex-idless-native-demotion");
    let topology = topology("codex");
    let root = runtime
        .ingest_hook_with_context(
            &event("root-start", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    let before = runtime.snapshot_for("server-a");

    let mut idless = event("idless-stop", 0, "Stop");
    idless.native_session_id.clear();
    assert!(matches!(
        runtime.ingest_hook_with_context(&idless, "server-a", Some(&topology)),
        Err(HookIngestFailure::Superseded)
    ));

    let after = runtime.snapshot_for("server-a");
    assert_eq!(after, before);
    assert_eq!(after.agents[0].agent_id, root.agent_id);
}

#[test]
fn a_goal_continuation_keeps_session_ownership_during_late_fork_teardown() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!(
            "phase6-agent-continuation-fork-{}",
            uuid::Uuid::new_v4()
        ))
        .join("agents.json");
    let replies = Arc::new(Mutex::new(Vec::new()));
    let reply_calls = Arc::clone(&replies);
    let runtime = AgentRuntime::isolated_with_sink(
        path,
        Box::new(move |reply| reply_calls.lock().unwrap().push(reply)),
    );
    let topology = topology("codex");
    let turn_a = root_turn(1);
    let turn_b = root_turn(2);
    let root = runtime
        .ingest_hook_with_context(
            &event_for_turn("root-prompt-a", "UserPromptSubmit", &turn_a),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &event_for_turn("root-stop-a", "Stop", &turn_a),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &event_for_turn("root-continues-b", "PreToolUse", &turn_b),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let before = runtime.snapshot_for("server-a");

    for (index, (event_name, topology)) in [("Stop", Some(&topology)), ("SessionEnd", None)]
        .into_iter()
        .enumerate()
    {
        let mut late = event_for_turn(
            &format!("fork-late-{index}"),
            event_name,
            &root_turn(10 + index as u16),
        );
        late.native_session_id = "native-fork".into();
        let mut payload: serde_json::Value = serde_json::from_slice(&late.payload_json).unwrap();
        payload[adapters::LAST_ASSISTANT_MESSAGE_FIELD] = "wrong fork reply".into();
        late.payload_json = serde_json::to_vec(&payload).unwrap();
        assert!(matches!(
            runtime.ingest_hook_with_context(&late, "server-a", topology),
            Err(HookIngestFailure::Superseded)
        ));
    }

    let after = runtime.snapshot_for("server-a");
    assert_eq!(after, before);
    assert_eq!(after.agents.len(), 1);
    assert_eq!(after.agents[0].agent_id, root.agent_id);
    assert_eq!(
        after.agents[0].lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    let state = runtime.state.lock().unwrap();
    assert_eq!(
        state.agents[&root.agent_id].codex_active_root_turn_id,
        turn_b
    );
    drop(state);
    let replies = replies.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].text, "wrong fork reply");
}

#[test]
fn superseded_session_authority_precedes_duplicate_transcript_repair() {
    let runtime = runtime("superseded-before-duplicate-repair");
    let topology = topology("codex");
    let turn_a = root_turn(1);
    let turn_b = root_turn(2);
    let child_turn = root_turn(3);
    for hook in [
        event_for_turn("prompt-a", "UserPromptSubmit", &turn_a),
        event_for_turn("activity-a", "PreToolUse", &turn_a),
        child_event_for_turn("child-a", "SubagentStart", "child", &child_turn),
        event_for_turn("continue-b", "PreToolUse", &turn_b),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let generation = runtime.snapshot_for("server-a").generation;

    let mut fork = event_for_turn("continue-b", "PostToolUse", &turn_a);
    fork.native_session_id = "native-fork".into();
    let mut payload: serde_json::Value = serde_json::from_slice(&fork.payload_json).unwrap();
    payload[adapters::CODEX_CHILD_TRANSITIONS_FIELD] =
        serde_json::json!([{"agent_id": "child", "active": false}]);
    fork.payload_json = serde_json::to_vec(&payload).unwrap();
    assert!(matches!(
        runtime.ingest_hook_with_context(&fork, "server-a", Some(&topology)),
        Err(HookIngestFailure::Superseded)
    ));

    let snapshot = runtime.snapshot_for("server-a");
    assert_eq!(snapshot.generation, generation);
    assert_eq!(snapshot.agents.len(), 1);
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.codex_active_root_turn_id, turn_b);
    assert_eq!(
        record
            .codex_running_subagents
            .get("child")
            .map(String::as_str),
        Some(child_turn.as_str())
    );
}

#[test]
fn foreign_hook_is_visible_but_never_invents_a_destination() {
    let runtime = runtime("foreign-route");
    let mut foreign = event("foreign", 0, "PermissionRequest");
    foreign.pane_id = "%99".into();
    runtime
        .ingest_hook_with_context(&foreign, "server-a", Some(&topology("codex")))
        .unwrap();
    let snapshot = runtime.snapshot_for("server-a");
    let route = snapshot.agents[0].route.as_ref().unwrap().clone();
    assert!(route.pane_id.is_empty());
    assert!(route.session_id.is_empty());
    assert!(route.window_id.is_empty());
}

#[test]
fn same_numbered_pane_from_another_server_remains_unmapped() {
    let runtime = runtime("foreign-server-collision");
    let topology = topology("codex");
    runtime.reconcile_topology(&topology, "server-a").unwrap();
    let direct_before = runtime.snapshot_for("server-a").agents[0].clone();
    let mut foreign = event("foreign-collision", 0, "UserPromptSubmit");
    foreign.origin_server_identity = "server-b".into();
    foreign.native_session_id = direct_before.native_session_id.clone();
    runtime
        .ingest_hook_with_context(&foreign, "server-a", Some(&topology))
        .unwrap();
    assert!(
        runtime
            .ingest_hook_with_context(&foreign, "server-a", Some(&topology))
            .is_err(),
        "foreign exact-ID continuity must retain source-event dedupe"
    );
    let mut completed = foreign.clone();
    completed.source_event_id = "foreign-completed".into();
    completed.payload_json =
        serde_json::to_vec(&serde_json::json!({"hook_event_name": "Stop"})).unwrap();
    runtime
        .ingest_hook_with_context(&completed, "server-a", Some(&topology))
        .unwrap();
    let snapshot = runtime.snapshot_for("server-a");
    let direct_after = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id == direct_before.agent_id)
        .unwrap();
    assert_eq!(direct_after, &direct_before);
    let unmapped = snapshot
        .agents
        .iter()
        .find(|agent| agent.agent_id != direct_before.agent_id)
        .unwrap();
    let route = unmapped.route.as_ref().unwrap();
    assert!(route.pane_id.is_empty() && route.session_id.is_empty() && route.window_id.is_empty());
    assert_eq!(unmapped.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert_eq!(unmapped.attention_kind, "completed");
    assert_eq!(unmapped.attention_generation, 1);
}

#[test]
fn blocked_to_idle_never_becomes_a_completed_attention() {
    let unseen = runtime("blocked-idle-unseen");
    unseen
        .ingest_hook_with_context(
            &event("blocked", 0, "PermissionRequest"),
            "server-a",
            Some(&topology("codex")),
        )
        .unwrap();
    let idle = unseen
        .ingest_hook_with_context(
            &event_for_turn("idle", "Stop", "turn-blocked"),
            "server-a",
            Some(&topology("codex")),
        )
        .unwrap();
    assert!(!idle.notify);
    let idle = idle.agent.unwrap();
    assert_eq!(idle.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert_eq!(idle.attention_kind, "blocked");
    assert_ne!(idle.attention_kind, "completed");

    let seen = runtime("blocked-idle-seen");
    let blocked = seen
        .ingest_hook_with_context(
            &event("blocked-seen", 0, "PermissionRequest"),
            "server-a",
            Some(&topology("codex")),
        )
        .unwrap()
        .agent
        .unwrap();
    seen.mark_seen(&blocked.agent_id, blocked.attention_generation)
        .unwrap();
    let idle = seen
        .ingest_hook_with_context(
            &event_for_turn("idle-seen", "Stop", "turn-blocked-seen"),
            "server-a",
            Some(&topology("codex")),
        )
        .unwrap();
    assert!(!idle.notify);
    assert_eq!(idle.agent.unwrap().attention_kind, "");
}

#[test]
fn concurrent_same_millisecond_hooks_do_not_use_wall_clock_ordering() {
    let runtime = Arc::new(runtime("concurrent"));
    let topology = Arc::new(topology("codex"));
    runtime.reconcile_topology(&topology, "server-a").unwrap();
    let occurred = now_millis();
    let handles: Vec<_> = ["concurrent-a", "concurrent-b"]
        .into_iter()
        .map(|id| {
            let runtime = Arc::clone(&runtime);
            let topology = Arc::clone(&topology);
            std::thread::spawn(move || {
                let mut event = event(id, 0, "UserPromptSubmit");
                event.occurred_at_unix_millis = occurred;
                runtime.ingest_hook_with_context(&event, "server-a", Some(&topology))
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap().unwrap();
    }
    let snapshot = runtime.snapshot_for("server-a");
    assert_eq!(snapshot.agents.len(), 1);
    assert!(snapshot.generation >= 3);
}

#[test]
fn authoritative_vendor_sequence_rejects_replay_but_unsequenced_hooks_remain_concurrent() {
    let runtime = runtime("vendor-sequence");
    let topology = topology("codex");
    let mut sequenced = event("vendor-10", 10, "UserPromptSubmit");
    sequenced.source_sequence_authoritative = true;
    runtime
        .ingest_hook_with_context(&sequenced, "server-a", Some(&topology))
        .unwrap();
    let mut older = event("vendor-9", 9, "PermissionRequest");
    older.source_sequence_authoritative = true;
    assert!(
        runtime
            .ingest_hook_with_context(&older, "server-a", Some(&topology))
            .is_err()
    );
    let unsequenced = event("parallel-no-sequence", 0, "PermissionRequest");
    assert!(
        runtime
            .ingest_hook_with_context(&unsequenced, "server-a", Some(&topology))
            .is_ok()
    );
}

#[test]
fn unsequenced_late_tool_events_cannot_regress_a_completed_phase() {
    let runtime = runtime("phase-reducer");
    let topology = topology("codex");
    for (id, name) in [
        ("prompt", "UserPromptSubmit"),
        ("stop", "Stop"),
        ("late-permission", "PermissionRequest"),
        ("late-post", "PostToolUse"),
    ] {
        runtime
            .ingest_hook_with_context(
                &event_for_turn(id, name, "turn-a"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
    }
    let completed = &runtime.snapshot_for("server-a").agents[0];
    assert_eq!(completed.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert_eq!(completed.attention_generation, 1);

    runtime
        .ingest_hook_with_context(
            &event("next-prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
}

#[test]
fn a_codex_goal_continuation_reopens_working_without_another_user_prompt() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!(
            "phase6-agent-goal-continuation-{}",
            uuid::Uuid::new_v4()
        ))
        .join("agents.json");
    let runtime = AgentRuntime::isolated(path.clone());
    let topology = topology("codex");
    for (id, name, turn_id) in [
        ("prompt-a", "UserPromptSubmit", "turn-a"),
        ("stop-a", "Stop", "turn-a"),
    ] {
        runtime
            .ingest_hook_with_context(
                &event_for_turn(id, name, turn_id),
                "server-a",
                Some(&topology),
            )
            .unwrap();
    }
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
    drop(runtime);
    let runtime = AgentRuntime::isolated(path);

    // Codex starts automatic goal continuations with a new turn ID but no
    // UserPromptSubmit. The first tool hook is therefore the start signal.
    runtime
        .ingest_hook_with_context(
            &event_for_turn("pre-b", "PreToolUse", "turn-b"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let continued = &runtime.snapshot_for("server-a").agents[0];
    assert_eq!(continued.lifecycle, v1::AgentLifecycleState::Working as i32);
    assert!(
        !runtime
            .state
            .lock()
            .unwrap()
            .agents
            .get(&continued.agent_id)
            .unwrap()
            .hook_terminal
    );

    // Events from the completed turn remain stale even after its successor
    // reopened, including a delayed terminal hook.
    for (id, name) in [
        ("late-post-a", "PostToolUse"),
        ("late-stop-a", "Stop"),
        ("late-prompt-a", "UserPromptSubmit"),
    ] {
        runtime
            .ingest_hook_with_context(
                &event_for_turn(id, name, "turn-a"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        assert_eq!(
            runtime.snapshot_for("server-a").agents[0].lifecycle,
            v1::AgentLifecycleState::Working as i32
        );
    }

    runtime
        .ingest_hook_with_context(
            &event_for_turn("stop-b", "Stop", "turn-b"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &event_for_turn("late-post-b", "PostToolUse", "turn-b"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn a_tool_free_goal_continuation_can_end_on_its_first_observed_hook() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!(
            "phase6-agent-tool-free-goal-continuation-{}",
            uuid::Uuid::new_v4()
        ))
        .join("agents.json");
    let replies = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&replies);
    let runtime = AgentRuntime::isolated_with_sink(
        path,
        Box::new(move |reply| sink.lock().unwrap().push(reply)),
    );
    let topology = topology("codex");
    let turn_a = root_turn(1);
    let turn_b = root_turn(2);
    runtime
        .ingest_hook_with_context(
            &event_for_turn("prompt-a", "UserPromptSubmit", &turn_a),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &event_for_turn("stop-a", "Stop", &turn_a),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let prior_attention = runtime
        .state
        .lock()
        .unwrap()
        .agents
        .values()
        .next()
        .unwrap()
        .attention_generation;

    let mut stop = event_for_turn("stop-b", "Stop", &turn_b);
    let mut payload: serde_json::Value = serde_json::from_slice(&stop.payload_json).unwrap();
    payload[adapters::LAST_ASSISTANT_MESSAGE_FIELD] = "tool-free reply".into();
    stop.payload_json = serde_json::to_vec(&payload).unwrap();
    let completed = runtime
        .ingest_hook_with_context(&stop, "server-a", Some(&topology))
        .unwrap();
    assert!(completed.notify);
    assert_eq!(replies.lock().unwrap()[0].text, "tool-free reply");
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert_eq!(record.codex_active_root_turn_id, turn_b);
    assert_eq!(record.attention_generation, prior_attention + 1);
    assert!(record.codex_terminal_root_turn_ids.contains(&turn_a));
    assert!(
        record
            .codex_terminal_root_turn_ids
            .contains(&record.codex_active_root_turn_id)
    );
}

#[test]
fn an_unseen_older_root_stop_cannot_displace_the_active_root() {
    let runtime = runtime("older-unseen-root-stop");
    let topology = topology("codex");
    let newer = root_turn(2);
    runtime
        .ingest_hook_with_context(
            &event_for_turn("prompt-newer", "UserPromptSubmit", &newer),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &event_for_turn("late-stop-older", "Stop", &root_turn(1)),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
    assert_eq!(record.codex_active_root_turn_id, newer);
    assert!(!record.hook_terminal);
}

#[test]
fn bounded_root_history_still_rejects_an_evicted_older_turn() {
    let runtime = runtime("long-root-history");
    let topology = topology("codex");
    for index in 0..70 {
        let turn = root_turn(index);
        runtime
            .ingest_hook_with_context(
                &event_for_turn(&format!("prompt-{index}"), "UserPromptSubmit", &turn),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        runtime
            .ingest_hook_with_context(
                &event_for_turn(&format!("stop-{index}"), "Stop", &turn),
                "server-a",
                Some(&topology),
            )
            .unwrap();
    }

    runtime
        .ingest_hook_with_context(
            &event_for_turn("late-oldest", "PostToolUse", &root_turn(0)),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert_eq!(record.codex_active_root_turn_id, root_turn(69));
    assert_eq!(
        record.codex_terminal_root_turn_ids.len(),
        MAX_CODEX_TERMINAL_ROOT_TURNS
    );
}

#[test]
fn a_new_root_owner_retires_the_displaced_turn_before_its_delayed_stop() {
    let runtime = runtime("goal-continuation-before-old-stop");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event_for_turn("prompt-a", "UserPromptSubmit", "turn-a"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &event_for_turn("pre-b", "PreToolUse", "turn-b"),
            "server-a",
            Some(&topology),
        )
        .unwrap();

    for (id, name) in [("late-post-a", "PostToolUse"), ("late-stop-a", "Stop")] {
        runtime
            .ingest_hook_with_context(
                &event_for_turn(id, name, "turn-a"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
    }
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
    assert_eq!(record.codex_active_root_turn_id, "turn-b");
    assert!(
        record
            .codex_terminal_root_turn_ids
            .iter()
            .any(|turn| turn == "turn-a")
    );
    assert!(!record.hook_terminal);
}

#[test]
fn a_new_root_first_seen_at_stop_still_waits_for_its_live_child() {
    let runtime = runtime("goal-continuation-stop-after-child");
    let topology = topology("codex");
    for hook in [
        event_for_turn("prompt-a", "UserPromptSubmit", "turn-a"),
        event_for_turn("stop-a", "Stop", "turn-a"),
        child_event_for_turn("child-b", "SubagentStart", "child", "child-turn-b"),
        event_for_turn("stop-b", "Stop", "turn-b"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    {
        let state = runtime.state.lock().unwrap();
        let record = state.agents.values().next().unwrap();
        assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
        assert_eq!(record.codex_active_root_turn_id, "turn-b");
        assert!(record.codex_parent_stopped_for_subagents);
        assert_eq!(
            record
                .codex_subagent_root_turn_ids
                .get("child")
                .map(String::as_str),
            Some("")
        );
        assert_eq!(
            record
                .codex_latest_subagent_turns
                .back()
                .map(|turn| turn.turn_id.as_str()),
            Some("child-turn-b")
        );
    }

    let mut repaired_stop = event_for_turn("stop-b", "Stop", "turn-b");
    let mut payload: serde_json::Value =
        serde_json::from_slice(&repaired_stop.payload_json).unwrap();
    payload[adapters::CODEX_CHILD_TRANSITIONS_FIELD] =
        serde_json::json!([{"agent_id": "child", "active": false}]);
    repaired_stop.payload_json = serde_json::to_vec(&payload).unwrap();
    let completed = runtime
        .ingest_hook_with_context(&repaired_stop, "server-a", Some(&topology))
        .unwrap();
    assert!(completed.notify);
    assert_eq!(
        completed.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn a_child_resume_before_its_new_root_binds_only_to_that_successor() {
    let runtime = runtime("goal-continuation-resume-before-root");
    let topology = topology("codex");
    for hook in [
        event_for_turn("prompt-a", "UserPromptSubmit", "turn-a"),
        child_event_for_turn("child-a", "SubagentStart", "child", "child-turn-a"),
        child_event_for_turn("child-stop-a", "SubagentStop", "child", "child-turn-a"),
        event_for_turn("stop-a", "Stop", "turn-a"),
        child_event_for_turn("child-b", "SubagentStart", "child", "child-turn-b"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    {
        let state = runtime.state.lock().unwrap();
        let record = state.agents.values().next().unwrap();
        assert!(record.codex_subagents_awaiting_root.contains("child"));
    }

    let mut stale_a = event_for_turn("late-stop-a", "Stop", "turn-a");
    let mut payload: serde_json::Value = serde_json::from_slice(&stale_a.payload_json).unwrap();
    payload[adapters::CODEX_CHILD_TRANSITIONS_FIELD] =
        serde_json::json!([{"agent_id": "child", "active": false}]);
    stale_a.payload_json = serde_json::to_vec(&payload).unwrap();
    runtime
        .ingest_hook_with_context(&stale_a, "server-a", Some(&topology))
        .unwrap();
    assert_eq!(
        runtime
            .state
            .lock()
            .unwrap()
            .agents
            .values()
            .next()
            .unwrap()
            .codex_running_subagents["child"],
        "child-turn-b"
    );

    let stop_b = event_for_turn("stop-b", "Stop", "turn-b");
    runtime
        .ingest_hook_with_context(&stop_b, "server-a", Some(&topology))
        .unwrap();
    {
        let state = runtime.state.lock().unwrap();
        let record = state.agents.values().next().unwrap();
        assert!(!record.codex_subagents_awaiting_root.contains("child"));
        assert_eq!(record.codex_subagent_root_turn_ids["child"], "turn-b");
        assert!(record.codex_parent_stopped_for_subagents);
    }

    let mut repaired_b = stop_b;
    let mut payload: serde_json::Value = serde_json::from_slice(&repaired_b.payload_json).unwrap();
    payload[adapters::CODEX_CHILD_TRANSITIONS_FIELD] =
        serde_json::json!([{"agent_id": "child", "active": false}]);
    repaired_b.payload_json = serde_json::to_vec(&payload).unwrap();
    let completed = runtime
        .ingest_hook_with_context(&repaired_b, "server-a", Some(&topology))
        .unwrap();
    assert!(completed.notify);
    assert_eq!(
        completed.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn an_unmatched_child_terminal_still_protects_its_later_resume() {
    let runtime = runtime("unmatched-child-terminal-history");
    let topology = topology("codex");
    for hook in [
        event_for_turn("prompt-a", "UserPromptSubmit", "turn-a"),
        child_event_for_turn(
            "missed-start-stop-a",
            "SubagentStop",
            "child",
            "child-turn-a",
        ),
        child_event_for_turn("late-activity-a", "PreToolUse", "child", "child-turn-a"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    assert!(
        runtime
            .state
            .lock()
            .unwrap()
            .agents
            .values()
            .next()
            .unwrap()
            .codex_running_subagents
            .is_empty()
    );
    for hook in [
        event_for_turn("stop-a", "Stop", "turn-a"),
        child_event_for_turn("child-b", "SubagentStart", "child", "child-turn-b"),
        event_for_turn("stop-b", "Stop", "turn-b"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }

    let mut stale_a = event_for_turn("late-stop-a", "Stop", "turn-a");
    let mut payload: serde_json::Value = serde_json::from_slice(&stale_a.payload_json).unwrap();
    payload[adapters::CODEX_CHILD_TRANSITIONS_FIELD] =
        serde_json::json!([{"agent_id": "child", "active": false}]);
    stale_a.payload_json = serde_json::to_vec(&payload).unwrap();
    runtime
        .ingest_hook_with_context(&stale_a, "server-a", Some(&topology))
        .unwrap();
    {
        let state = runtime.state.lock().unwrap();
        let record = state.agents.values().next().unwrap();
        assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
        assert_eq!(record.codex_running_subagents["child"], "child-turn-b");
        assert_eq!(record.codex_subagent_root_turn_ids["child"], "turn-b");
    }

    let completed = runtime
        .ingest_hook_with_context(
            &child_event_for_turn("child-stop-b", "SubagentStop", "child", "child-turn-b"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(completed.notify);
}

#[test]
fn a_late_older_child_terminal_tombstones_without_clearing_its_resume() {
    let runtime = runtime("late-older-child-terminal");
    let topology = topology("codex");
    for hook in [
        event_for_turn("prompt-a", "UserPromptSubmit", "turn-a"),
        child_event_for_turn("child-a", "SubagentStart", "child", "child-turn-a"),
        event_for_turn("pre-b", "PreToolUse", "turn-b"),
        child_event_for_turn("child-b", "SubagentStart", "child", "child-turn-b"),
        child_event_for_turn("late-stop-a", "SubagentStop", "child", "child-turn-a"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    assert_eq!(
        runtime
            .state
            .lock()
            .unwrap()
            .agents
            .values()
            .next()
            .unwrap()
            .codex_running_subagents["child"],
        "child-turn-b"
    );
    for hook in [
        event_for_turn("stop-b", "Stop", "turn-b"),
        child_event_for_turn("child-stop-b", "SubagentStop", "child", "child-turn-b"),
        child_event_for_turn("late-activity-a", "PreToolUse", "child", "child-turn-a"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert!(record.codex_running_subagents.is_empty());
}

#[test]
fn an_evicted_child_tombstone_is_still_rejected_by_the_latest_turn_watermark() {
    let runtime = runtime("evicted-child-terminal-watermark");
    let topology = topology("codex");
    let parent = root_turn(1);
    for hook in [
        event_for_turn("prompt", "UserPromptSubmit", &parent),
        event_for_turn("stop", "Stop", &parent),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }

    let oldest = root_turn(10);
    for index in 10..=(10 + MAX_CODEX_CHILD_TURN_HISTORY as u16) {
        let turn = root_turn(index);
        runtime
            .ingest_hook_with_context(
                &child_event_for_turn(
                    &format!("child-stop-{index}"),
                    "SubagentStop",
                    "child",
                    &turn,
                ),
                "server-a",
                Some(&topology),
            )
            .unwrap();
    }
    {
        let state = runtime.state.lock().unwrap();
        let record = state.agents.values().next().unwrap();
        assert!(
            !record
                .codex_terminal_subagent_turns
                .iter()
                .any(|turn| { turn.agent_id == "child" && turn.turn_id == oldest })
        );
    }

    runtime
        .ingest_hook_with_context(
            &child_event_for_turn("late-oldest", "PreToolUse", "child", &oldest),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert!(record.codex_running_subagents.is_empty());
}

#[test]
fn an_evicted_child_tombstone_is_rejected_while_a_sibling_keeps_working() {
    let runtime = runtime("evicted-child-terminal-with-live-sibling");
    let topology = topology("codex");
    let completed_child = root_turn(2);
    for hook in [
        event_for_turn("prompt", "UserPromptSubmit", &root_turn(1)),
        child_event_for_turn("sibling-start", "SubagentStart", "sibling", &root_turn(3)),
        child_event_for_turn(
            "completed-start",
            "SubagentStart",
            "completed",
            &completed_child,
        ),
        child_event_for_turn(
            "completed-stop",
            "SubagentStop",
            "completed",
            &completed_child,
        ),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    for index in 10..=(10 + MAX_CODEX_CHILD_TURN_HISTORY as u16) {
        runtime
            .ingest_hook_with_context(
                &child_event_for_turn(
                    &format!("churn-stop-{index}"),
                    "SubagentStop",
                    "churn",
                    &root_turn(index),
                ),
                "server-a",
                Some(&topology),
            )
            .unwrap();
    }
    runtime
        .ingest_hook_with_context(
            &child_event_for_turn(
                "completed-late",
                "PreToolUse",
                "completed",
                &completed_child,
            ),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
    assert_eq!(record.codex_running_subagents.len(), 1);
    assert!(record.codex_running_subagents.contains_key("sibling"));
}

#[test]
fn a_session_terminal_tombstones_live_child_turns_before_clearing_them() {
    let runtime = runtime("session-terminal-child-tombstones");
    let topology = topology("codex");
    let parent = root_turn(1);
    let child = root_turn(2);
    for hook in [
        event_for_turn("prompt", "UserPromptSubmit", &parent),
        child_event_for_turn("child-start", "SubagentStart", "child", &child),
        event_for_turn("session-end", "SessionEnd", &parent),
        child_event_for_turn("late-child", "PreToolUse", "child", &child),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert!(record.codex_running_subagents.is_empty());
    assert!(
        record
            .codex_terminal_subagent_turns
            .iter()
            .any(|turn| { turn.agent_id == "child" && turn.turn_id == child })
    );
}

#[test]
fn ignored_child_activity_does_not_refresh_a_live_siblings_evidence() {
    let runtime = runtime("stale-child-evidence-refresh");
    let topology = topology("codex");
    for hook in [
        event_for_turn("prompt", "UserPromptSubmit", "turn-a"),
        child_event_for_turn("sibling-start", "SubagentStart", "sibling", "sibling-turn"),
        child_event_for_turn("stale-start", "SubagentStart", "stale", "stale-turn"),
        child_event_for_turn("stale-stop", "SubagentStop", "stale", "stale-turn"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    {
        let mut state = runtime.state.lock().unwrap();
        state
            .agents
            .values_mut()
            .next()
            .unwrap()
            .subagent_evidence_observed_at_unix_millis = 123;
    }
    runtime
        .ingest_hook_with_context(
            &child_event_for_turn("stale-late", "PreToolUse", "stale", "stale-turn"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.subagent_evidence_observed_at_unix_millis, 123);
    assert_eq!(record.codex_running_subagents["sibling"], "sibling-turn");
}

#[test]
fn an_awaiting_child_terminal_does_not_complete_the_previous_root() {
    let runtime = runtime("awaiting-child-terminal-before-root");
    let topology = topology("codex");
    for hook in [
        event_for_turn("prompt-a", "UserPromptSubmit", "turn-a"),
        child_event_for_turn("child-a", "SubagentStart", "child", "child-turn-a"),
        child_event_for_turn("child-stop-a", "SubagentStop", "child", "child-turn-a"),
        event_for_turn("stop-a", "Stop", "turn-a"),
        child_event_for_turn("child-b", "SubagentStart", "child", "child-turn-b"),
        child_event_for_turn("child-stop-b", "SubagentStop", "child", "child-turn-b"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
    assert!(!record.hook_terminal);
    assert!(!record.codex_parent_stopped_for_subagents);
}

#[test]
fn stale_root_transcript_repair_cannot_stop_the_new_parent() {
    let runtime = runtime("stale-root-child-repair");
    let topology = topology("codex");
    for hook in [
        event_for_turn("prompt-a", "UserPromptSubmit", "turn-a"),
        child_event_for_turn("child-a", "SubagentStart", "child", "child-turn-a"),
        event_for_turn("stop-a", "Stop", "turn-a"),
        event_for_turn("pre-b", "PreToolUse", "turn-b"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }

    let mut stale = event_for_turn("late-stop-a", "Stop", "turn-a");
    let mut payload: serde_json::Value = serde_json::from_slice(&stale.payload_json).unwrap();
    payload[adapters::CODEX_CHILD_TRANSITIONS_FIELD] =
        serde_json::json!([{"agent_id": "child", "active": false}]);
    stale.payload_json = serde_json::to_vec(&payload).unwrap();
    let repaired = runtime
        .ingest_hook_with_context(&stale, "server-a", Some(&topology))
        .unwrap();
    assert!(!repaired.notify);
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
    assert_eq!(record.codex_active_root_turn_id, "turn-b");
    assert_eq!(record.codex_running_subagents["child"], "child-turn-a");
    assert!(!record.codex_parent_stopped_for_subagents);
    assert!(!record.hook_terminal);
}

#[test]
fn a_goal_continuation_owns_parent_lifecycle_across_old_child_and_fallback_events() {
    let runtime = runtime("goal-continuation-child-ownership");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event_for_turn("prompt-a", "UserPromptSubmit", "turn-a"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &child_event_for_turn("child-a", "SubagentStart", "child", "child-turn-a"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &child_event_for_turn(
                "old-child-a",
                "SubagentStart",
                "old-child",
                "old-child-turn-a",
            ),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &event_for_turn("stop-a", "Stop", "turn-a"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(
        runtime
            .state
            .lock()
            .unwrap()
            .agents
            .values()
            .next()
            .unwrap()
            .codex_parent_stopped_for_subagents
    );
    runtime
        .ingest_hook_with_context(
            &event_for_turn("late-permission-a", "PermissionRequest", "turn-a"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    {
        let state = runtime.state.lock().unwrap();
        let record = state.agents.values().next().unwrap();
        assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
        assert!(record.codex_parent_stopped_for_subagents);
    }

    runtime
        .ingest_hook_with_context(
            &event_for_turn("pre-b", "PreToolUse", "turn-b"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &child_event_for_turn("child-a-stop", "SubagentStop", "child", "child-turn-a"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Working as i32
    );

    runtime
        .ingest_hook_with_context(
            &child_event_for_turn("child-b", "SubagentStart", "child", "child-turn-b"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let mut stale_fallback = event_for_turn("late-stop-a-fallback", "Stop", "turn-a");
    let mut payload: serde_json::Value =
        serde_json::from_slice(&stale_fallback.payload_json).unwrap();
    payload[adapters::CODEX_CHILD_TRANSITIONS_FIELD] = serde_json::json!([
        {"agent_id": "child", "active": false},
        {"agent_id": "old-child", "active": false},
    ]);
    stale_fallback.payload_json = serde_json::to_vec(&payload).unwrap();
    let repaired = runtime
        .ingest_hook_with_context(&stale_fallback, "server-a", Some(&topology))
        .unwrap();
    assert!(!repaired.notify);

    let idless_stop = event("idless-stop", 0, "Stop");
    runtime
        .ingest_hook_with_context(&idless_stop, "server-a", Some(&topology))
        .unwrap();
    let child_scoped_stop = child_event_for_turn("child-stop", "Stop", "child", "child-turn-b");
    runtime
        .ingest_hook_with_context(&child_scoped_stop, "server-a", Some(&topology))
        .unwrap();
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
    assert!(!record.codex_running_subagents.contains_key("child"));
    assert!(!record.codex_subagent_root_turn_ids.contains_key("child"));
    assert_eq!(
        record.codex_running_subagents["old-child"],
        "old-child-turn-a"
    );
    assert_eq!(record.codex_subagent_root_turn_ids["old-child"], "");
    assert!(!record.codex_parent_stopped_for_subagents);
}

#[test]
fn completed_phase_survives_runtime_reload_before_late_fallback() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase6-agent-reload-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    let topology = topology("codex");
    {
        let runtime = AgentRuntime::isolated(path.clone());
        runtime
            .ingest_hook_with_context(
                &event("prompt", 0, "UserPromptSubmit"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        runtime
            .ingest_hook_with_context(&event("stop", 0, "Stop"), "server-a", Some(&topology))
            .unwrap();
    }
    let runtime = AgentRuntime::isolated(path);
    runtime
        .ingest_hook_with_context(&event("late", 0, "PreToolUse"), "server-a", Some(&topology))
        .unwrap();
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

/// Voice mode (docs/mobile/voice-mode-plan.md §4.7): the final message is
/// handed on exactly when a `Stop` lands the agent in Idle, and nothing the
/// runtime emits or persists carries it.
#[test]
fn a_stop_landing_in_idle_hands_the_reply_on_once_and_stores_none_of_it() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase6-agent-reply-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    let replies = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&replies);
    let runtime = AgentRuntime::isolated_with_sink(
        path.clone(),
        Box::new(move |reply| sink.lock().unwrap().push(reply)),
    );
    let topology = topology("claude");
    let claude = |id: &str, name: &str, extra: serde_json::Value| {
        let mut event = event(id, 0, name);
        event.adapter = v1::AgentAdapterKind::ClaudeCode.into();
        let mut payload = serde_json::json!({"hook_event_name": name});
        for (key, value) in extra.as_object().unwrap() {
            payload[key] = value.clone();
        }
        event.payload_json = serde_json::to_vec(&payload).unwrap();
        event
    };
    let secret = "The private final reply. Nothing stores me.";

    runtime
        .ingest_hook_with_context(
            &claude("prompt", "UserPromptSubmit", serde_json::json!({})),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    // Claude's Stop while a subagent still runs keeps the lifecycle Working:
    // no reply yet.
    let mid_turn = runtime
        .ingest_hook_with_context(
            &claude(
                "stop-subagent",
                "Stop",
                serde_json::json!({
                    adapters::CLAUDE_HAS_RUNNING_SUBAGENT_FIELD: true,
                    adapters::LAST_ASSISTANT_MESSAGE_FIELD: "interim",
                }),
            ),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert_eq!(
        mid_turn.agent.as_ref().unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    assert!(replies.lock().unwrap().is_empty());

    let done = runtime
        .ingest_hook_with_context(
            &claude(
                "stop-final",
                "Stop",
                serde_json::json!({
                    adapters::LAST_ASSISTANT_MESSAGE_FIELD: secret,
                    adapters::LAST_ASSISTANT_MESSAGE_TRUNCATED_FIELD: true,
                }),
            ),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let handed = replies.lock().unwrap().clone();
    assert_eq!(handed.len(), 1);
    assert_eq!(handed[0].text, secret);
    assert!(handed[0].truncated);
    assert_eq!(handed[0].pane_id, "%7");
    assert_eq!(handed[0].state_generation, done.generation);
    assert_eq!(
        handed[0].occurred_at_unix_millis,
        done.agent.as_ref().unwrap().updated_at_unix_millis
    );

    // Nothing outside the sink saw the text.
    let event_bytes = serde_json::to_string(&format!("{done:?}")).unwrap();
    assert!(!event_bytes.contains(secret));
    let snapshot = format!("{:?}", runtime.snapshot_for("server-a"));
    assert!(!snapshot.contains(secret));
    let persisted = fs::read_to_string(&path).unwrap();
    assert!(!persisted.contains(secret));
    assert!(!persisted.contains("interim"));
    assert!(!persisted.contains(adapters::LAST_ASSISTANT_MESSAGE_FIELD));

    // A Stop without a message, and a duplicate Stop, hand nothing on.
    runtime
        .ingest_hook_with_context(
            &claude("stop-silent", "Stop", serde_json::json!({})),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(
        runtime
            .ingest_hook_with_context(
                &claude(
                    "stop-final",
                    "Stop",
                    serde_json::json!({ adapters::LAST_ASSISTANT_MESSAGE_FIELD: secret })
                ),
                "server-a",
                Some(&topology),
            )
            .is_err()
    );
    assert_eq!(replies.lock().unwrap().len(), 1);
}

#[test]
fn pane_voice_receives_a_superseded_root_reply_but_not_a_child_reply() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("pane-voice-superseded-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    let replies = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&replies);
    let runtime = AgentRuntime::isolated_with_sink(
        path,
        Box::new(move |reply| sink.lock().unwrap().push(reply)),
    );
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event_for_turn("parent-prompt", "UserPromptSubmit", &root_turn(1)),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let mut side = event_for_turn("side-prompt", "UserPromptSubmit", &root_turn(2));
    side.native_session_id = "native-side".into();
    runtime
        .ingest_hook_with_context(&side, "server-a", Some(&topology))
        .unwrap();

    let mut idless_child = event("idless-child", 0, "Stop");
    idless_child.native_session_id = "native-side".into();
    idless_child.payload_json = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "Stop",
        adapters::CODEX_SUBAGENT_ID_FIELD: "child-without-turn",
        adapters::LAST_ASSISTANT_MESSAGE_FIELD: "must not be spoken",
    }))
    .unwrap();
    runtime
        .ingest_hook_with_context(&idless_child, "server-a", Some(&topology))
        .unwrap();
    assert!(replies.lock().unwrap().is_empty());

    let mut late_root = event_for_turn("late-root", "Stop", &root_turn(1));
    let mut root_payload: serde_json::Value =
        serde_json::from_slice(&late_root.payload_json).unwrap();
    root_payload[adapters::LAST_ASSISTANT_MESSAGE_FIELD] = "parent answer".into();
    late_root.payload_json = serde_json::to_vec(&root_payload).unwrap();
    assert!(matches!(
        runtime.ingest_hook_with_context(&late_root, "server-a", Some(&topology)),
        Err(HookIngestFailure::Superseded)
    ));

    let mut late_child = child_event_for_turn("late-child", "Stop", "child", &root_turn(3));
    let mut child_payload: serde_json::Value =
        serde_json::from_slice(&late_child.payload_json).unwrap();
    child_payload[adapters::LAST_ASSISTANT_MESSAGE_FIELD] = "child answer".into();
    late_child.payload_json = serde_json::to_vec(&child_payload).unwrap();
    assert!(matches!(
        runtime.ingest_hook_with_context(&late_child, "server-a", Some(&topology)),
        Err(HookIngestFailure::Superseded)
    ));

    let replies = replies.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].pane_id, "%7");
    assert_eq!(replies[0].text, "parent answer");
}

#[test]
fn delayed_old_server_reply_keeps_its_origin_after_reconciliation() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("pane-voice-old-server-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    let replies = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&replies);
    let runtime = AgentRuntime::isolated_with_sink(
        path,
        Box::new(move |reply| sink.lock().unwrap().push(reply)),
    );
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("old-prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime.reconcile_topology(&topology, "server-b").unwrap();

    let mut late = event("old-stop", 0, "Stop");
    late.payload_json = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "Stop",
        adapters::LAST_ASSISTANT_MESSAGE_FIELD: "old answer",
    }))
    .unwrap();
    runtime
        .ingest_hook_with_context(&late, "server-b", Some(&topology))
        .unwrap();

    let replies = replies.lock().unwrap();
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].server_identity, "server-a");
    assert_eq!(replies[0].pane_id, "%7");
}

#[test]
fn an_unwaited_codex_parent_reply_is_handed_on_at_its_only_stop() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("codex-unwaited-reply-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    let replies = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&replies);
    let runtime = AgentRuntime::isolated_with_sink(
        path.clone(),
        Box::new(move |reply| sink.lock().unwrap().push(reply)),
    );
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let mut start = event("start", 0, "SubagentStart");
    start.payload_json = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "SubagentStart",
        adapters::CODEX_SUBAGENT_ID_FIELD: "child",
        adapters::CODEX_APPROVAL_TURN_ID_FIELD: "turn-child",
    }))
    .unwrap();
    runtime
        .ingest_hook_with_context(&start, "server-a", Some(&topology))
        .unwrap();
    let secret = "The one Codex parent reply.";
    let mut stop = event("parent-stop", 0, "Stop");
    stop.payload_json = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "Stop",
        adapters::LAST_ASSISTANT_MESSAGE_FIELD: secret,
    }))
    .unwrap();
    let working = runtime
        .ingest_hook_with_context(&stop, "server-a", Some(&topology))
        .unwrap();
    assert_eq!(
        working.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    assert_eq!(replies.lock().unwrap()[0].text, secret);

    let mut child_stop = event("child-stop", 0, "SubagentStop");
    child_stop.payload_json = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "SubagentStop",
        adapters::CODEX_SUBAGENT_ID_FIELD: "child",
        adapters::CODEX_APPROVAL_TURN_ID_FIELD: "turn-child",
    }))
    .unwrap();
    let finished = runtime
        .ingest_hook_with_context(&child_stop, "server-a", Some(&topology))
        .unwrap();
    assert_eq!(
        finished.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
    assert_eq!(replies.lock().unwrap().len(), 1);
    assert!(!fs::read_to_string(path).unwrap().contains(secret));
}

/// `agents.json` is shaped by the lifecycle alone: a Stop that carries a
/// message persists exactly what the same Stop without one persists.
#[test]
fn agents_json_is_byte_identical_with_and_without_a_reply_message() {
    let root = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase6-agent-identical-{}", uuid::Uuid::new_v4()));
    let topology = topology("codex");
    let occurred = now_millis();
    let run = |name: &str, message: Option<&str>| {
        let runtime =
            AgentRuntime::isolated_with_sink(root.join(name).join("agents.json"), Box::new(|_| {}));
        let mut stop = event("stop", 0, "Stop");
        stop.occurred_at_unix_millis = occurred;
        let mut payload = serde_json::json!({"hook_event_name": "Stop"});
        if let Some(message) = message {
            payload[adapters::LAST_ASSISTANT_MESSAGE_FIELD] = message.into();
        }
        stop.payload_json = serde_json::to_vec(&payload).unwrap();
        runtime
            .ingest_hook_with_context(&stop, "server-a", Some(&topology))
            .unwrap();
        // The wall-clock observation fields are the only legitimate
        // difference between two runs; pin them before comparing.
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join(name).join("agents.json")).unwrap())
                .unwrap();
        for agent in state["agents"].as_object_mut().unwrap().values_mut() {
            for field in [
                "hook_authority_expires_at_unix_millis",
                "lifecycle_observed_at_unix_millis",
                "lifecycle_changed_at_unix_millis",
            ] {
                agent[field] = 0.into();
            }
        }
        state
    };
    let without = run("without", None);
    let with = run("with", Some("a reply that must not be persisted"));
    assert_eq!(with, without);
    assert!(!with.to_string().contains("must not be persisted"));
    fs::remove_dir_all(root).unwrap();
}
