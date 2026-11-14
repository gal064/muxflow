use super::*;

fn store() -> (tempfile::TempDir, History) {
    let directory = tempfile::tempdir().unwrap();
    let history = History::load(directory.path().join("sessions.json"));
    (directory, history)
}

fn seen(session: &str, title: &str, at: i64) -> Seen {
    Seen {
        adapter_id: "claude-code".into(),
        native_session_id: session.into(),
        agent_id: format!("agent-{session}"),
        pane_id: format!("pane-{session}"),
        title: title.into(),
        fallback_title: "Claude Code".into(),
        workspace_name: "muxflow".into(),
        cwd: Some("/work".into()),
        chat: Some(false),
        last_active_at: at,
    }
}

fn titles(history: &History) -> Vec<String> {
    history
        .list()
        .entries
        .into_iter()
        .map(|entry| entry.title)
        .collect()
}

#[test]
fn a_session_stays_listed_after_its_agent_is_gone() {
    let (_directory, history) = store();
    history.observe(vec![seen("s1", "Fix tests", 10), seen("s2", "Docs", 20)]);
    let listed = history.list();
    assert_eq!(titles(&history), ["Docs", "Fix tests"]);
    assert!(listed.entries.iter().all(|entry| entry.running));
    assert_eq!(listed.entries[0].agent_id, "agent-s2");

    history.observe(vec![seen("s2", "Docs", 30)]);
    let listed = history.list();
    assert_eq!(listed.entries.len(), 2);
    let closed = &listed.entries[1];
    assert_eq!(closed.native_session_id, "s1");
    assert!(!closed.running && closed.agent_id.is_empty());
    assert_eq!(
        (closed.workspace_name.as_str(), closed.cwd.as_str()),
        ("muxflow", "/work")
    );
}

#[test]
fn the_title_follows_the_tab_and_a_generic_one_keeps_the_last() {
    let (_directory, history) = store();
    history.observe(vec![seen("s1", "", 1)]);
    assert_eq!(titles(&history), ["Claude Code"]);
    history.observe(vec![seen("s1", "Fix tests", 2)]);
    history.observe(vec![seen("s1", "", 3)]);
    assert_eq!(titles(&history), ["Fix tests"]);
}

#[test]
fn an_observation_without_a_topology_keeps_the_folder() {
    let (_directory, history) = store();
    history.observe(vec![seen("s1", "Fix tests", 1)]);
    history.observe(vec![Seen {
        cwd: None,
        chat: None,
        ..seen("s1", "Fix tests", 2)
    }]);
    assert_eq!(history.list().entries[0].cwd, "/work");
}

#[test]
fn a_side_fork_is_never_listed_and_a_branch_is() {
    let (_directory, history) = store();
    history.hide_pane("pane-s3");
    history.observe(vec![
        seen("s1", "Fix tests", 1),
        seen("s2", "Fix tests (fork)", 2),
        seen("s3", "Fix tests", 3),
    ]);
    // Still hidden once it closes, and after a restart.
    history.observe(Vec::new());
    history.maintain(1);
    let reloaded = History::load(history.path.clone());
    assert_eq!(titles(&reloaded), ["Fix tests (fork)", "Fix tests"]);
    assert!(reloaded.lookup("claude-code", "s3").is_some());
}

#[test]
fn a_session_the_chat_runtime_replaced_is_never_listed() {
    let (_directory, history) = store();
    let in_pane = |session: &str, at| Seen {
        pane_id: "%1".into(),
        ..seen(session, "Fix tests", at)
    };
    // An agent that ran in the pane earlier and was quit stays listed.
    history.observe(vec![in_pane("quit", 1)]);
    history.observe(Vec::new());
    // A terminal agent starts there and is replaced by its chat, which is
    // on another session. The agents service goes on reporting the first
    // until the second says it started.
    history.observe(vec![in_pane("terminal", 2)]);
    history.replaced("claude-code", "terminal");
    history.observe(vec![in_pane("terminal", 3)]);
    history.observe(vec![in_pane("chat", 4)]);
    let listed = |history: &History| -> Vec<String> {
        let entries = history.list().entries.into_iter();
        entries.map(|entry| entry.native_session_id).collect()
    };
    assert_eq!(listed(&history), ["chat", "quit"]);
    // One that is replaced before it was ever reported is not listed when
    // it is, and none of it is forgotten by a restart.
    history.replaced("claude-code", "late");
    history.observe(vec![in_pane("chat", 5), seen("late", "Fix tests", 6)]);
    history.maintain(7);
    assert_eq!(
        listed(&History::load(history.path.clone())),
        ["chat", "quit"]
    );
}

#[test]
fn a_side_fork_that_becomes_a_chat_is_still_not_listed() {
    let (_directory, history) = store();
    let fork = |session: &str, at| Seen {
        pane_id: "%2".into(),
        ..seen(session, "Fix tests (fork)", at)
    };
    history.hide_pane("%2");
    history.observe(vec![seen("source", "Fix tests", 1), fork("terminal", 2)]);
    // The fork's agent is replaced by its chat, which copies the source
    // again: the pane is marked once more, for the chat's session.
    history.replaced("claude-code", "terminal");
    history.hide_pane("%2");
    history.observe(vec![seen("source", "Fix tests", 3), fork("terminal", 4)]);
    history.observe(vec![seen("source", "Fix tests", 5), fork("chat", 6)]);
    assert_eq!(titles(&history), ["Fix tests"]);
    // Whatever runs in the pane after the fork is a session of its own.
    history.observe(vec![fork("later", 7)]);
    assert_eq!(titles(&history), ["Fix tests (fork)", "Fix tests"]);
}

#[test]
fn a_snooze_due_while_its_session_runs_ends_without_waking() {
    let (_directory, history) = store();
    history.observe(vec![seen("s1", "Fix tests", 1)]);
    history.snooze("claude-code", "s1", 5).unwrap();
    assert_eq!(history.maintain(5), None);
    let entry = history.lookup("claude-code", "s1").unwrap();
    assert!(!entry.woke && entry.snoozed_until_unix_millis == 0);
}

#[test]
fn a_snooze_taken_back_restores_what_was_there() {
    let (_directory, history) = store();
    history.observe(vec![seen("s1", "Fix tests", 1)]);
    history.snooze("claude-code", "s1", 50).unwrap();
    history.snooze("claude-code", "s1", 90).unwrap();
    history.restore("claude-code", "s1", 50, false).unwrap();
    let entry = history.lookup("claude-code", "s1").unwrap();
    assert_eq!(entry.snoozed_until_unix_millis, 50);
}

#[test]
fn activity_alone_is_written_once_a_minute() {
    let (_directory, history) = store();
    history.observe(vec![seen("s1", "Fix tests", 1)]);
    history.maintain(100_000);
    history.observe(vec![seen("s1", "Fix tests", 2)]);
    history.maintain(110_000);
    let on_disk =
        || History::load(history.path.clone()).list().entries[0].last_active_at_unix_millis;
    assert_eq!(on_disk(), 1);
    history.maintain(160_000);
    assert_eq!(on_disk(), 2);
}

#[test]
fn an_unreadable_store_is_kept_aside() {
    let (directory, _) = store();
    let path = directory.path().join("sessions.json");
    std::fs::write(&path, b"{not json").unwrap();
    let history = History::load(path.clone());
    assert!(history.list().entries.is_empty());
    assert!(directory.path().join("sessions.json.unreadable").exists());
    assert!(!path.exists());
}

#[test]
fn a_snooze_survives_a_restart_and_wakes_once_due() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.json");
    let history = History::load(path.clone());
    history.observe(vec![seen("s1", "Fix tests", 1)]);
    history.snooze("claude-code", "s1", 60_000).unwrap();
    history.observe(Vec::new());
    drop(history);

    let history = History::load(path);
    history.observe(Vec::new());
    let entry = history.lookup("claude-code", "s1").unwrap();
    assert_eq!(entry.snoozed_until_unix_millis, 60_000);
    assert!(!entry.woke && !entry.running);
    assert_eq!(history.maintain(59_999), None);

    let event = history.maintain(60_000).unwrap();
    assert_eq!(event.woke.len(), 1);
    assert_eq!(event.woke[0].title, "Fix tests");
    let entry = history.lookup("claude-code", "s1").unwrap();
    assert!(entry.woke);
    assert_eq!(entry.snoozed_until_unix_millis, 0);
    // Told once.
    assert_eq!(history.maintain(60_001), None);
}

#[test]
fn wake_now_leaves_a_placeholder_without_a_notification() {
    let (_directory, history) = store();
    history.observe(vec![seen("s1", "Fix tests", 1)]);
    history.snooze("claude-code", "s1", i64::MAX).unwrap();
    let event = history.wake("claude-code", "s1").unwrap();
    assert!(event.woke.is_empty());
    assert!(history.lookup("claude-code", "s1").unwrap().woke);
}

#[test]
fn running_again_ends_the_placeholder_and_says_so() {
    let (_directory, history) = store();
    history.observe(vec![seen("s1", "Fix tests", 1)]);
    history.snooze("claude-code", "s1", 5).unwrap();
    // Still running while its pane closes: not a new start.
    history.observe(vec![seen("s1", "Fix tests", 2)]);
    assert_eq!(
        history
            .lookup("claude-code", "s1")
            .unwrap()
            .snoozed_until_unix_millis,
        5
    );
    history.observe(Vec::new());
    history.maintain(10).unwrap();

    history.observe(vec![seen("s1", "Fix tests", 20)]);
    let entry = history.lookup("claude-code", "s1").unwrap();
    assert!(!entry.woke && entry.running);
    let event = history.maintain(21).unwrap();
    assert!(event.woke.is_empty());
}

#[test]
fn the_first_look_after_a_restart_is_not_a_new_start() {
    let (_directory, history) = store();
    history.observe(Vec::new());
    history.snooze("claude-code", "s1", 5).unwrap_err();
    history.observe(vec![seen("s1", "Fix tests", 1)]);
    history.snooze("claude-code", "s1", 5).unwrap();

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sessions.json");
    std::fs::copy(&history.path, &path).unwrap();
    let restarted = History::load(path);
    restarted.observe(vec![seen("s1", "Fix tests", 2)]);
    assert_eq!(
        restarted
            .lookup("claude-code", "s1")
            .unwrap()
            .snoozed_until_unix_millis,
        5
    );
}

#[test]
fn resuming_clears_the_snooze_and_the_placeholder() {
    let (_directory, history) = store();
    history.observe(vec![seen("s1", "Fix tests", 1)]);
    history.snooze("claude-code", "s1", 5).unwrap();
    history.observe(Vec::new());
    history.maintain(5).unwrap();
    history.resumed("claude-code", "s1").unwrap();
    let entry = history.lookup("claude-code", "s1").unwrap();
    assert!(!entry.woke && entry.snoozed_until_unix_millis == 0);
}

#[test]
fn the_oldest_go_first_past_the_cap_but_never_a_snoozed_one() {
    let (_directory, history) = store();
    history.observe(vec![seen("old", "Oldest", 0)]);
    history.snooze("claude-code", "old", i64::MAX).unwrap();
    let many: Vec<Seen> = (1..=MAX_ENTRIES as i64)
        .map(|at| seen(&format!("s{at}"), "Session", at))
        .collect();
    history.observe(many);
    history.observe(Vec::new());
    let listed = history.list().entries;
    assert_eq!(listed.len(), MAX_ENTRIES);
    assert!(listed.iter().any(|entry| entry.native_session_id == "old"));
    assert!(!listed.iter().any(|entry| entry.native_session_id == "s1"));
}

#[test]
fn an_unknown_session_cannot_be_snoozed() {
    let (_directory, history) = store();
    assert_eq!(
        history.snooze("codex", "nope", 5).unwrap_err().code,
        "history_unknown_session"
    );
}

#[test]
fn a_tab_title_drops_the_ticker_and_skips_generic_names() {
    assert_eq!(tab_title("✳ Fix tests", "claude"), "Fix tests");
    assert_eq!(
        tab_title("", "Release notes (fork)"),
        "Release notes (fork)"
    );
    assert_eq!(tab_title("codex", "Codex"), "");
    assert_eq!(tab_title("", "sh"), "");
    assert_eq!(
        tab_title("0123abcd-0123-4567-89ab-0123456789ab", "docs"),
        "docs"
    );
    assert_eq!(tab_title("⠋ ", "Review"), "Review");
}

#[test]
fn a_resume_under_way_cannot_be_placed_twice() {
    let (_directory, history) = store();
    assert!(history.claim_resume("claude-code", "s1", 0));
    assert!(!history.claim_resume("claude-code", "s1", 1));
    // Seen running: the claim is over, and running refuses on its own.
    history.observe(vec![seen("s1", "Fix tests", 2)]);
    history.observe(Vec::new());
    assert!(history.claim_resume("claude-code", "s1", 3));
    history.release_resume("claude-code", "s1");
    assert!(history.claim_resume("claude-code", "s1", 4));
    assert!(history.claim_resume("claude-code", "s1", 4 + RESUME_CLAIM_MILLIS));
}

#[test]
fn nothing_wakes_before_the_first_look_after_a_start() {
    let (_directory, history) = store();
    history.observe(vec![seen("s1", "Fix tests", 1)]);
    history.snooze("claude-code", "s1", 5).unwrap();
    let restarted = History::load(history.path.clone());
    let started = restarted.inner().loaded_at;
    assert_eq!(restarted.maintain(started), None);
    assert!(restarted.maintain(started + STARTUP_GRACE_MILLIS).is_some());
}

#[test]
fn a_placeholder_running_at_the_first_look_is_over() {
    let (_directory, history) = store();
    history.observe(vec![seen("s1", "Fix tests", 1)]);
    history.snooze("claude-code", "s1", 5).unwrap();
    history.observe(Vec::new());
    history.maintain(5).unwrap();
    let restarted = History::load(history.path.clone());
    restarted.observe(vec![seen("s1", "Fix tests", 6)]);
    assert!(!restarted.lookup("claude-code", "s1").unwrap().woke);
}
