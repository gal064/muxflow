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

fn codex_subagent_event(id: &str, event_name: &str, agent_id: &str) -> v1::AgentHookEvent {
    codex_subagent_turn_event(id, event_name, agent_id, &format!("turn-{agent_id}"))
}

fn codex_subagent_turn_event(
    id: &str,
    event_name: &str,
    agent_id: &str,
    turn_id: &str,
) -> v1::AgentHookEvent {
    let mut hook = event(id, 0, event_name);
    hook.payload_json = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": event_name,
        "agent_id": agent_id,
        adapters::CODEX_APPROVAL_TURN_ID_FIELD: turn_id,
    }))
    .unwrap();
    hook
}

fn codex_turn_terminal(turn_id: &str, kind: &str) -> String {
    serde_json::json!({
        "type": "event_msg",
        "payload": {
            "type": kind,
            "turn_id": turn_id,
        }
    })
    .to_string()
}

fn attach_child_monitor(
    runtime: &AgentRuntime,
    home: &std::path::Path,
    transcript: &std::path::Path,
    agent_id: &str,
    turn_id: &str,
) -> String {
    let record_id = runtime
        .state
        .lock()
        .unwrap()
        .agents
        .keys()
        .next()
        .unwrap()
        .clone();
    let monitor = crate::hook::codex_transcript::TurnMonitor::open(
        &serde_json::json!({"turn_id": turn_id, "transcript_path": transcript}),
        home,
    )
    .unwrap();
    runtime.track_codex_child(
        &record_id,
        Some(CodexTurnKey {
            agent_id: agent_id.into(),
            turn_id: turn_id.into(),
        }),
        Some(monitor),
        true,
        false,
    );
    record_id
}

fn has_auto_review_turn(record: &StoredAgent, agent_id: &str, turn_id: &str) -> bool {
    record.codex_turn_reviews.iter().any(|review| {
        review.turn.agent_id == agent_id
            && review.turn.turn_id == turn_id
            && review.reviewer == CodexReviewer::AutoReview
    })
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
    assert!(!cold_turn.notify);
    assert_eq!(
        cold_turn.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
}

#[test]
fn permission_revalidates_auto_review_after_prompt_cache_miss() {
    let runtime = runtime("auto-review-revalidated-at-permission");
    let topology = topology("codex");
    let prompt = runtime
        .ingest_hook_with_context(
            &prompt_for_turn("prompt-before-context", "turn-1", None),
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
            .all(|record| record.codex_turn_reviews.is_empty())
    );

    let mut permission = permission_for_turn("permission-with-context", "turn-1");
    let mut payload: serde_json::Value = serde_json::from_slice(&permission.payload_json).unwrap();
    payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "auto_review".into();
    permission.payload_json = serde_json::to_vec(&payload).unwrap();
    let revalidated = runtime
        .ingest_hook_with_context(&permission, "server-a", Some(&topology))
        .unwrap();
    assert!(!revalidated.notify);
    assert_eq!(
        revalidated.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    assert!(
        runtime
            .state
            .lock()
            .unwrap()
            .agents
            .values()
            .all(|record| has_auto_review_turn(record, "", "turn-1"))
    );

    let cached = runtime
        .ingest_hook_with_context(
            &permission_for_turn("permission-after-revalidation", "turn-1"),
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

#[test]
fn overlapping_subagents_keep_independent_auto_review_evidence() {
    let runtime = runtime("overlapping-auto-review-turns");
    let topology = topology("codex");
    for (event_id, agent_id, turn_id) in [
        ("child-1-observed", "child-1", "turn-1"),
        ("child-2-observed", "child-2", "turn-2"),
    ] {
        let mut permission = permission_for_turn(event_id, turn_id);
        let mut payload: serde_json::Value =
            serde_json::from_slice(&permission.payload_json).unwrap();
        payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "auto_review".into();
        payload[adapters::CODEX_SUBAGENT_ID_FIELD] = agent_id.into();
        permission.payload_json = serde_json::to_vec(&payload).unwrap();
        assert!(
            !runtime
                .ingest_hook_with_context(&permission, "server-a", Some(&topology))
                .unwrap()
                .notify
        );
    }

    for (event_id, agent_id, turn_id) in [
        ("child-1-cached", "child-1", "turn-1"),
        ("child-2-cached", "child-2", "turn-2"),
    ] {
        let mut permission = permission_for_turn(event_id, turn_id);
        let mut payload: serde_json::Value =
            serde_json::from_slice(&permission.payload_json).unwrap();
        payload[adapters::CODEX_SUBAGENT_ID_FIELD] = agent_id.into();
        permission.payload_json = serde_json::to_vec(&payload).unwrap();
        let cached = runtime
            .ingest_hook_with_context(&permission, "server-a", Some(&topology))
            .unwrap();
        assert!(!cached.notify);
        assert_eq!(
            cached.agent.unwrap().lifecycle,
            v1::AgentLifecycleState::Working as i32
        );
    }
}

#[test]
fn transcript_terminal_clears_a_cancelled_manual_permission_without_a_stop_hook() {
    use std::io::Write as _;

    let runtime = runtime("manual-permission-transcript-terminal");
    let topology = topology("codex");
    let mut permission = permission_for_turn("manual", "turn-1");
    let mut payload: serde_json::Value = serde_json::from_slice(&permission.payload_json).unwrap();
    payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "user".into();
    permission.payload_json = serde_json::to_vec(&payload).unwrap();
    let blocked = runtime
        .ingest_hook_with_context(&permission, "server-a", Some(&topology))
        .unwrap();
    let blocked = blocked.agent.unwrap();
    assert_eq!(blocked.lifecycle, v1::AgentLifecycleState::Blocked as i32);

    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/08");
    std::fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    std::fs::write(&transcript, b"\n").unwrap();
    let monitor = crate::hook::codex_transcript::TurnMonitor::open(
        &serde_json::json!({
            "turn_id": "turn-1",
            "transcript_path": transcript,
        }),
        home.path(),
    )
    .unwrap();
    runtime.track_codex_permission(
        &blocked.agent_id,
        "PermissionRequest",
        Some(CodexTurnKey {
            agent_id: String::new(),
            turn_id: "turn-1".into(),
        }),
        Some(monitor),
        runtime.state.lock().unwrap().agents[&blocked.agent_id].lifecycle_changed_at_unix_millis,
        ("turn-1", None),
    );

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "turn_aborted", "turn_id": "turn-1"},
        })
    )
    .unwrap();
    file.sync_all().unwrap();

    let resolved = runtime.sweep_codex_permission_terminals();
    assert_eq!(resolved.len(), 1);
    assert!(!resolved[0].notify);
    assert_eq!(resolved[0].reason, "permission_turn_aborted");
    let idle = resolved[0].agent.as_ref().unwrap();
    assert_eq!(idle.lifecycle, v1::AgentLifecycleState::Idle as i32);
    let state = runtime.state.lock().unwrap();
    let record = &state.agents[&blocked.agent_id];
    assert!(record.hook_terminal);
    assert!(
        record
            .codex_terminal_root_turn_ids
            .iter()
            .any(|turn| turn == "turn-1")
    );
    drop(state);
    assert!(runtime.sweep_codex_permission_terminals().is_empty());
}

#[test]
fn a_new_root_turn_disarms_a_displaced_child_permission_monitor() {
    use std::io::Write as _;

    let runtime = runtime("displaced-permission-monitor");
    let topology = topology("codex");
    let mut permission_a = permission_for_turn("permission-a", "turn-a");
    let mut payload: serde_json::Value =
        serde_json::from_slice(&permission_a.payload_json).unwrap();
    payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "user".into();
    permission_a.payload_json = serde_json::to_vec(&payload).unwrap();
    let blocked = runtime
        .ingest_hook_with_context(&permission_a, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();

    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/13");
    std::fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    std::fs::write(&transcript, b"\n").unwrap();
    let monitor = crate::hook::codex_transcript::TurnMonitor::open(
        &serde_json::json!({
            "turn_id": "turn-a",
            "transcript_path": transcript,
        }),
        home.path(),
    )
    .unwrap();
    runtime.track_codex_permission(
        &blocked.agent_id,
        "PermissionRequest",
        Some(CodexTurnKey {
            agent_id: "child".into(),
            turn_id: "turn-a".into(),
        }),
        Some(monitor),
        runtime.state.lock().unwrap().agents[&blocked.agent_id].lifecycle_changed_at_unix_millis,
        ("turn-a", None),
    );

    let mut permission_b = permission_for_turn("permission-b", "turn-b");
    let mut payload: serde_json::Value =
        serde_json::from_slice(&permission_b.payload_json).unwrap();
    payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "user".into();
    permission_b.payload_json = serde_json::to_vec(&payload).unwrap();
    runtime
        .ingest_hook_with_context(&permission_b, "server-a", Some(&topology))
        .unwrap();
    assert_eq!(runtime.pending_codex_permissions.lock().unwrap().len(), 1);

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "turn_aborted", "turn_id": "turn-a"},
        })
    )
    .unwrap();
    file.sync_all().unwrap();

    assert!(runtime.sweep_codex_permission_terminals().is_empty());
    assert!(runtime.pending_codex_permissions.lock().unwrap().is_empty());
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    assert_eq!(record.codex_active_root_turn_id, "turn-b");
}

#[test]
fn an_awaiting_child_permission_rebinds_to_the_next_root() {
    use std::io::Write as _;

    let runtime = runtime("awaiting-child-permission");
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
    let mut permission = permission_for_turn("permission-b", "child-turn-b");
    let mut payload: serde_json::Value = serde_json::from_slice(&permission.payload_json).unwrap();
    payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "user".into();
    payload[adapters::CODEX_SUBAGENT_ID_FIELD] = "child".into();
    permission.payload_json = serde_json::to_vec(&payload).unwrap();
    let blocked = runtime
        .ingest_hook_with_context(&permission, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();

    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/13");
    std::fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    std::fs::write(&transcript, b"\n").unwrap();
    let monitor = crate::hook::codex_transcript::TurnMonitor::open(
        &serde_json::json!({
            "turn_id": "child-turn-b",
            "transcript_path": transcript,
        }),
        home.path(),
    )
    .unwrap();
    runtime.track_codex_permission(
        &blocked.agent_id,
        "PermissionRequest",
        Some(CodexTurnKey {
            agent_id: "child".into(),
            turn_id: "child-turn-b".into(),
        }),
        Some(monitor),
        runtime.state.lock().unwrap().agents[&blocked.agent_id].lifecycle_changed_at_unix_millis,
        ("", None),
    );

    runtime
        .ingest_hook_with_context(
            &event_for_turn("stop-b", "Stop", "turn-b"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    {
        let state = runtime.state.lock().unwrap();
        let record = state.agents.values().next().unwrap();
        assert_eq!(record.lifecycle, v1::AgentLifecycleState::Blocked as i32);
        assert!(record.codex_parent_stopped_for_subagents);
        assert_eq!(record.codex_subagent_root_turn_ids["child"], "turn-b");
    }

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "turn_aborted", "turn_id": "child-turn-b"},
        })
    )
    .unwrap();
    file.sync_all().unwrap();

    let resolved = runtime.sweep_codex_permission_terminals();
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        resolved[0].agent.as_ref().unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn a_resumed_child_disarms_its_older_awaiting_permission_monitor() {
    use std::io::Write as _;

    let runtime = runtime("resumed-awaiting-child-permission");
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
    let mut permission = permission_for_turn("permission-b", "child-turn-b");
    let mut payload: serde_json::Value = serde_json::from_slice(&permission.payload_json).unwrap();
    payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "user".into();
    payload[adapters::CODEX_SUBAGENT_ID_FIELD] = "child".into();
    permission.payload_json = serde_json::to_vec(&payload).unwrap();
    let blocked = runtime
        .ingest_hook_with_context(&permission, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();

    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/13");
    std::fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    std::fs::write(&transcript, b"\n").unwrap();
    let monitor = crate::hook::codex_transcript::TurnMonitor::open(
        &serde_json::json!({
            "turn_id": "child-turn-b",
            "transcript_path": transcript,
        }),
        home.path(),
    )
    .unwrap();
    runtime.track_codex_permission(
        &blocked.agent_id,
        "PermissionRequest",
        Some(CodexTurnKey {
            agent_id: "child".into(),
            turn_id: "child-turn-b".into(),
        }),
        Some(monitor),
        runtime.state.lock().unwrap().agents[&blocked.agent_id].lifecycle_changed_at_unix_millis,
        ("", None),
    );

    runtime
        .ingest_hook_with_context(
            &child_event_for_turn("child-c", "SubagentStart", "child", "child-turn-c"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &event_for_turn("stop-b", "Stop", "turn-b"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(runtime.pending_codex_permissions.lock().unwrap().is_empty());

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "turn_aborted", "turn_id": "child-turn-b"},
        })
    )
    .unwrap();
    file.sync_all().unwrap();
    assert!(runtime.sweep_codex_permission_terminals().is_empty());
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    assert_eq!(record.codex_running_subagents["child"], "child-turn-c");
}

#[test]
fn a_root_permission_terminal_waits_for_its_live_children() {
    use std::io::Write as _;

    let runtime = runtime("root-permission-with-child");
    let topology = topology("codex");
    for hook in [
        event_for_turn("prompt", "UserPromptSubmit", "turn-a"),
        child_event_for_turn("child", "SubagentStart", "child", "child-turn"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let mut permission = permission_for_turn("permission", "turn-a");
    let mut payload: serde_json::Value = serde_json::from_slice(&permission.payload_json).unwrap();
    payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "user".into();
    permission.payload_json = serde_json::to_vec(&payload).unwrap();
    let blocked = runtime
        .ingest_hook_with_context(&permission, "server-a", Some(&topology))
        .unwrap()
        .agent
        .unwrap();

    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/13");
    std::fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    std::fs::write(&transcript, b"\n").unwrap();
    let monitor = crate::hook::codex_transcript::TurnMonitor::open(
        &serde_json::json!({"turn_id": "turn-a", "transcript_path": transcript}),
        home.path(),
    )
    .unwrap();
    runtime.track_codex_permission(
        &blocked.agent_id,
        "PermissionRequest",
        Some(CodexTurnKey {
            agent_id: String::new(),
            turn_id: "turn-a".into(),
        }),
        Some(monitor),
        runtime.state.lock().unwrap().agents[&blocked.agent_id].lifecycle_changed_at_unix_millis,
        ("turn-a", None),
    );
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "type": "event_msg",
            "payload": {"type": "turn_aborted", "turn_id": "turn-a"},
        })
    )
    .unwrap();
    file.sync_all().unwrap();

    let resolved = runtime.sweep_codex_permission_terminals();
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        resolved[0].agent.as_ref().unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    {
        let state = runtime.state.lock().unwrap();
        let record = state.agents.values().next().unwrap();
        assert!(record.codex_parent_stopped_for_subagents);
        assert!(record.codex_terminal_root_turn_ids.contains("turn-a"));
        assert_eq!(record.codex_running_subagents["child"], "child-turn");
    }

    runtime
        .ingest_hook_with_context(
            &event_for_turn("late-prompt", "UserPromptSubmit", "turn-a"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let completed = runtime
        .ingest_hook_with_context(
            &child_event_for_turn("child-stop", "SubagentStop", "child", "child-turn"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(completed.notify);
    assert_eq!(
        completed.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn permission_cache_miss_is_not_a_cached_block_decision() {
    let runtime = runtime("permission-negative-is-not-cached");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &prompt_for_turn("prompt-before-context", "turn-1", None),
            "server-a",
            Some(&topology),
        )
        .unwrap();

    let unclassified = runtime
        .ingest_hook_with_context(
            &permission_for_turn("permission-before-context", "turn-1"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!unclassified.notify);
    assert_eq!(
        unclassified.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    assert!(
        runtime
            .state
            .lock()
            .unwrap()
            .agents
            .values()
            .all(|record| record.codex_turn_reviews.is_empty())
    );

    let mut permission = permission_for_turn("permission-after-context", "turn-1");
    let mut payload: serde_json::Value = serde_json::from_slice(&permission.payload_json).unwrap();
    payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "auto_review".into();
    permission.payload_json = serde_json::to_vec(&payload).unwrap();
    let recovered = runtime
        .ingest_hook_with_context(&permission, "server-a", Some(&topology))
        .unwrap();
    assert!(!recovered.notify);
    assert_eq!(
        recovered.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    assert!(
        runtime
            .state
            .lock()
            .unwrap()
            .agents
            .values()
            .all(|record| has_auto_review_turn(record, "", "turn-1"))
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
        let permission = runtime
            .ingest_hook_with_context(
                &permission_for_turn("permission", "turn-1"),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        assert_eq!(permission.notify, reviewer == "user");
        assert_eq!(
            permission.agent.unwrap().lifecycle,
            if reviewer == "user" {
                v1::AgentLifecycleState::Blocked as i32
            } else {
                v1::AgentLifecycleState::Working as i32
            }
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
            .all(|record| record.codex_turn_reviews.is_empty())
    );
    let unclassified = runtime
        .ingest_hook_with_context(
            &permission_for_turn("permission", "turn-1"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!unclassified.notify);
    assert_eq!(
        unclassified.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
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
        assert!(!missed.notify);
        assert_eq!(
            missed.agent.unwrap().lifecycle,
            v1::AgentLifecycleState::Working as i32
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
        .ingest_hook_with_context(
            &event_for_turn("stop", "Stop", "turn-1"),
            "server-a",
            Some(&topology),
        )
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
        .ingest_hook_with_context(
            &event_for_turn("stop", "Stop", "turn-1"),
            "server-a",
            Some(&topology),
        )
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
        .ingest_hook_with_context(
            &event_for_turn("stop", "Stop", "turn-1"),
            "server-a",
            Some(&topology),
        )
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
            .ingest_hook_with_context(
                &event_for_turn(id, name, "turn-1"),
                "server-a",
                Some(&topology),
            )
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
        runtime
            .ingest_hook_with_context(
                &event_for_turn("next-prompt", "UserPromptSubmit", "turn-2"),
                "server-a",
                Some(&topology),
            )
            .unwrap()
            .agent
            .unwrap()
            .lifecycle,
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
    assert!(process::detect(&topology.panes[0]).is_some());
    assert!(
        !pane_has_supported_process(&topology.panes[0]),
        "the Voice guard ignores cached command text on this synthetic pane"
    );
    assert!(
        runtime
            .with_valid_input_pane("server-a", "%7", true, false, || Ok(()))
            .is_ok()
    );
    assert!(
        runtime
            .with_valid_input_pane("server-a", "%8", false, false, || Ok(()))
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
    assert!(!pane_has_supported_process(&departed.panes[0]));
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
            .with_valid_input_pane("server-a", "%7", false, false, || Ok(()))
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

    let sibling_finished = claude_hook(
        &runtime,
        &topology,
        "sibling-finished",
        serde_json::json!({"hook_event_name": "SubagentStop"}),
    );
    assert!(!sibling_finished.notify);
    assert_eq!(
        sibling_finished.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Blocked as i32
    );

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

#[test]
fn unwaited_codex_subagents_keep_the_parent_working_until_the_last_one_stops() {
    let runtime = runtime("codex-unwaited-subagents");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    for child in ["child-1", "child-2", "child-3"] {
        runtime
            .ingest_hook_with_context(
                &codex_subagent_event(&format!("start-{child}"), "SubagentStart", child),
                "server-a",
                Some(&topology),
            )
            .unwrap();
    }

    let parent_stop = runtime
        .ingest_hook_with_context(
            &event("parent-stop", 0, "Stop"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!parent_stop.notify);
    assert_eq!(
        parent_stop.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    {
        let state = runtime.state.lock().unwrap();
        let stored = state.agents.values().next().unwrap();
        assert_eq!(stored.codex_running_subagents.len(), 3);
        assert!(stored.codex_parent_stopped_for_subagents);
        assert!(!stored.hook_terminal);
    }

    for (event_id, child, remaining) in [
        ("stop-1", "child-1", 2),
        ("duplicate-stop-1", "child-1", 2),
        ("stop-2", "child-2", 1),
    ] {
        let stopped = runtime
            .ingest_hook_with_context(
                &codex_subagent_event(event_id, "SubagentStop", child),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        assert!(!stopped.notify);
        assert_eq!(
            stopped.agent.unwrap().lifecycle,
            v1::AgentLifecycleState::Working as i32
        );
        assert_eq!(
            runtime
                .state
                .lock()
                .unwrap()
                .agents
                .values()
                .next()
                .unwrap()
                .codex_running_subagents
                .len(),
            remaining
        );
    }

    let finished = runtime
        .ingest_hook_with_context(
            &codex_subagent_event("stop-3", "SubagentStop", "child-3"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(finished.notify);
    let finished = finished.agent.unwrap();
    assert_eq!(finished.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert_eq!(finished.attention_generation, 1);
    assert_eq!(finished.attention_kind, "completed");
    let state = runtime.state.lock().unwrap();
    let stored = state.agents.values().next().unwrap();
    assert!(stored.codex_running_subagents.is_empty());
    assert!(!stored.codex_parent_stopped_for_subagents);
    assert!(stored.hook_terminal);
}

#[test]
fn codex_interrupt_and_session_end_clear_unwaited_subagents() {
    for terminal_event in ["Interrupt", "SessionEnd"] {
        for (scope, terminal) in [
            ("idless", event("terminal-idless", 0, terminal_event)),
            (
                "stale-turn",
                event_for_turn("terminal-stale", terminal_event, "turn-stale"),
            ),
        ] {
            let runtime = runtime(&format!(
                "codex-{}-{scope}-clears-unwaited-subagents",
                terminal_event.to_ascii_lowercase()
            ));
            let topology = topology("codex");
            for hook in [
                event_for_turn("prompt", "UserPromptSubmit", "turn-root"),
                codex_subagent_event("start", "SubagentStart", "child"),
                event_for_turn("parent-stop", "Stop", "turn-root"),
            ] {
                runtime
                    .ingest_hook_with_context(&hook, "server-a", Some(&topology))
                    .unwrap();
            }

            let terminal = runtime
                .ingest_hook_with_context(&terminal, "server-a", Some(&topology))
                .unwrap();
            assert_eq!(
                terminal.agent.unwrap().lifecycle,
                v1::AgentLifecycleState::Idle as i32
            );

            let state = runtime.state.lock().unwrap();
            let record = state.agents.values().next().unwrap();
            assert!(record.codex_running_subagents.is_empty());
            assert!(!record.codex_parent_stopped_for_subagents);
            assert!(record.hook_terminal);
        }
    }
}

#[test]
fn a_child_stop_clears_an_active_id_loaded_without_a_turn() {
    let runtime = runtime("codex-legacy-child-id-stop");
    let topology = topology("codex");
    for hook in [
        event("prompt", 0, "UserPromptSubmit"),
        codex_subagent_event("start", "SubagentStart", "child"),
        event("parent-stop", 0, "Stop"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    runtime
        .state
        .lock()
        .unwrap()
        .agents
        .values_mut()
        .next()
        .unwrap()
        .codex_running_subagents
        .insert("child".into(), String::new());

    let stopped = runtime
        .ingest_hook_with_context(
            &codex_subagent_event("stop", "SubagentStop", "child"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert_eq!(
        stopped.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
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
}

#[test]
fn waited_codex_subagents_leave_completion_to_the_final_parent_stop() {
    let runtime = runtime("codex-waited-subagents");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    for child in ["child-1", "child-2", "child-3"] {
        runtime
            .ingest_hook_with_context(
                &codex_subagent_event(&format!("start-{child}"), "SubagentStart", child),
                "server-a",
                Some(&topology),
            )
            .unwrap();
    }
    for child in ["child-1", "child-2", "child-3"] {
        let stopped = runtime
            .ingest_hook_with_context(
                &codex_subagent_event(&format!("stop-{child}"), "SubagentStop", child),
                "server-a",
                Some(&topology),
            )
            .unwrap();
        assert!(!stopped.notify);
        assert_eq!(
            stopped.agent.unwrap().lifecycle,
            v1::AgentLifecycleState::Working as i32
        );
    }
    let finished = runtime
        .ingest_hook_with_context(
            &event("parent-stop", 0, "Stop"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(finished.notify);
    assert_eq!(
        finished.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn an_unwaited_codex_subagent_guard_survives_a_daemon_restart() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("codex-subagent-restart-{}", uuid::Uuid::new_v4()))
        .join("agents.json");
    let topology = topology("codex");
    {
        let runtime = AgentRuntime::isolated(path.clone());
        for hook in [
            event("prompt", 0, "UserPromptSubmit"),
            codex_subagent_event("start", "SubagentStart", "child"),
            event("parent-stop", 0, "Stop"),
        ] {
            runtime
                .ingest_hook_with_context(&hook, "server-a", Some(&topology))
                .unwrap();
        }
    }

    let restarted = AgentRuntime::isolated(path);
    let finished = restarted
        .ingest_hook_with_context(
            &codex_subagent_event("child-stop", "SubagentStop", "child"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(finished.notify);
    assert_eq!(
        finished.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn a_codex_sibling_finishing_cannot_hide_a_permission_block() {
    let runtime = runtime("codex-subagent-permission");
    let topology = topology("codex");
    for hook in [
        event("prompt", 0, "UserPromptSubmit"),
        codex_subagent_event("start-1", "SubagentStart", "child-1"),
        codex_subagent_event("start-2", "SubagentStart", "child-2"),
        event("parent-stop", 0, "Stop"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let mut permission = permission_for_turn("permission", "turn-child-2");
    let mut payload: serde_json::Value = serde_json::from_slice(&permission.payload_json).unwrap();
    payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "user".into();
    payload[adapters::CODEX_SUBAGENT_ID_FIELD] = "child-2".into();
    permission.payload_json = serde_json::to_vec(&payload).unwrap();
    let blocked = runtime
        .ingest_hook_with_context(&permission, "server-a", Some(&topology))
        .unwrap();
    assert!(blocked.notify);
    let attention = blocked.agent.unwrap().attention_generation;

    let sibling_finished = runtime
        .ingest_hook_with_context(
            &codex_subagent_event("stop-1", "SubagentStop", "child-1"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!sibling_finished.notify);
    let sibling_finished = sibling_finished.agent.unwrap();
    assert_eq!(
        sibling_finished.lifecycle,
        v1::AgentLifecycleState::Blocked as i32
    );
    assert_eq!(sibling_finished.attention_generation, attention);

    runtime
        .ingest_hook_with_context(
            &event("permission-resolved", 0, "PostToolUse"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let finished = runtime
        .ingest_hook_with_context(
            &codex_subagent_event("stop-2", "SubagentStop", "child-2"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(finished.notify);
    assert_eq!(
        finished.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn the_final_codex_child_cannot_terminalize_a_blocked_parent() {
    let runtime = runtime("codex-final-child-permission");
    let topology = topology("codex");
    for hook in [
        event("prompt", 0, "UserPromptSubmit"),
        codex_subagent_event("start", "SubagentStart", "child"),
        event("parent-stop", 0, "Stop"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let mut permission = permission_for_turn("permission", "turn-child");
    let mut payload: serde_json::Value = serde_json::from_slice(&permission.payload_json).unwrap();
    payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "user".into();
    payload[adapters::CODEX_SUBAGENT_ID_FIELD] = "child".into();
    permission.payload_json = serde_json::to_vec(&payload).unwrap();
    runtime
        .ingest_hook_with_context(&permission, "server-a", Some(&topology))
        .unwrap();

    let final_child = runtime
        .ingest_hook_with_context(
            &codex_subagent_event("stop", "SubagentStop", "child"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert_eq!(
        final_child.agent.as_ref().unwrap().lifecycle,
        v1::AgentLifecycleState::Blocked as i32
    );
    let state = runtime.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert!(record.codex_running_subagents.is_empty());
    assert!(record.codex_parent_stopped_for_subagents);
    assert!(!record.hook_terminal);
}

#[test]
fn a_new_codex_prompt_owns_completion_while_an_older_child_finishes() {
    let runtime = runtime("codex-overlapping-prompt");
    let topology = topology("codex");
    for hook in [
        event("prompt-1", 0, "UserPromptSubmit"),
        codex_subagent_event("start", "SubagentStart", "child"),
        event("parent-stop-1", 0, "Stop"),
        event("prompt-2", 0, "UserPromptSubmit"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let child_stop = runtime
        .ingest_hook_with_context(
            &codex_subagent_event("child-stop", "SubagentStop", "child"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!child_stop.notify);
    assert_eq!(
        child_stop.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    let parent_stop = runtime
        .ingest_hook_with_context(
            &event("parent-stop-2", 0, "Stop"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(parent_stop.notify);
    assert_eq!(
        parent_stop.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn continued_codex_child_activity_reopens_a_stopped_parent() {
    let runtime = runtime("codex-continued-child");
    let topology = topology("codex");
    for hook in [
        event("prompt", 0, "UserPromptSubmit"),
        codex_subagent_event("start", "SubagentStart", "child"),
        event("parent-stop", 0, "Stop"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let first_stop = runtime
        .ingest_hook_with_context(
            &codex_subagent_event("child-stop-1", "SubagentStop", "child"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(first_stop.notify);
    assert_eq!(
        first_stop.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );

    let resumed = runtime
        .ingest_hook_with_context(
            &codex_subagent_turn_event(
                "child-resumed",
                "PreToolUse",
                "child",
                "turn-child-resumed",
            ),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!resumed.notify);
    assert_eq!(
        resumed.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    {
        let state = runtime.state.lock().unwrap();
        let record = state.agents.values().next().unwrap();
        assert!(record.codex_running_subagents.contains_key("child"));
        assert!(record.codex_parent_stopped_for_subagents);
        assert!(!record.hook_terminal);
    }

    let final_stop = runtime
        .ingest_hook_with_context(
            &codex_subagent_turn_event(
                "child-stop-2",
                "SubagentStop",
                "child",
                "turn-child-resumed",
            ),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(final_stop.notify);
    assert_eq!(
        final_stop.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn transcript_interruption_finishes_a_stopped_parent_without_a_subagent_stop_hook() {
    use std::io::Write as _;

    let runtime = runtime("codex-transcript-interruption");
    let topology = topology("codex");
    for hook in [
        event("prompt", 0, "UserPromptSubmit"),
        codex_subagent_event("start", "SubagentStart", "child"),
        event("parent-stop", 0, "Stop"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/09");
    std::fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    std::fs::write(&transcript, b"").unwrap();
    let record_id = attach_child_monitor(&runtime, home.path(), &transcript, "child", "turn-child");

    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        "{}",
        codex_turn_terminal("turn-child", "turn_aborted")
    )
    .unwrap();
    file.sync_all().unwrap();
    let events = runtime.sweep_codex_child_terminals();

    assert_eq!(events.len(), 1);
    assert!(events[0].notify);
    assert_eq!(events[0].reason, "child_turn_aborted");
    assert_eq!(
        events[0].agent.as_ref().unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
    let record = &runtime.state.lock().unwrap().agents[&record_id];
    assert!(record.codex_running_subagents.is_empty());
    assert!(record.hook_terminal);
    assert!(runtime.codex_child_monitors.lock().unwrap().is_empty());
}

#[test]
fn a_resume_hook_wins_over_an_older_unpolled_interruption() {
    use std::io::Write as _;

    let runtime = runtime("codex-transcript-resume-ordering");
    let topology = topology("codex");
    for hook in [
        event("prompt", 0, "UserPromptSubmit"),
        codex_subagent_event("start", "SubagentStart", "child"),
        event("parent-stop", 0, "Stop"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/09");
    std::fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    std::fs::write(&transcript, b"").unwrap();
    let record_id = attach_child_monitor(&runtime, home.path(), &transcript, "child", "turn-child");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        "{}",
        codex_turn_terminal("turn-child", "turn_aborted")
    )
    .unwrap();
    file.sync_all().unwrap();

    let resumed = runtime
        .ingest_hook_with_context(
            &codex_subagent_turn_event("resume", "PreToolUse", "child", "turn-child-resumed"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert_eq!(
        resumed.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    {
        let state = runtime.state.lock().unwrap();
        let record = &state.agents[&record_id];
        assert_eq!(
            record.codex_running_subagents,
            std::collections::BTreeMap::from([(
                "child".to_owned(),
                "turn-child-resumed".to_owned(),
            )])
        );
        assert!(!record.hook_terminal);
    }
    assert!(runtime.sweep_codex_child_terminals().is_empty());
}

#[test]
fn recent_child_terminal_is_not_starved_by_sixty_four_historical_ids() {
    use std::io::Write as _;

    let runtime = runtime("codex-transcript-recent-cap");
    let topology = topology("codex");
    for hook in [
        event("prompt", 0, "UserPromptSubmit"),
        codex_subagent_event("start", "SubagentStart", "current-child"),
        event("parent-stop", 0, "Stop"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/09");
    std::fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    std::fs::write(&transcript, b"").unwrap();
    attach_child_monitor(
        &runtime,
        home.path(),
        &transcript,
        "current-child",
        "turn-current-child",
    );
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    for index in 0..MAX_CODEX_CHILDREN {
        writeln!(
            file,
            "{}",
            codex_turn_terminal(&format!("old-{index}"), "task_complete")
        )
        .unwrap();
    }
    writeln!(
        file,
        "{}",
        codex_turn_terminal("turn-current-child", "turn_aborted")
    )
    .unwrap();
    file.sync_all().unwrap();

    let events = runtime.sweep_codex_child_terminals();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].agent.as_ref().unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn transcript_completion_before_parent_stop_leaves_completion_to_the_parent() {
    use std::io::Write as _;

    let runtime = runtime("codex-transcript-parent-ordering");
    let topology = topology("codex");
    for hook in [
        event("prompt", 0, "UserPromptSubmit"),
        codex_subagent_event("start", "SubagentStart", "child"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/09");
    std::fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    std::fs::write(&transcript, b"").unwrap();
    attach_child_monitor(&runtime, home.path(), &transcript, "child", "turn-child");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    writeln!(
        file,
        "{}",
        codex_turn_terminal("turn-child", "task_complete")
    )
    .unwrap();
    file.sync_all().unwrap();

    let reconciled = runtime.sweep_codex_child_terminals();
    assert_eq!(reconciled.len(), 1);
    assert!(!reconciled[0].notify);
    assert_eq!(
        reconciled[0].agent.as_ref().unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    let stopped = runtime
        .ingest_hook_with_context(
            &event("parent-stop", 0, "Stop"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(stopped.notify);
    assert_eq!(
        stopped.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn transcript_reconciles_parallel_and_nested_children_without_hiding_a_block() {
    use std::io::Write as _;

    let runtime = runtime("codex-transcript-parallel-nested-blocked");
    let topology = topology("codex");
    for hook in [
        event_for_turn("prompt", "UserPromptSubmit", "turn-root"),
        codex_subagent_event("direct", "SubagentStart", "direct-child"),
        codex_subagent_event("nested", "SubagentStart", "nested-child"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let blocked = runtime
        .ingest_hook_with_context(
            &event_for_turn("blocked", "PermissionRequest", "turn-root"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert_eq!(
        blocked.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Blocked as i32
    );

    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/09");
    std::fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    std::fs::write(&transcript, b"").unwrap();
    attach_child_monitor(
        &runtime,
        home.path(),
        &transcript,
        "direct-child",
        "turn-direct-child",
    );
    attach_child_monitor(
        &runtime,
        home.path(),
        &transcript,
        "nested-child",
        "turn-nested-child",
    );
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&transcript)
        .unwrap();
    for (turn, kind) in [
        ("turn-direct-child", "task_complete"),
        ("turn-nested-child", "turn_aborted"),
    ] {
        writeln!(file, "{}", codex_turn_terminal(turn, kind)).unwrap();
    }
    file.sync_all().unwrap();

    let reconciled = runtime.sweep_codex_child_terminals();
    assert_eq!(reconciled.len(), 2);
    let blocked = reconciled.last().unwrap().agent.as_ref().unwrap();
    assert_eq!(blocked.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    assert_eq!(blocked.attention_kind, "blocked");
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
    {
        let state = runtime.state.lock().unwrap();
        let record = state.agents.values().next().unwrap();
        assert!(!record.codex_parent_stopped_for_subagents);
        assert!(!record.hook_terminal);
    }
    let resolved = runtime
        .ingest_hook_with_context(
            &event_for_turn("permission-resolved", "PostToolUse", "turn-root"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(!resolved.notify);
    assert_eq!(
        resolved.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    let completed = runtime
        .ingest_hook_with_context(
            &event_for_turn("parent-stop", "Stop", "turn-root"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(completed.notify);
    assert_eq!(
        completed.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
}

#[test]
fn child_ids_and_transcript_monitors_are_bounded() {
    let runtime = runtime("codex-child-limits");
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    for index in 0..(MAX_CODEX_CHILDREN + 10) {
        runtime
            .ingest_hook_with_context(
                &codex_subagent_event(
                    &format!("start-{index}"),
                    "SubagentStart",
                    &format!("child-{index}"),
                ),
                "server-a",
                Some(&topology),
            )
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
            .codex_running_subagents
            .len(),
        MAX_CODEX_CHILDREN
    );
    assert!(
        runtime
            .state
            .lock()
            .unwrap()
            .agents
            .values()
            .next()
            .unwrap()
            .codex_subagent_capacity_exceeded
    );

    let mut homes = Vec::new();
    for index in 0..(MAX_CODEX_TRANSCRIPT_MONITORS + 10) {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let transcript = sessions.join(format!("{index}.jsonl"));
        std::fs::write(&transcript, b"").unwrap();
        let monitor = crate::hook::codex_transcript::TurnMonitor::open(
            &serde_json::json!({"turn_id": format!("turn-{index}"), "transcript_path": transcript}),
            home.path(),
        )
        .unwrap();
        runtime.track_codex_child(
            &format!("record-{index}"),
            Some(CodexTurnKey {
                agent_id: format!("child-{index}"),
                turn_id: format!("turn-{index}"),
            }),
            Some(monitor),
            true,
            false,
        );
        homes.push(home);
    }
    assert_eq!(
        runtime.codex_child_monitors.lock().unwrap().len(),
        MAX_CODEX_TRANSCRIPT_MONITORS
    );
}

#[test]
fn daemon_load_bounds_an_older_oversized_child_set_without_false_idle() {
    let path = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("codex-oversized-restart-{}", uuid::Uuid::new_v4()))
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
        let mut state = runtime.state.lock().unwrap();
        let record = state.agents.values_mut().next().unwrap();
        record.codex_running_subagents = (0..80)
            .map(|index| {
                (
                    format!("legacy-child-{index}"),
                    format!("legacy-turn-{index}"),
                )
            })
            .collect();
        record.codex_subagent_root_turn_ids = (0..80)
            .map(|index| {
                (
                    format!("legacy-child-{index}"),
                    format!("root-turn-{index}"),
                )
            })
            .collect();
        record.codex_latest_subagent_turns = (0..(MAX_CODEX_CHILD_TURN_HISTORY + 10))
            .map(|index| CodexTurnKey {
                agent_id: format!("history-child-{index}"),
                turn_id: format!("legacy-turn-{index}"),
            })
            .collect();
        record.codex_parent_stopped_for_subagents = true;
        runtime.persist_locked(&state).unwrap();
    }

    let restarted = AgentRuntime::isolated(path);
    let state = restarted.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert_eq!(record.codex_running_subagents.len(), MAX_CODEX_CHILDREN);
    assert_eq!(
        record.codex_subagent_root_turn_ids.len(),
        MAX_CODEX_CHILDREN
    );
    assert!(
        record
            .codex_subagent_root_turn_ids
            .keys()
            .all(|child_id| { record.codex_running_subagents.contains_key(child_id) })
    );
    assert_eq!(
        record.codex_latest_subagent_turns.len(),
        MAX_CODEX_CHILD_TURN_HISTORY
    );
    assert!(record.codex_subagent_capacity_exceeded);
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
}

#[test]
fn a_readable_child_transcript_is_stronger_than_the_twenty_four_hour_backstop() {
    let runtime = runtime("codex-readable-transcript-stale");
    let topology = topology("codex");
    for hook in [
        event("prompt", 0, "UserPromptSubmit"),
        codex_subagent_event("start", "SubagentStart", "child"),
        event("parent-stop", 0, "Stop"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    let home = tempfile::tempdir().unwrap();
    let sessions = home.path().join(".codex/sessions/2026/09/09");
    std::fs::create_dir_all(&sessions).unwrap();
    let transcript = sessions.join("rollout.jsonl");
    std::fs::write(&transcript, b"").unwrap();
    attach_child_monitor(&runtime, home.path(), &transcript, "child", "turn-child");
    {
        let mut state = runtime.state.lock().unwrap();
        let record = state.agents.values_mut().next().unwrap();
        record.subagent_evidence_observed_at_unix_millis =
            now_millis() - STALE_SUBAGENT_WORKING_TTL_MILLIS - 1_000;
    }

    assert!(runtime.sweep_codex_child_terminals().is_empty());
    assert!(runtime.sweep_stale().is_empty());
    assert_eq!(
        runtime.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
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

#[test]
fn staleness_never_overrides_direct_evidence_of_a_running_subagent() {
    let codex_topology = topology("codex");
    let codex = runtime("stale-codex-subagent");
    for hook in [
        event("prompt", 0, "UserPromptSubmit"),
        codex_subagent_event("start", "SubagentStart", "child"),
        event("parent-stop", 0, "Stop"),
    ] {
        codex
            .ingest_hook_with_context(&hook, "server-a", Some(&codex_topology))
            .unwrap();
    }
    {
        let mut state = codex.state.lock().unwrap();
        let record = state.agents.values_mut().next().unwrap();
        record.lifecycle_observed_at_unix_millis = now_millis() - STALE_WORKING_TTL_MILLIS - 1_000;
        record.subagent_evidence_observed_at_unix_millis =
            now_millis() - STALE_WORKING_TTL_MILLIS - 1_000;
    }
    assert!(codex.sweep_stale().is_empty());
    let finished = codex
        .ingest_hook_with_context(
            &codex_subagent_event("stop", "SubagentStop", "child"),
            "server-a",
            Some(&codex_topology),
        )
        .unwrap();
    assert!(finished.notify);
    assert_eq!(
        finished.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );

    let claude = runtime("stale-claude-subagent");
    let claude_topology = topology("claude");
    claude_hook(
        &claude,
        &claude_topology,
        "prompt",
        serde_json::json!({"hook_event_name": "UserPromptSubmit"}),
    );
    claude_hook(
        &claude,
        &claude_topology,
        "parent-stop",
        serde_json::json!({
            "hook_event_name": "Stop",
            "has_running_subagent": true
        }),
    );
    {
        let mut state = claude.state.lock().unwrap();
        let record = state.agents.values_mut().next().unwrap();
        record.lifecycle_observed_at_unix_millis = now_millis() - STALE_WORKING_TTL_MILLIS - 1_000;
        record.subagent_evidence_observed_at_unix_millis =
            now_millis() - STALE_WORKING_TTL_MILLIS - 1_000;
    }
    assert!(claude.sweep_stale().is_empty());
    assert_eq!(
        claude.snapshot_for("server-a").agents[0].lifecycle,
        v1::AgentLifecycleState::Working as i32
    );
    {
        let mut state = claude.state.lock().unwrap();
        let record = state.agents.values_mut().next().unwrap();
        record.lifecycle_observed_at_unix_millis =
            now_millis() - STALE_SUBAGENT_WORKING_TTL_MILLIS - 1_000;
        record.subagent_evidence_observed_at_unix_millis =
            now_millis() - STALE_SUBAGENT_WORKING_TTL_MILLIS - 1_000;
    }
    let stale = claude.sweep_stale();
    assert_eq!(
        stale.len(),
        1,
        "lost child-stop evidence cannot live forever"
    );
    assert_eq!(
        stale[0].agent.as_ref().unwrap().lifecycle,
        v1::AgentLifecycleState::Unknown as i32
    );
    let state = claude.state.lock().unwrap();
    let record = state.agents.values().next().unwrap();
    assert!(!record.claude_has_running_subagent);
    assert!(record.codex_running_subagents.is_empty());
    assert!(!record.codex_parent_stopped_for_subagents);
    assert_eq!(record.subagent_evidence_observed_at_unix_millis, 0);
}

#[test]
fn expired_codex_subagent_evidence_cannot_poison_the_next_turn() {
    let runtime = runtime("expired-codex-subagent");
    let topology = topology("codex");
    for hook in [
        event("prompt-1", 0, "UserPromptSubmit"),
        codex_subagent_event("start", "SubagentStart", "lost-child"),
        event("parent-stop-1", 0, "Stop"),
    ] {
        runtime
            .ingest_hook_with_context(&hook, "server-a", Some(&topology))
            .unwrap();
    }
    runtime
        .state
        .lock()
        .unwrap()
        .agents
        .values_mut()
        .next()
        .unwrap()
        .subagent_evidence_observed_at_unix_millis =
        now_millis() - STALE_SUBAGENT_WORKING_TTL_MILLIS - 1_000;
    assert_eq!(runtime.sweep_stale().len(), 1);
    {
        let state = runtime.state.lock().unwrap();
        let record = state.agents.values().next().unwrap();
        assert!(record.codex_running_subagents.is_empty());
        assert!(!record.codex_parent_stopped_for_subagents);
    }

    runtime
        .ingest_hook_with_context(
            &event("prompt-2", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let finished = runtime
        .ingest_hook_with_context(
            &event("parent-stop-2", 0, "Stop"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    assert!(finished.notify);
    assert_eq!(
        finished.agent.unwrap().lifecycle,
        v1::AgentLifecycleState::Idle as i32
    );
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
