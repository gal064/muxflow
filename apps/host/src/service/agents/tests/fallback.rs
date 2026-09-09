use super::*;

/// A turn that starts and then blocks while the daemon is down.
///
/// The mailbox used to keep one event per pane, so only the block survived
/// — and the daemon then correctly ignored it, because the prompt that
/// opened the new turn had been overwritten and the previous turn was
/// already finished. The user came back to the old result and no sign that
/// an agent was waiting on them.
#[test]
fn a_turn_that_starts_and_blocks_offline_replays_in_the_order_it_happened() {
    let dir = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase13-offline-turn-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let runtime = AgentRuntime::isolated(dir.join("agents.json"));
    let topology = topology("codex");
    for (id, name) in [("prompt", "UserPromptSubmit"), ("stop", "Stop")] {
        runtime
            .ingest_hook_with_context(&event(id, 0, name), "server-a", Some(&topology))
            .unwrap();
    }

    // Written out of order on purpose: the replay must be driven by the
    // names, not by whatever order the directory happens to hand back.
    for (name, event_name) in [
        (
            "hook-fallback-codex-7-00000000000000000002-b.pb",
            "PermissionRequest",
        ),
        (
            "hook-fallback-codex-7-00000000000000000001-a.pb",
            "UserPromptSubmit",
        ),
    ] {
        fs::write(
            dir.join(name),
            event(event_name, 0, event_name).encode_to_vec(),
        )
        .unwrap();
    }
    let mut replayed = Vec::new();
    let report = fallback::consume(&dir, |event| {
        replayed.push(event.source_event_id.clone());
        match runtime.ingest_hook_with_context(&event, "server-a", Some(&topology)) {
            Ok(_) => fallback::HookReplayDisposition::Applied,
            Err(HookIngestFailure::Duplicate | HookIngestFailure::Permanent(_)) => {
                fallback::HookReplayDisposition::Discarded
            }
            Err(HookIngestFailure::Retryable(_)) => fallback::HookReplayDisposition::Retryable,
        }
    })
    .unwrap();
    assert_eq!(report.applied, 2);
    assert_eq!(report.retained, 0);
    assert_eq!(replayed, ["UserPromptSubmit", "PermissionRequest"]);
    let record = &runtime.snapshot_for("server-a").agents[0];
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Blocked as i32);
    assert_eq!(record.attention_kind, "blocked");
    assert!(record.attention_generation > record.seen_generation);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn replay_preserves_an_auto_review_permission_as_working() {
    let dir = std::env::current_dir().unwrap().join("tmp").join(format!(
        "phase13-auto-review-replay-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    let runtime = AgentRuntime::isolated(dir.join("agents.json"));
    let topology = topology("codex");
    let mut prompt = event("prompt", 0, "UserPromptSubmit");
    let mut prompt_payload = serde_json::json!({"hook_event_name": "UserPromptSubmit"});
    prompt_payload[adapters::CODEX_APPROVAL_REVIEWER_FIELD] = "auto_review".into();
    prompt_payload[adapters::CODEX_APPROVAL_TURN_ID_FIELD] = "turn-1".into();
    prompt.payload_json = serde_json::to_vec(&prompt_payload).unwrap();
    let mut permission = event("permission", 0, "PermissionRequest");
    let mut payload = serde_json::json!({"hook_event_name": "PermissionRequest"});
    payload[adapters::CODEX_APPROVAL_TURN_ID_FIELD] = "turn-1".into();
    permission.payload_json = serde_json::to_vec(&payload).unwrap();
    let mut cached = event("cached-permission", 0, "PermissionRequest");
    let mut cached_payload = serde_json::json!({"hook_event_name": "PermissionRequest"});
    cached_payload[adapters::CODEX_APPROVAL_TURN_ID_FIELD] = "turn-1".into();
    cached.payload_json = serde_json::to_vec(&cached_payload).unwrap();

    for (name, hook) in [
        ("hook-fallback-codex-7-00000000000000000001-a.pb", prompt),
        (
            "hook-fallback-codex-7-00000000000000000002-b.pb",
            permission,
        ),
        ("hook-fallback-codex-7-00000000000000000003-c.pb", cached),
    ] {
        fs::write(dir.join(name), hook.encode_to_vec()).unwrap();
    }
    let report = fallback::consume(&dir, |event| {
        match runtime.ingest_hook_with_context(&event, "server-a", Some(&topology)) {
            Ok(_) => fallback::HookReplayDisposition::Applied,
            Err(HookIngestFailure::Duplicate | HookIngestFailure::Permanent(_)) => {
                fallback::HookReplayDisposition::Discarded
            }
            Err(HookIngestFailure::Retryable(_)) => fallback::HookReplayDisposition::Retryable,
        }
    })
    .unwrap();
    assert_eq!(report.applied, 3);
    let record = &runtime.snapshot_for("server-a").agents[0];
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Working as i32);
    assert_eq!(record.attention_generation, 0);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn replayed_sanitized_transcript_edges_repair_a_missing_child_stop() {
    let dir = std::env::current_dir().unwrap().join("tmp").join(format!(
        "phase13-child-transcript-replay-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    let runtime = AgentRuntime::isolated(dir.join("agents.json"));
    let topology = topology("codex");

    let mut child_start = event("child-start", 0, "SubagentStart");
    child_start.payload_json = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "SubagentStart",
        adapters::CODEX_SUBAGENT_ID_FIELD: "child",
        adapters::CODEX_CHILD_TRANSITIONS_FIELD: [
            {"agent_id": "older-child", "active": false},
            {"agent_id": "child", "active": true},
        ],
    }))
    .unwrap();
    let mut repaired = event("transcript-repair", 0, "PostToolUse");
    repaired.payload_json = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "PostToolUse",
        adapters::CODEX_CHILD_TRANSITIONS_FIELD: [
            {"agent_id": "child", "active": false},
        ],
    }))
    .unwrap();
    for (name, hook) in [
        (
            "hook-fallback-codex-7-00000000000000000001-a.pb",
            event("prompt", 0, "UserPromptSubmit"),
        ),
        (
            "hook-fallback-codex-7-00000000000000000002-b.pb",
            child_start,
        ),
        (
            "hook-fallback-codex-7-00000000000000000003-c.pb",
            event("parent-stop", 0, "Stop"),
        ),
        ("hook-fallback-codex-7-00000000000000000004-d.pb", repaired),
    ] {
        fs::write(dir.join(name), hook.encode_to_vec()).unwrap();
    }

    let report = fallback::consume(&dir, |event| {
        match runtime.ingest_hook_with_context(&event, "server-a", Some(&topology)) {
            Ok(_) => fallback::HookReplayDisposition::Applied,
            Err(HookIngestFailure::Duplicate | HookIngestFailure::Permanent(_)) => {
                fallback::HookReplayDisposition::Discarded
            }
            Err(HookIngestFailure::Retryable(_)) => fallback::HookReplayDisposition::Retryable,
        }
    })
    .unwrap();

    assert_eq!(report.applied, 4);
    let record = &runtime.snapshot_for("server-a").agents[0];
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Idle as i32);
    assert_eq!(record.attention_kind, "completed");
    let stored = runtime.state.lock().unwrap();
    let stored = stored.agents.values().next().unwrap();
    assert!(stored.codex_running_subagent_ids.is_empty());
    assert!(stored.hook_terminal);
    fs::remove_dir_all(dir).unwrap();
}

/// The configuration probe reads the *daemon process's* `PATH`, and a
/// daemon started by launchd or a non-login SSH exec has one without
/// `~/.local/bin`. A running agent is proof its vendor is installed here —
/// reported as `Absent`, the desktop offers nothing and says nothing,
/// which is the original failure with the volume turned down.
///
/// Asserted as an invariant over the running set rather than by observing
/// a machine without the agent installed: whether *this* machine has Codex
/// on its `PATH` is not something a test may depend on. The `Absent` half
/// is covered by `an_agent_that_is_not_on_this_host_is_absent_rather_than_unwired`,
/// which passes the search path in.
#[test]
fn a_running_agent_is_never_reported_as_an_agent_this_host_does_not_have() {
    let home = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase13-running-absent-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&home).unwrap();
    let manager = hooks::HookManager::for_home(&home);
    let running: BTreeSet<&str> = adapters::all().map(|adapter| adapter.id()).collect();
    // Nothing is configured here at all, so every answer would otherwise be
    // whatever the search path happened to say.
    for (adapter, observed) in manager.wiring_with_running(&running) {
        assert_eq!(
            observed.state,
            v1::AgentHookWiring::NotWired,
            "{} is running and was reported as {:?}",
            adapter.id(),
            observed.state
        );
    }
    std::fs::remove_dir_all(home).unwrap();
}

#[test]
fn malformed_fallback_is_removed_and_does_not_stop_the_scan() {
    let dir = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase6-fallback-scan-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("hook-fallback-a-1.pb"), b"malformed").unwrap();
    fs::write(
        dir.join("hook-fallback-b-2.pb"),
        event("valid", 0, "Stop").encode_to_vec(),
    )
    .unwrap();
    let mut seen = Vec::new();
    let report = fallback::consume(&dir, |event| {
        seen.push(event.source_event_id);
        fallback::HookReplayDisposition::Applied
    })
    .unwrap();
    assert_eq!(report.applied, 1);
    assert_eq!(report.retained, 0);
    assert_eq!(seen, ["valid"]);
    let remaining = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(remaining, [".hook-fallback-mailbox.lock"]);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn retryable_fallback_replay_stays_queued_until_a_later_disposition() {
    let dir = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase14-fallback-retain-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("hook-fallback-codex-7-00000000000000000001-a.pb");
    let later = dir.join("hook-fallback-codex-7-00000000000000000002-b.pb");
    fs::write(
        &path,
        event("retryable", 0, "PermissionRequest").encode_to_vec(),
    )
    .unwrap();
    fs::write(&later, event("later", 0, "Stop").encode_to_vec()).unwrap();

    let mut attempts = 0;
    let retained = fallback::consume(&dir, |_| {
        attempts += 1;
        fallback::HookReplayDisposition::Retryable
    })
    .unwrap();
    assert_eq!(retained.applied, 0);
    assert_eq!(retained.retained, 2);
    assert_eq!(
        attempts, 1,
        "later events must not overtake a retained event"
    );
    assert!(path.exists(), "retryable replay must remain durable");
    assert!(later.exists(), "later replay must remain ordered behind it");

    let discarded =
        fallback::consume(&dir, |_| fallback::HookReplayDisposition::Discarded).unwrap();
    assert_eq!(discarded.applied, 0);
    assert_eq!(discarded.retained, 0);
    assert!(!path.exists(), "a permanent/duplicate disposition is final");
    assert!(!later.exists(), "later permanent input is also discarded");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn recovered_mailbox_applies_before_the_next_live_event_without_a_restart() {
    let root = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase14-live-recovery-{}", uuid::Uuid::new_v4()));
    let state_parent = root.join("state");
    let mailbox = root.join("mailbox");
    fs::create_dir_all(&mailbox).unwrap();
    fs::write(&state_parent, b"blocks persistence").unwrap();
    let runtime = AgentRuntime::isolated(state_parent.join("agents.json"));
    let topology = topology("codex");
    let retained = event("retained-a", 0, "PermissionRequest");
    assert!(matches!(
        runtime.ingest_hook_with_context(&retained, "server-a", Some(&topology)),
        Err(HookIngestFailure::Retryable(_))
    ));
    let queued = mailbox.join("hook-fallback-codex-7-00000000000000000001-a.pb");
    fs::write(&queued, retained.encode_to_vec()).unwrap();

    fs::remove_file(&state_parent).unwrap();
    fs::create_dir_all(&state_parent).unwrap();
    let live = event("live-b", 0, "Stop");
    let live_event = runtime
        .ingest_after_replay_with_context(&live, "server-a", Some(&topology), || {
            let report = fallback::consume(&mailbox, |event| {
                match runtime.ingest_hook_with_context(&event, "server-a", Some(&topology)) {
                    Ok(_) => fallback::HookReplayDisposition::Applied,
                    Err(HookIngestFailure::Duplicate | HookIngestFailure::Permanent(_)) => {
                        fallback::HookReplayDisposition::Discarded
                    }
                    Err(HookIngestFailure::Retryable(_)) => {
                        fallback::HookReplayDisposition::Retryable
                    }
                }
            })?;
            if report.retained > 0 {
                anyhow::bail!("mailbox is still retained");
            }
            Ok(report.applied)
        })
        .unwrap();

    assert!(!queued.exists());
    let record = live_event.agent.unwrap();
    assert_eq!(record.lifecycle, v1::AgentLifecycleState::Idle as i32);
    let stored = &runtime.state.lock().unwrap().agents[&record.agent_id];
    assert_eq!(
        stored.source_event_ids.iter().cloned().collect::<Vec<_>>(),
        ["retained-a", "live-b"]
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn incomplete_mailbox_sweep_rejects_live_input_after_partial_progress() {
    let root = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase14-incomplete-sweep-{}", uuid::Uuid::new_v4()));
    let mailbox = root.join("mailbox");
    let failed_root = root.join("failed-root");
    fs::create_dir_all(&mailbox).unwrap();
    fs::write(&failed_root, b"not a directory").unwrap();
    let runtime = AgentRuntime::isolated(root.join("state/agents.json"));
    let topology = topology("codex");
    let queued = mailbox.join("hook-fallback-codex-7-00000000000000000001-a.pb");
    fs::write(
        &queued,
        event("replayed-a", 0, "UserPromptSubmit").encode_to_vec(),
    )
    .unwrap();

    let roots = [mailbox, failed_root];
    let live = event("live-b", 0, "Stop");
    let result =
        runtime.ingest_after_replay_with_context(&live, "server-a", Some(&topology), || {
            fallback::consume_roots(&roots, |event| {
                match runtime.ingest_hook_with_context(&event, "server-a", Some(&topology)) {
                    Ok(_) => fallback::HookReplayDisposition::Applied,
                    Err(HookIngestFailure::Duplicate | HookIngestFailure::Permanent(_)) => {
                        fallback::HookReplayDisposition::Discarded
                    }
                    Err(HookIngestFailure::Retryable(_)) => {
                        fallback::HookReplayDisposition::Retryable
                    }
                }
            })
        });

    assert!(matches!(result, Err(HookIngestFailure::Retryable(_))));
    assert!(
        !queued.exists(),
        "successfully replayed input is acknowledged"
    );
    let state = runtime.state.lock().unwrap();
    let stored = state.agents.values().next().unwrap();
    assert_eq!(
        stored.source_event_ids.iter().cloned().collect::<Vec<_>>(),
        ["replayed-a"],
        "live input must wait until every mailbox root was inspected"
    );
    drop(state);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn ambiguous_duplicate_replay_still_applies_its_sanitized_child_terminal() {
    let dir = std::env::current_dir().unwrap().join("tmp").join(format!(
        "phase14-duplicate-child-repair-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir_all(&dir).unwrap();
    let runtime = AgentRuntime::isolated(dir.join("agents.json"));
    let topology = topology("codex");
    runtime
        .ingest_hook_with_context(
            &event("prompt", 0, "UserPromptSubmit"),
            "server-a",
            Some(&topology),
        )
        .unwrap();
    let mut started = event("possibly-applied", 0, "SubagentStart");
    started.payload_json = serde_json::to_vec(&serde_json::json!({
        "hook_event_name": "SubagentStart",
        adapters::CODEX_SUBAGENT_ID_FIELD: "child",
    }))
    .unwrap();
    runtime
        .ingest_hook_with_context(&started, "server-a", Some(&topology))
        .unwrap();
    runtime
        .ingest_hook_with_context(
            &event("parent-stop", 0, "Stop"),
            "server-a",
            Some(&topology),
        )
        .unwrap();

    let mut fallback = started;
    let mut payload: serde_json::Value = serde_json::from_slice(&fallback.payload_json).unwrap();
    payload[adapters::CODEX_CHILD_TRANSITIONS_FIELD] =
        serde_json::json!([{"agent_id": "child", "active": false}]);
    fallback.payload_json = serde_json::to_vec(&payload).unwrap();
    let repaired = runtime
        .ingest_hook_with_context(&fallback, "server-a", Some(&topology))
        .unwrap();

    assert!(repaired.notify);
    assert_eq!(
        repaired.agent.unwrap().lifecycle,
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
            .hook_terminal
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn lost_ack_replay_discards_duplicate_without_republishing_or_advancing_state() {
    let root = std::env::current_dir()
        .unwrap()
        .join("tmp")
        .join(format!("phase14-lost-ack-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let runtime = AgentRuntime::isolated(root.join("state/agents.json"));
    let topology = topology("codex");
    let applied = event("ack-lost", 0, "PermissionRequest");
    runtime
        .ingest_hook_with_context(&applied, "server-a", Some(&topology))
        .unwrap();
    let generation = runtime.state.lock().unwrap().generation;
    let queued = root.join("hook-fallback-codex-7-00000000000000000001-a.pb");
    fs::write(&queued, applied.encode_to_vec()).unwrap();

    let report = fallback::consume(&root, |event| {
        match runtime.ingest_hook_with_context(&event, "server-a", Some(&topology)) {
            Ok(_) => fallback::HookReplayDisposition::Applied,
            Err(HookIngestFailure::Duplicate | HookIngestFailure::Permanent(_)) => {
                fallback::HookReplayDisposition::Discarded
            }
            Err(HookIngestFailure::Retryable(_)) => fallback::HookReplayDisposition::Retryable,
        }
    })
    .unwrap();
    assert_eq!(report.applied, 0);
    assert_eq!(report.retained, 0);
    assert_eq!(runtime.state.lock().unwrap().generation, generation);
    assert!(!queued.exists());
    fs::remove_dir_all(root).unwrap();
}
