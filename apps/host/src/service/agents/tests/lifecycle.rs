use super::*;

fn prompt_for_turn(
    id: &str,
    turn_id: &str,
    reviewer: Option<serde_json::Value>,
) -> v1::AgentHookEvent {
    let mut prompt = event(id, 0, "UserPromptSubmit");
    let mut payload = serde_json::json!({"hook_event_name": "UserPromptSubmit"});
    payload[adapters::CODEX_APPROVAL_TURN_ID_FIELD] = turn_id.into();
    if let Some(reviewer) = reviewer {
        payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = reviewer;
    }
    prompt.payload_json = serde_json::to_vec(&payload).unwrap();
    prompt
}

fn permission_for_turn(id: &str, turn_id: &str) -> v1::AgentHookEvent {
    let mut permission = event(id, 0, "PermissionRequest");
    let mut payload = serde_json::json!({"hook_event_name": "PermissionRequest"});
    payload[adapters::CODEX_APPROVAL_TURN_ID_FIELD] = turn_id.into();
    permission.payload_json = serde_json::to_vec(&payload).unwrap();
    permission
}

fn codex_tool_event(id: &str, event_name: &str, tool_name: &str) -> v1::AgentHookEvent {
    let mut tool = event(id, 0, event_name);
    tool.payload_json = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": event_name,
        "tool_name": tool_name,
    }))
    .unwrap();
    tool
}

#[test]
fn auto_review_cache_is_durable_and_scoped_to_one_exact_turn() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("auto-review-cache-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    let topology = topology("codex");
    {
        let runtime = AgentRuntime::isolated(path.clone());
        let confirmed = runtime
            .ingest_hook_with_context(
                &prompt_for_turn("confirmed", "turn-1", Some("auto_review".into())),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        assert!(!confirmed.notify);
        assert_eq!(
            confirmed.agent.unwrap().lifecycle,
            v1::AgentLifecycleState::Working as i32
        );

        let cached = runtime
            .ingest_hook_with_context(
                &permission_for_turn("cached", "turn-1"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        assert!(!cached.notify);
        assert_eq!(
            cached.agent.unwrap().lifecycle,
            v1::AgentLifecycleState::Working as i32
        );
    }

    let restarted = AgentRuntime::isolated(path);
    let cached = restarted
        .ingest_hook_with_context(
            &permission_for_turn("cached-after-restart", "turn-1"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!cached.notify);
    assert_eq!(
        cached.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );

    let cold_turn = restarted
        .ingest_hook_with_context(
            &permission_for_turn("different-turn", "turn-2"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(cold_turn.notify);
    assert_eq!(
        cold_turn.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Blocked as i32
    );
}

#[test]
fn explicit_non_auto_turn_start_clears_the_same_turn_cache() {
    for reviewer in ["user", "future_reviewer"] {
        let runtime = runtime(&format!("auto-review-override-{reviewer}"));
        let topology = topology("codex");
        runtime
            .ingest_hook_with_context(
                &prompt_for_turn("confirmed", "turn-1", Some("auto_review".into())),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        let prompt = runtime
            .ingest_hook_with_context(
                &prompt_for_turn("overridden", "turn-1", Some(reviewer.into())),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        assert!(!prompt.notify);
        assert_eq!(
            prompt.agent.unwrap().lifecycle,
            v1::AgentLifecycleState::Working as i32
        );
        assert!(
            runtime
                .state
                .lock()
                .unwrap()
                .agents
                .values()
                .all(|record| record.codex_auto_review_turn_id.is_empty())
        );
        let permission = runtime
            .ingest_hook_with_context(
                &permission_for_turn("permission", "turn-1"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        assert!(permission.notify);
        assert_eq!(
            permission.agent.unwrap().lifecycle,
            v1::AgentLifecycleState::Blocked as i32
        );
    }
}

#[test]
fn malformed_reviewer_cannot_reuse_the_same_turn_cache() {
    let runtime = runtime("auto-review-malformed-reviewer");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &prompt_for_turn("confirmed", "turn-1", Some("auto_review".into())),
            "server-a",
            Some(&topology),
        )
        .unwrap();

    let malformed = prompt_for_turn("malformed", "turn-1", Some(serde_json::json!({})));
    let prompt = runtime
        .ingest_hook_with_context(&malformed, "server-a", Some(&topology))
        .unwrap();
    assert!(!prompt.notify);
    assert_eq!(
        prompt.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    assert!(
        runtime
            .state
            .lock()
            .unwrap()
            .agents
            .values()
            .all(|record| record.codex_auto_review_turn_id.is_empty())
    );
    let blocked = runtime
        .ingest_hook_with_context(
            &permission_for_turn("permission", "turn-1"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(blocked.notify);
    assert_eq!(
        blocked.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Blocked as i32
    );
}

#[test]
fn incomplete_turn_start_cannot_reuse_the_cache() {
    for (turn_id, reviewer) in [
        ("turn-1", None),
        ("", Some(serde_json::json!("auto_review"))),
    ] {
        let runtime = runtime(&format!("auto-review-cache-miss-{turn_id}"));
        let topology = topology("codex");
        runtime
            .ingest_hook_with_context(
                &prompt_for_turn("confirmed", "turn-1", Some("auto_review".into())),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        runtime
            .ingest_hook_with_context(
                &prompt_for_turn("incomplete", turn_id, reviewer),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        let missed = runtime
            .ingest_hook_with_context(
                &permission_for_turn("missed", "turn-1"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        assert!(missed.notify);
        assert_eq!(
            missed.agent.unwrap().lifecycle,
            v1::AgentLifecycleState::Blocked as i32
        );
    }
}

#[test]
fn auto_review_approval_stays_working_and_only_stop_requests_attention() {
    let runtime = runtime("auto-review-approved");
    let topology = topology("codex");
    let prompt = prompt_for_turn("prompt", "turn-1", Some("auto_review".into()));
    assert!(
        !runtime
            .ingest_hook_with_context(&prompt, "server-a", Some(&topology))
            .unwrap()
            .notify
    );
    assert!(
        !runtime
            .ingest_hook_with_context(&event("pre", 0, "PreToolUse"), "server-a", Some(&topology),)
            .unwrap()
            .notify
    );

    let permission = runtime
        .ingest_hook_with_context(
            &permission_for_turn("permission", "turn-1"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!permission.notify);
    assert_eq!(
        permission.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );

    let resumed = runtime
        .ingest_hook_with_context(
            &event("post", 0, "PostToolUse"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!resumed.notify);
    let completed = runtime
        .ingest_hook_with_context(&event("stop", 0, "Stop"), "server-a", Some(&topology))
        .unwrap();
    assert!(completed.notify);
    assert_eq!(completed.reason, "completed");
    assert_eq!(
        completed.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn codex_question_blocks_once_and_resumes_after_its_tool_returns() {
    let runtime = runtime("codex-question");
    let topology = topology("codex");
    let prompt = runtime
        .ingest_hook_with_context(
            &prompt_for_turn("prompt", "turn-1", Some("auto_review".into())),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!prompt.notify);
    assert_eq!(
        prompt.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );

    let blocked = runtime
        .ingest_hook_with_context(
            &codex_tool_event("question", "PreToolUse", "request_user_input"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(blocked.notify);
    assert_eq!(blocked.reason, "blocked");
    let blocked_agent = blocked.agent.unwrap();
    assert_eq!(
        blocked_agent.lifecycle,
        v1::AgentLifecycleState::Blocked as i32
    );
    assert_eq!(blocked_agent.attention_kind, "blocked");

    let repeated = runtime
        .ingest_hook_with_context(
            &codex_tool_event("question-repeat", "PreToolUse", "request_user_input"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!repeated.notify);
    assert_eq!(
        repeated.agent.unwrap().attention_generation,
        blocked_agent.attention_generation
    );

    let resumed = runtime
        .ingest_hook_with_context(
            &codex_tool_event("answer", "PostToolUse", "request_user_input"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!resumed.notify);
    assert_eq!(
        resumed.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );

    let completed = runtime
        .ingest_hook_with_context(&event("stop", 0, "Stop"), "server-a", Some(&topology))
        .unwrap();
    assert!(completed.notify);
    assert_eq!(completed.reason, "completed");
    let completed_agent = completed.agent.unwrap();
    assert_eq!(
        completed_agent.lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
    assert_eq!(completed_agent.attention_kind, "completed");
}

#[test]
fn auto_review_denial_stays_working_until_stop_completes_the_turn() {
    let runtime = runtime("auto-review-denied");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &prompt_for_turn("prompt", "turn-1", Some("auto_review".into())),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let permission = runtime
        .ingest_hook_with_context(
            &permission_for_turn("permission", "turn-1"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!permission.notify);
    assert_eq!(
        permission.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );

    let completed = runtime
        .ingest_hook_with_context(&event("stop", 0, "Stop"), "server-a", Some(&topology))
        .unwrap();
    assert!(completed.notify);
    assert_eq!(completed.reason, "completed");
    assert_eq!(completed.agent.unwrap().attention_kind, "completed");
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
            .with_valid_input_target(&working.agent_id, "%7", || Ok(()))
            .is_ok()
    );
    assert!(
        runtime
            .with_valid_input_target(&working.agent_id, "%8", || Ok(()))
            .is_err()
    );
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
    let events = runtime.retire_departed_from(&departed, "server-a");
    assert_eq!(events.len(), 1);
    assert!(events[0].agent.is_none(), "there is no record left to send");
    assert_eq!(events[0].retired_agent_ids, vec![working.agent_id.clone()]);
    assert_eq!(events[0].reason, "departed");
    assert!(!events[0].notify);
    assert!(runtime.snapshot_for("server-a").agents.is_empty());
    assert!(
        runtime
            .with_valid_input_target(&working.agent_id, "%7", || Ok(()))
            .is_err()
    );
}

#[test]
fn a_transient_process_scan_miss_does_not_retire_a_working_agent() {
    let runtime = runtime("departed-transient-miss");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let mut missed = topology.clone();
    missed.panes[0].current_command = "zsh".into();
    missed.panes[0].start_command = "zsh".into();

    assert!(runtime.retire_departed_from(&missed, "server-a").is_empty());
    assert!(
        runtime
            .retire_departed_from(&topology, "server-a")
            .is_empty()
    );
    assert!(runtime.retire_departed_from(&missed, "server-a").is_empty());
    assert!(runtime.retire_departed_from(&missed, "server-a").is_empty());
    assert_eq!(runtime.snapshot_for("server-a").agents.len(), 1);

    let events = runtime.retire_departed_from(&missed, "server-a");
    assert_eq!(events.len(), 1, "only three consecutive misses retire");
}

#[test]
fn topology_process_detection_breaks_a_run_of_maintenance_misses() {
    let runtime = runtime("departed-topology-positive");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let mut missed = topology.clone();
    missed.panes[0].current_command = "zsh".into();
    missed.panes[0].start_command = "zsh".into();

    assert!(runtime.retire_departed_from(&missed, "server-a").is_empty());
    runtime.reconcile_topology(&topology, "server-a").unwrap();
    assert!(runtime.retire_departed_from(&missed, "server-a").is_empty());
    assert!(runtime.retire_departed_from(&missed, "server-a").is_empty());
    assert_eq!(runtime.snapshot_for("server-a").agents.len(), 1);
    assert_eq!(runtime.retire_departed_from(&missed, "server-a").len(), 1);
}

#[test]
fn topology_reconciliation_keeps_mid_turn_identity_on_a_live_pane_scan_miss() {
    let runtime = runtime("reconcile-transient-miss");
    let topology = topology("codex");
    let agent_id = runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap()
        .agent
        .unwrap()
        .agent_id;
    let mut missed = topology;
    missed.panes[0].current_command = "zsh".into();
    missed.panes[0].start_command = "zsh".into();

    assert!(!runtime.reconcile_topology(&missed, "server-a").unwrap());
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].agent_id,
        agent_id
    );
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

/// Idle is when the user is most likely to begin the next prompt. It gets the
/// same transient-scan protection as a mid-turn agent, while a real exit still
/// converges to removal.
#[test]
fn idle_agent_requires_consecutive_misses_before_retirement() {
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
    runtime.reconcile_topology(&topology, "server-a").unwrap();
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
    assert_eq!(runtime.snapshot_for("server-a").agents.len(), 1);
    assert_eq!(runtime.retire_departed_from(&departed, "server-a").len(), 1);
    assert!(runtime.snapshot_for("server-a").agents.is_empty());
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
    claude_hook(
        &runtime,
        &topology,
        "late-intermediate-stop",
        serde_json::json!({
            "hook_event_name": "Stop",
            "has_running_subagent": true
        }),
    );
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert!(record.hook_terminal);
    assert!(!record.claude_has_running_subagent);
}

#[test]
fn an_active_subagent_never_hides_a_real_permission_block() {
    let runtime = runtime("subagent-permission");
    let topology = topology("claude");
    claude_hook(
        &runtime,
        &topology,
        "prompt",
        serde_json::json!({"hook_event_name": "UserPromptSubmit"}),
    );
    claude_hook(
        &runtime,
        &topology,
        "parent-stop",
        serde_json::json!({
            "hook_event_name": "Stop",
            "has_running_subagent": true
        }),
    );
    let blocked = claude_hook(
        &runtime,
        &topology,
        "permission",
        serde_json::json!({"hook_event_name": "PermissionRequest"}),
    );
    assert!(blocked.notify);
    let blocked = blocked.agent.unwrap();
    assert_eq!(blocked.lifecycle, v1::AgentLifecycleState::Blocked as i32);

    let repeated = claude_hook(
        &runtime,
        &topology,
        "idle-notification",
        serde_json::json!({
            "hook_event_name": "Notification",
            "notification_type": "idle_prompt"
        }),
    );
    assert!(
        !repeated.notify,
        "the same block must not earn attention twice"
    );
    let repeated = repeated.agent.unwrap();
    assert_eq!(repeated.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    assert_eq!(repeated.attention_generation, blocked.attention_generation);

    let resumed = claude_hook(
        &runtime,
        &topology,
        "permission-resolved",
        serde_json::json!({"hook_event_name": "PostToolUse"}),
    )
    .agent
    .unwrap();
    assert_eq!(resumed.lifecycle, v1::AgentLifecycleState::Working as i32);
}

#[test]
fn the_active_subagent_idle_guard_survives_a_daemon_restart() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("claude-subagent-restart-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    let topology = topology("claude");
    {
        let runtime = AgentRuntime::isolated(path.clone());
        claude_hook(
            &runtime,
            &topology,
            "prompt",
            serde_json::json!({"hook_event_name": "UserPromptSubmit"}),
        );
        claude_hook(
            &runtime,
            &topology,
            "parent-stop",
            serde_json::json!({
                "hook_event_name": "Stop",
                "has_running_subagent": true
            }),
        );
    }

    let restarted = AgentRuntime::isolated(path);
    let idle = claude_hook(
        &restarted,
        &topology,
        "idle-after-restart",
        serde_json::json!({
            "hook_event_name": "Notification",
            "notification_type": "idle_prompt"
        }),
    );
    assert!(!idle.notify);
    let idle = idle.agent.unwrap();
    assert_eq!(idle.lifecycle, v1::AgentLifecycleState::Working as i32);
    assert!(
        restarted
            .state
            .lock()
            .unwrap()
            .agents
            .get(&idle.agent_id)
            .unwrap()
            .claude_has_running_subagent
    );
}

/// Ingests one Claude Code hook with a payload written out in full, which is
/// what the notification and background-work cases turn on.
fn claude_hook(
    runtime: &AgentRuntime,
    topology: &tmux_control::TmuxSnapshot,
    id: &str,
    payload: serde_json::Value,
) -> v1::AgentEvent {
    let mut value = event(id, 0, "");
    value.adapter = v1::AgentAdapterKind::ClaudeCode.into();
    value.adapter_id = "claude-code".into();
    value.payload_json = serde_json::to_vec(&payload).unwrap();
    runtime
        .ingest_hook_with_context(&value, "server-a", Some(topology))
        .unwrap()
}

/// A `Stop` ends the turn even when background tasks outlive it, and the
/// terminal flag that idle Stop sets is what keeps Claude Code's routine idle
/// notification — which fires roughly a minute after every idle turn — from
/// being read as an agent asking for a human.
#[test]
fn background_work_neither_extends_the_turn_nor_disarms_the_notification_guard() {
    let runtime = runtime("stop-with-background-work");
    let topology = topology("claude");
    claude_hook(
        &runtime,
        &topology,
        "prompt",
        serde_json::json!({"hook_event_name": "UserPromptSubmit"}),
    );
    let stopped = claude_hook(
        &runtime,
        &topology,
        "stop",
        serde_json::json!({
            "hook_event_name": "Stop",
            "background_tasks": [{"command": "sleep 300", "status": "running"}]
        }),
    )
    .agent
    .unwrap();
    assert_eq!(stopped.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert!(
        runtime
            .state
            .lock()
            .unwrap()
            .agents
            .get(&stopped.agent_id)
            .unwrap()
            .hook_terminal,
        "an idle Stop is what arms the guard against late events"
    );

    claude_hook(
        &runtime,
        &topology,
        "idle-notification",
        serde_json::json!({
            "hook_event_name": "Notification",
            "notification_type": "idle_prompt"
        }),
    );
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Idle as i32,
        "the idle nag after a finished turn is not a blocked agent"
    );
}

/// Claude's parent emits intermediate Stops while background subagents are
/// running. Those Stops and each subagent completion remain within one working
/// turn. Only the parent's final empty Stop completes it and arms the terminal
/// notification guard.
#[test]
fn running_subagents_keep_the_parent_working_until_its_final_stop() {
    let runtime = runtime("running-subagents");
    let topology = topology("claude");
    let prompt = claude_hook(
        &runtime,
        &topology,
        "prompt",
        serde_json::json!({"hook_event_name": "UserPromptSubmit"}),
    )
    .agent
    .unwrap();
    let attention_before = prompt.attention_generation;

    for (id, payload) in [
        (
            "parent-stop-three",
            serde_json::json!({
                "hook_event_name": "Stop",
                "has_running_subagent": true
            }),
        ),
        (
            "idle-notification-during-subagents",
            serde_json::json!({
                "hook_event_name": "Notification",
                "notification_type": "idle_prompt"
            }),
        ),
        (
            "short-stop",
            serde_json::json!({"hook_event_name": "SubagentStop"}),
        ),
        (
            "parent-stop-two",
            serde_json::json!({
                "hook_event_name": "Stop",
                "has_running_subagent": true
            }),
        ),
        (
            "medium-stop",
            serde_json::json!({"hook_event_name": "SubagentStop"}),
        ),
        (
            "parent-stop-one",
            serde_json::json!({
                "hook_event_name": "Stop",
                "has_running_subagent": true
            }),
        ),
        (
            "long-stop",
            serde_json::json!({"hook_event_name": "SubagentStop"}),
        ),
    ] {
        let event = claude_hook(&runtime, &topology, id, payload);
        assert!(!event.notify, "{id} must not report completion");
        let record = event.agent.unwrap();
        assert_eq!(
            record.lifecycle,
            v1::AgentLifecycleState::Working as i32,
            "{id} must keep the parent working"
        );
        assert_eq!(record.attention_generation, attention_before);
        assert!(
            !runtime
                .state
                .lock()
                .unwrap()
                .agents
                .get(&record.agent_id)
                .unwrap()
                .hook_terminal,
            "{id} must not close the turn"
        );
        assert!(
            runtime
                .state
                .lock()
                .unwrap()
                .agents
                .get(&record.agent_id)
                .unwrap()
                .claude_has_running_subagent,
            "{id} must retain the active-subagent guard"
        );
    }

    let finished = claude_hook(
        &runtime,
        &topology,
        "final-stop",
        serde_json::json!({
            "hook_event_name": "Stop",
            "has_running_subagent": false
        }),
    );
    assert!(finished.notify);
    let finished = finished.agent.unwrap();
    assert_eq!(finished.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert_eq!(finished.attention_generation, attention_before + 1);
    assert_eq!(finished.attention_kind, "completed");
    assert!(
        runtime
            .state
            .lock()
            .unwrap()
            .agents
            .get(&finished.agent_id)
            .unwrap()
            .hook_terminal
    );
    assert!(
        !runtime
            .state
            .lock()
            .unwrap()
            .agents
            .get(&finished.agent_id)
            .unwrap()
            .claude_has_running_subagent
    );

    claude_hook(
        &runtime,
        &topology,
        "idle-notification",
        serde_json::json!({
            "hook_event_name": "Notification",
            "notification_type": "idle_prompt"
        }),
    );
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

/// The other half: with no terminal Stop behind it, an idle prompt really is
/// the agent waiting on a human, and must still read as blocked.
#[test]
fn an_idle_prompt_inside_a_live_turn_still_asks_for_a_human() {
    let runtime = runtime("idle-prompt-mid-turn");
    let topology = topology("claude");
    claude_hook(
        &runtime,
        &topology,
        "prompt",
        serde_json::json!({"hook_event_name": "UserPromptSubmit"}),
    );
    let blocked = claude_hook(
        &runtime,
        &topology,
        "idle-notification",
        serde_json::json!({
            "hook_event_name": "Notification",
            "notification_type": "idle_prompt"
        }),
    )
    .agent
    .unwrap();
    assert_eq!(blocked.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    assert_eq!(blocked.attention_kind, "blocked");
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
        record.lifecycle_changed_at_unix_millis = 123;
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
    assert!(stale.lifecycle_changed_at_unix_millis > 123);
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
        pinned: false,
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
