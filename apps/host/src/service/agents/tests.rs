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
    assert!(runtime.reconcile_topology(&departed, "server-a").unwrap());
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
        state: Mutex::new(baseline_state.clone()),
        wiring: Mutex::new(hooks::WiringCache::default()),
        departure_misses: Mutex::new(BTreeMap::new()),
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
    assert_eq!(handed[0].agent_id, done.agent.as_ref().unwrap().agent_id);
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
