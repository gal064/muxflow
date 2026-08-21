use super::*;

fn codex_approval_fixture(sequence: &str) -> Vec<v1::AgentHookEvent> {
    let fixture: serde_json::Value = serde_json::from_slice(include_bytes!(
        "../../../../../../tests/integration/agents/fixtures/codex-approval-sequences.json"
    ))
    .unwrap();
    fixture[sequence]
        .as_array()
        .unwrap()
        .iter()
        .map(|payload| {
            crate::hook::build_event(
                v1::AgentAdapterKind::Codex,
                serde_json::to_vec(payload).unwrap(),
                "%7",
                "server-a",
                now_millis(),
            )
            .unwrap()
        })
        .collect()
}

fn ingest_fixture(
    runtime: &AgentRuntime,
    topology: &tmux_control::TmuxSnapshot,
    event: &v1::AgentHookEvent,
) -> v1::AgentRecord {
    runtime
        .ingest_hook_with_context(event, "server-a", Some(topology))
        .unwrap()
        .agent
        .unwrap()
}

/// Codex 0.148's PermissionRequest has no tool_use_id. The canonical hook
/// boundary therefore correlates with the stable fields both sides do expose:
/// turn_id, tool_name and tool_input. A parallel tool completing must not erase
/// the approval; the corresponding PostToolUse is the first observable proof
/// that the approved tool resumed.
#[test]
fn real_codex_approval_order_stays_blocked_until_the_matching_tool_resolves() {
    let runtime = runtime("codex-real-approval-order");
    let topology = topology("codex");
    let events = codex_approval_fixture("approved");

    assert_eq!(
        ingest_fixture(&runtime, &topology, &events[0]).lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    ingest_fixture(&runtime, &topology, &events[1]);
    let blocked = ingest_fixture(&runtime, &topology, &events[2]);
    assert_eq!(blocked.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    assert_eq!(blocked.attention_kind, "blocked");
    let state_path = runtime.state_path.clone();
    assert!(
        !runtime
            .state
            .lock()
            .unwrap()
            .agents
            .values()
            .next()
            .unwrap()
            .pending_approval_keys
            .is_empty()
    );
    drop(runtime);
    let runtime = AgentRuntime::isolated(state_path);
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Blocked as i32,
        "the pending approval must survive a daemon restart"
    );

    for unrelated in &events[3..=4] {
        assert_eq!(
            ingest_fixture(&runtime, &topology, unrelated).lifecycle,
            v1::AgentLifecycleState::Blocked as i32,
            "an unrelated tool lifecycle event cannot resolve the pending approval"
        );
    }
    assert_eq!(
        ingest_fixture(&runtime, &topology, &events[5]).lifecycle,
        v1::AgentLifecycleState::Working as i32,
        "the matching PostToolUse proves approval and tool completion"
    );
    let completed = ingest_fixture(&runtime, &topology, &events[6]);
    assert_eq!(completed.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert_eq!(completed.attention_kind, "completed");
}

/// Codex exposes no distinct denial hook. A denied request ends at Stop; an
/// aborted session ends at SessionEnd. Both are supported observable terminal
/// boundaries and both must clear the durable pending state.
#[test]
fn codex_denial_and_cancellation_resolve_pending_approval() {
    for sequence in ["denied", "cancelled"] {
        let runtime = runtime(&format!("codex-{sequence}-approval"));
        let topology = topology("codex");
        let events = codex_approval_fixture(sequence);
        assert_eq!(
            ingest_fixture(&runtime, &topology, &events[0]).lifecycle,
            v1::AgentLifecycleState::Blocked as i32
        );
        let resolved = ingest_fixture(&runtime, &topology, &events[1]);
        assert_eq!(resolved.lifecycle, v1::AgentLifecycleState::Idle as i32);
        let state = runtime.state.lock().unwrap();
        assert!(
            state
                .agents
                .values()
                .next()
                .unwrap()
                .pending_approval_keys
                .is_empty()
        );
    }
}

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

/// The guard that hooks stayed the *only* writer of lifecycle.
///
/// Screen scraping was the second writer, and it was the one that could not be
/// made correct: it only ever saw panes the user had on screen, and it matched
/// vendor TUI output that changes without notice. Its failure mode was a pane
/// stuck at `working` with no reachable exit.
///
/// If a future change reintroduces any inference from pane content, presence,
/// or elapsed time, this test fails — a pane that has produced no hook must
/// read `unknown` no matter how much traffic it emits or how long it sits.
#[test]
fn a_pane_without_hooks_stays_unknown_no_matter_what_it_puts_on_screen() {
    let runtime = runtime("hookless-stays-unknown");
    let topology = topology("codex");
    runtime.reconcile_topology(&topology, "server-a").unwrap();
    let unknown = || runtime.snapshot_for("server-a").agents[0].lifecycle;
    assert_eq!(unknown(), v1::AgentLifecycleState::Unknown as i32);

    // Reconciling repeatedly is the closest thing left to "the host looked at
    // the pane again". Presence is re-proved every time and still says nothing.
    for _ in 0..3 {
        runtime.reconcile_topology(&topology, "server-a").unwrap();
        assert_eq!(unknown(), v1::AgentLifecycleState::Unknown as i32);
    }

    // Nor does age move it. Staleness only ever withdraws a claim; it has no
    // claim to withdraw here.
    {
        let mut state = runtime.state.lock().unwrap();
        for record in state.agents.values_mut() {
            record.updated_at_unix_millis = now_millis() - STALE_WORKING_TTL_MILLIS * 10;
            record.lifecycle_observed_at_unix_millis = now_millis() - STALE_WORKING_TTL_MILLIS * 10;
        }
    }
    assert!(runtime.sweep_stale().is_empty());
    assert_eq!(unknown(), v1::AgentLifecycleState::Unknown as i32);
}

/// A killed agent's row disappears, and does so on process evidence alone —
/// no tmux topology change is required, which is what `reconcile_topology`
/// waits for and why a process-tree-detected agent used to claim `Working`
/// forever after exiting.
#[test]
fn a_departed_process_retires_a_working_agent_and_publishes_the_retirement() {
    let runtime = runtime("departed");
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
        runtime
            .retire_departed_from(&topology, "server-a")
            .is_empty(),
        "a detected process is not departed"
    );

    // The agent exits. The pane survives it, which is exactly the case
    // reconciliation cannot see.
    let mut departed = topology.clone();
    departed.panes[0].current_command = "zsh".into();
    departed.panes[0].start_command = "zsh".into();
    let events = runtime.retire_departed_from(&departed, "server-a");
    assert_eq!(events.len(), 1);
    assert!(events[0].agent.is_none(), "there is no record left to send");
    assert_eq!(events[0].retired_agent_ids, vec![working.agent_id.clone()]);
    assert_eq!(events[0].reason, "departed");
    assert!(!events[0].notify);
    assert!(runtime.snapshot_for("server-a").agents.is_empty());
}

/// The conservatism half. A false positive deletes a live agent's row, so
/// every guard that stops one is pinned here.
#[test]
fn retirement_never_convicts_an_agent_on_evidence_that_cannot_see_it() {
    let runtime = runtime("departed-conservative");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    // An unmapped hook-only record: no pane to look for, so no pane's absence
    // can convict it. It lives on its hook lease, as reconciliation also allows.
    let mut unmapped = event("unmapped", 0, "UserPromptSubmit");
    unmapped.native_session_id = "unmapped-session".into();
    unmapped.pane_id = "%404".into();
    runtime
        .ingest_hook_with_context(&unmapped, "server-a", Some(&topology))
        .unwrap();
    assert_eq!(runtime.snapshot_for("server-a").agents.len(), 2);

    let mut departed = topology.clone();
    departed.panes[0].current_command = "zsh".into();
    departed.panes[0].start_command = "zsh".into();

    // Another tmux server's topology is evidence about that server, not this
    // one. Judging this host's records against it would empty the list.
    assert!(
        runtime
            .retire_departed_from(&departed, "server-b")
            .is_empty(),
        "a foreign server's snapshot convicts nobody"
    );
    assert_eq!(runtime.snapshot_for("server-a").agents.len(), 2);

    let events = runtime.retire_departed_from(&departed, "server-a");
    assert_eq!(events.len(), 1);
    let survivors = runtime.snapshot_for("server-a").agents;
    assert_eq!(survivors.len(), 1);
    assert_eq!(
        survivors[0].route.as_ref().unwrap().pane_id,
        "",
        "the unmapped hook-only record survived on its lease"
    );
}

/// An idle agent is never retired on absence. It is claiming nothing that
/// outliving its process would turn into a lie, and its row is still how the
/// user reaches that pane. Retirement exists to end a false `working`, not to
/// garbage-collect the list — reconciliation already owns that.
#[test]
fn retirement_leaves_an_idle_agent_alone() {
    let runtime = runtime("departed-idle");
    let topology = topology("codex");
    for name in ["UserPromptSubmit", "Stop"] {
        runtime
            .ingest_hook_with_context(&event(name, 0, name), "server-a", Some(&topology))
            .unwrap();
    }
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
    let mut departed = topology.clone();
    departed.panes[0].current_command = "zsh".into();
    departed.panes[0].start_command = "zsh".into();
    assert!(
        runtime
            .retire_departed_from(&departed, "server-a")
            .is_empty()
    );
    assert_eq!(runtime.snapshot_for("server-a").agents.len(), 1);
}

/// The headless case, which is the whole point of moving the sweep onto the
/// daemon: no subscriber, no hub, nothing connected — and the state still
/// stops claiming to work.
///
/// `maintain()` is deliberately not called here. It resolves
/// `AgentRuntime::global()`, which would read the developer's own agent store
/// instead of this test's; `fallback.rs` records that class of bug. The
/// isolated runtime with no hub registered *is* the headless condition.
#[test]
fn a_working_agent_decays_with_nothing_connected_to_ask_for_it() {
    let runtime = runtime("headless-decay");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    {
        let mut state = runtime.state.lock().unwrap();
        for record in state.agents.values_mut() {
            record.lifecycle_observed_at_unix_millis =
                now_millis() - STALE_WORKING_TTL_MILLIS - 1_000;
        }
    }
    let events = runtime.sweep_stale();
    assert_eq!(
        events.len(),
        1,
        "no subscriber is not a reason to keep lying"
    );
    assert_eq!(events[0].reason, "stale");
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Unknown as i32
    );
    // Publishing with no hub registered is a no-op rather than a failure,
    // which is what lets the daemon run this pass unconditionally.
    for event in events {
        publish(event);
    }
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

#[test]
fn a_terminal_hook_can_never_preserve_an_inconsistent_working_state() {
    let runtime = runtime("terminal-working-invariant");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    {
        // Reproduce the live schema-2 record: an earlier terminal event was
        // remembered while lifecycle still claimed Working.
        let mut state = runtime.state.lock().unwrap();
        let record = state.agents.values_mut().next().unwrap();
        record.hook_terminal = true;
        record.lifecycle = v1::AgentLifecycleState::Working as i32;
    }

    let stopped = runtime
        .ingest_hook_with_context(&event("stop", 0, "Stop"), "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(stopped.lifecycle, v1::AgentLifecycleState::Idle as i32);

    let late = runtime
        .ingest_hook_with_context(
            &event("late-tool", 0, "PostToolUse"),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap();
    assert_eq!(late.lifecycle, v1::AgentLifecycleState::Idle as i32);
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
