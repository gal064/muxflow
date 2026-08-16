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
    v1::AgentHookEvent {
        adapter: v1::AgentAdapterKind::Codex.into(),
        source_event_id: id.into(),
        source_generation: generation,
        native_session_id: "native-1".into(),
        pane_id: "%7".into(),
        payload_json: serde_json::to_vec(&serde_json::json!({"hook_event_name": name})).unwrap(),
        occurred_at_unix_millis: now_millis(),
        origin_server_identity: "server-a".into(),
        ..Default::default()
    }
}

fn topology(command: &str) -> tmux_control::TmuxSnapshot {
    tmux_control::TmuxSnapshot {
        sessions: vec![tmux_control::Session {
            id: "$1".into(),
            name: "workspace".into(),
            window_count: 1,
            attached_clients: 0,
            order: 0,
        }],
        windows: vec![tmux_control::Window {
            id: "@2".into(),
            session_id: "$1".into(),
            index: 0,
            name: "agent".into(),
            active: true,
            layout: String::new(),
            zoomed: false,
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
    let snapshot = runtime.snapshot();
    assert!(snapshot.authoritative);
    assert_eq!(snapshot.notification_watermark, snapshot.generation);
    assert_eq!(snapshot.agents[0].seen_generation, 1);
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
        state: Mutex::new(baseline_state.clone()),
        screen_observer: Mutex::new(screen::ScreenObserver::default()),
        wiring: Mutex::new(hooks::WiringCache::default()),
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

#[test]
fn unexpired_hook_authority_survives_process_reconciliation() {
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
    assert_eq!(record.authority, v1::AgentAuthority::Hook as i32);
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Blocked as i32);
}

#[test]
fn screen_fallback_classifies_but_cannot_override_current_hook() {
    let runtime = runtime("screen-authority");
    runtime
        .ingest_hook(&event("e1", 1, "UserPromptSubmit"))
        .unwrap();
    runtime
        .observe_screen("%7", b"Permission required: Allow command?", true)
        .unwrap();
    let current = &runtime.snapshot().agents[0];
    assert_eq!(current.authority, v1::AgentAuthority::Hook as i32);
    assert_eq!(current.lifecycle, v1::AgentLifecycleState::Working as i32);
}

#[test]
fn reconciliation_retires_process_exit_pane_close_and_adapter_replacement() {
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
    assert_eq!(
        replaced.agents[0].adapter,
        v1::AgentAdapterKind::ClaudeCode as i32
    );

    runtime
        .reconcile_topology(&topology("bash"), "server-a")
        .unwrap();
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
fn missing_same_server_pane_cannot_replace_a_mapped_native_agent() {
    let runtime = runtime("same-server-missing-pane");
    let topology = topology("codex");
    runtime.reconcile_topology(&topology, "server-a").unwrap();
    runtime
        .ingest_hook_with_context(
            &event("mapped-working", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let direct_before = runtime.snapshot_for("server-a").agents[0].clone();

    let mut stale = event("stale-working", 0, "UserPromptSubmit");
    stale.pane_id = "%99".into();
    stale.native_session_id = direct_before.native_session_id.clone();
    runtime
        .ingest_hook_with_context(&stale, "server-a", Some(&topology))
        .unwrap();
    assert!(
        runtime
            .ingest_hook_with_context(&stale, "server-a", Some(&topology))
            .is_err(),
        "same-server unmapped continuity must retain source-event dedupe"
    );
    let mut completed = stale.clone();
    completed.source_event_id = "stale-completed".into();
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
    assert_eq!(unmapped.native_session_id, "native-1");
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
            &event("idle", 0, "Stop"),
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
            &event("idle-seen", 0, "Stop"),
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
            .ingest_hook_with_context(&event(id, 0, name), "server-a", Some(&topology))
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
