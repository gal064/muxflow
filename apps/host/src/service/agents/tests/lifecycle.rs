use super::*;

#[test]
fn hook_expiry_retires_an_unmapped_record_without_touching_direct_detection() {
    let runtime = runtime("expired-evidence");
    let topology = topology("codex");
    runtime.reconcile_topology(&topology, "server-a").unwrap();
    let mut unmapped = event("unmapped-expiry", 0, "PermissionRequest");
    unmapped.native_session_id = "unmapped-session".into();
    unmapped.pane_id = "%101".into();
    runtime
        .ingest_hook_with_context(&unmapped, "server-a", Some(&topology))
        .unwrap();
    assert_eq!(runtime.snapshot_for("server-a").agents.len(), 2);
    {
        let mut state = runtime.state.lock().unwrap();
        state.agents.values_mut().for_each(|record| {
            if record.route.pane_id.is_empty() {
                record.hook_authority_expires_at_unix_millis = 1;
            }
        });
    }
    runtime.reconcile_topology(&topology, "server-a").unwrap();
    let snapshot = runtime.snapshot_for("server-a");
    assert_eq!(snapshot.agents.len(), 1);
    assert_eq!(snapshot.agents[0].route.as_ref().unwrap().pane_id, "%7");
}

/// The transition sequence the whole phase is judged on, driven only by
/// hook events, with the manual detection that precedes them.
#[test]
fn manual_detection_is_unknown_and_only_hooks_move_an_agent_through_its_turn() {
    let runtime = runtime("honest-lifecycle");
    let topology = topology("codex");
    runtime.reconcile_topology(&topology, "server-a").unwrap();
    let detected = &runtime.snapshot_for("server-a").agents[0];
    assert_eq!(
        detected.lifecycle,
        v1::AgentLifecycleState::Unknown as i32,
        "a running process proves presence, never activity"
    );
    assert!(detected.detected_manually);
    assert_eq!(detected.attention_generation, 0);

    let hook = |id: &str, name: &str| {
        runtime
            .ingest_hook_with_context(&event(id, 0, name), "server-a", Some(&topology))
            .unwrap()
            .agent
            .unwrap()
    };
    assert_eq!(
        hook("prompt", "UserPromptSubmit").lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    let blocked = hook("permission", "PermissionRequest");
    assert_eq!(blocked.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    assert_eq!(blocked.attention_kind, "blocked");
    assert!(blocked.attention_generation > blocked.seen_generation);
    runtime
        .mark_seen(&blocked.agent_id, blocked.attention_generation)
        .unwrap();
    assert_eq!(
        hook("resumed", "PostToolUse").lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    let done = hook("stop", "Stop");
    assert_eq!(done.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert_eq!(done.attention_kind, "completed");
    assert!(
        done.attention_generation > done.seen_generation,
        "a finished turn stays unread until its pane is looked at"
    );
    runtime
        .mark_seen(&done.agent_id, done.attention_generation)
        .unwrap();
    let seen = &runtime.snapshot_for("server-a").agents[0];
    assert_eq!(seen.seen_generation, seen.attention_generation);
    assert_eq!(
        hook("next-prompt", "UserPromptSubmit").lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
}

/// Claude Code's `StopFailure` ends a turn exactly like `Stop`. Before it
/// was taken, a failed turn left the agent working with nothing left to
/// arrive that could ever end it.
#[test]
fn a_failed_turn_ends_the_turn_and_asks_for_a_human() {
    let runtime = runtime("stop-failure");
    let topology = topology("claude");
    let claude = |id: &str, name: &str| {
        let mut value = event(id, 0, name);
        value.adapter = v1::AgentAdapterKind::ClaudeCode.into();
        value.adapter_id = "claude-code".into();
        runtime
            .ingest_hook_with_context(&value, "server-a", Some(&topology))
            .unwrap()
    };
    claude("prompt", "UserPromptSubmit");
    let failed = claude("stop-failure", "StopFailure");
    assert!(failed.notify);
    let record = failed.agent.unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert_eq!(record.attention_kind, "completed");
    // And it is terminal: a late tool event cannot revive the turn.
    claude("late-post", "PostToolUse");
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

/// A subagent finishing is the parent still working, not the parent done.
#[test]
fn a_finished_subagent_does_not_end_its_parents_turn() {
    let runtime = runtime("subagent-stop");
    let topology = topology("codex");
    for (id, name) in [("prompt", "UserPromptSubmit"), ("sub", "SubagentStop")] {
        runtime
            .ingest_hook_with_context(&event(id, 0, name), "server-a", Some(&topology))
            .unwrap();
    }
    let record = &runtime.snapshot_for("server-a").agents[0];
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
    assert_eq!(record.attention_generation, 0);
}

#[test]
fn a_working_agent_that_stops_reporting_stops_claiming_to_work() {
    let runtime = runtime("stale-working");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let working = runtime.snapshot_for("server-a").agents[0].clone();
    assert_eq!(working.lifecycle, v1::AgentLifecycleState::Working as i32);
    assert!(
        runtime.sweep_stale().is_empty(),
        "an agent that reported a moment ago is not stale"
    );

    // One millisecond inside the window is still not stale; one outside is.
    let age = |millis: i64| {
        let mut state = runtime.state.lock().unwrap();
        let record = state.agents.get_mut(&working.agent_id).unwrap();
        record.lifecycle = v1::AgentLifecycleState::Working as i32;
        record.updated_at_unix_millis = now_millis() - millis;
        record.lifecycle_observed_at_unix_millis = now_millis() - millis;
    };
    age(STALE_WORKING_TTL_MILLIS);
    assert!(runtime.sweep_stale().is_empty());

    // Moving the agent's pane is not evidence of what it is doing, and
    // reconciliation writes `updated_at` when it happens. A dead agent
    // whose pane moved must not get its silence clock reset.
    age(STALE_WORKING_TTL_MILLIS + 1_000);
    {
        let mut state = runtime.state.lock().unwrap();
        state
            .agents
            .get_mut(&working.agent_id)
            .unwrap()
            .updated_at_unix_millis = now_millis();
    }
    assert_eq!(
        runtime.sweep_stale().len(),
        1,
        "a route change reset the staleness clock"
    );
    runtime
        .ingest_hook_with_context(
            &event("re-working", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    age(STALE_WORKING_TTL_MILLIS + 1_000);
    let events = runtime.sweep_stale();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].reason, "stale");
    assert!(!events[0].notify, "going quiet is not an event to chase");
    let stale = &runtime.snapshot_for("server-a").agents[0];
    assert_eq!(stale.lifecycle, v1::AgentLifecycleState::Unknown as i32);
    assert!(runtime.sweep_stale().is_empty(), "degrading is done once");

    // The next hook of any kind restores real state.
    runtime
        .ingest_hook_with_context(
            &event("recovered", 0, "PostToolUse"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
}

/// Attention that was already earned is not erased by silence: an agent
/// that asked for a human and then went quiet is still asking.
#[test]
fn staleness_never_touches_attention_or_a_blocked_agent() {
    let runtime = runtime("stale-preserves-attention");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("blocked", 0, "PermissionRequest"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    {
        let mut state = runtime.state.lock().unwrap();
        for record in state.agents.values_mut() {
            record.updated_at_unix_millis = now_millis() - STALE_WORKING_TTL_MILLIS * 10;
        }
    }
    assert!(runtime.sweep_stale().is_empty());
    let record = &runtime.snapshot_for("server-a").agents[0];
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    assert_eq!(record.attention_kind, "blocked");
}

/// The disconnect catch-up contract: everything that happened while the
/// desktop was away is in the persisted store, and the snapshot a
/// reconnecting desktop asks for carries it — unread, and attributed.
#[test]
fn attention_earned_while_the_desktop_was_away_survives_a_daemon_restart() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase13-catch-up-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    let mut topology = topology("codex");
    let mut second_pane = topology.panes[0].clone();
    second_pane.id = "%8".into();
    topology.panes.push(second_pane);
    {
        let runtime = AgentRuntime::isolated(path.clone());
        for (id, name) in [("prompt", "UserPromptSubmit"), ("stop", "Stop")] {
            runtime
                .ingest_hook_with_context(&event(id, 0, name), "server-a", Some(&topology))
                .unwrap();
        }
        let mut blocked = event("blocked", 0, "PermissionRequest");
        blocked.pane_id = "%8".into();
        blocked.native_session_id = "native-2".into();
        runtime
            .ingest_hook_with_context(&blocked, "server-a", Some(&topology))
            .unwrap();
    }

    // A fresh runtime is what a restarted daemon, or a desktop reconnecting
    // to one that never stopped, actually reads.
    let reconnected = AgentRuntime::isolated(path);
    let snapshot = reconnected.snapshot_for("server-a");
    assert_eq!(snapshot.agents.len(), 2);
    let done = snapshot
        .agents
        .iter()
        .find(|agent| agent.attention_kind == "completed")
        .expect("the finished turn is still unread");
    assert_eq!(done.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert!(done.attention_generation > done.seen_generation);
    let blocked = snapshot
        .agents
        .iter()
        .find(|agent| agent.attention_kind == "blocked")
        .expect("the blocked agent is still blocked");
    assert_eq!(blocked.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    assert!(blocked.attention_generation > blocked.seen_generation);
    assert_eq!(
        snapshot.notification_watermark, snapshot.generation,
        "the watermark a reconnecting desktop replays against is the store's own"
    );
}

/// Routing is by exact pane ID and nothing else. On a stock tmux every
/// agent window is called `claude`, so a name that participated in routing
/// would send one agent's state to another agent's pane.
#[test]
fn identical_window_names_never_route_one_agents_state_to_another() {
    let runtime = runtime("duplicate-window-names");
    let mut topology = topology("claude");
    topology.windows[0].name = "claude".into();
    topology.windows.push(tmux_control::Window {
        id: "@3".into(),
        session_id: "$1".into(),
        index: 1,
        name: "claude".into(),
        active: false,
        layout: String::new(),
        zoomed: false,
    });
    let mut second = topology.panes[0].clone();
    second.id = "%8".into();
    second.window_id = "@3".into();
    topology.panes.push(second);
    runtime.reconcile_topology(&topology, "server-a").unwrap();

    let mut blocked = event("blocked-in-second-window", 0, "PermissionRequest");
    blocked.adapter = v1::AgentAdapterKind::ClaudeCode.into();
    blocked.adapter_id = "claude-code".into();
    blocked.pane_id = "%8".into();
    blocked.native_session_id = "native-2".into();
    runtime
        .ingest_hook_with_context(&blocked, "server-a", Some(&topology))
        .unwrap();

    let snapshot = runtime.snapshot_for("server-a");
    assert_eq!(snapshot.agents.len(), 2);
    let routed = snapshot
        .agents
        .iter()
        .find(|agent| agent.lifecycle == v1::AgentLifecycleState::Blocked as i32)
        .unwrap();
    let route = routed.route.as_ref().unwrap();
    assert_eq!(route.pane_id, "%8");
    assert_eq!(route.window_id, "@3");
    assert!(
        snapshot
            .agents
            .iter()
            .filter(|agent| agent.route.as_ref().unwrap().window_id == "@2")
            .all(|agent| agent.lifecycle == v1::AgentLifecycleState::Unknown as i32),
        "the identically named window kept its own state"
    );
}
