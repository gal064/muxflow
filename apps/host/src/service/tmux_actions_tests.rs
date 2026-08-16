use super::*;

fn session(id: &str, name: &str, order: u32) -> tmux_control::Session {
    tmux_control::Session {
        id: id.into(),
        name: name.into(),
        window_count: 2,
        attached_clients: 0,
        order,
    }
}

fn window(id: &str, index: u32, active: bool) -> tmux_control::Window {
    tmux_control::Window {
        id: id.into(),
        session_id: "$1".into(),
        index,
        name: id.into(),
        active,
        layout: format!("layout-{id}"),
        zoomed: false,
    }
}

fn pane(id: &str, window_id: &str, active: bool) -> tmux_control::Pane {
    tmux_control::Pane {
        id: id.into(),
        session_id: "$1".into(),
        window_id: window_id.into(),
        index: 0,
        active,
        width: 80,
        height: 24,
        left: 0,
        top: 0,
        current_path: String::new(),
        current_command: "bash".into(),
        pane_pid: 0,
        start_command: String::new(),
    }
}

fn topology() -> tmux_control::TmuxSnapshot {
    tmux_control::TmuxSnapshot {
        sessions: vec![session("$1", "one", 0), session("$2", "two", 1)],
        windows: vec![window("@1", 0, true), window("@2", 1, false)],
        panes: vec![pane("%1", "@1", true), pane("%2", "@2", true)],
    }
}

#[test]
fn creating_the_first_session_is_the_only_action_allowed_with_no_tmux_server() {
    assert!(bootstraps_server(
        v1::TmuxActionKind::CreateSession,
        "tmux:none"
    ));
    // With a server already running this is an ordinary create, not a
    // bootstrap, and must still go through consistent discovery.
    assert!(!bootstraps_server(
        v1::TmuxActionKind::CreateSession,
        "tmux:12345"
    ));
    for kind in [
        v1::TmuxActionKind::CreateWindow,
        v1::TmuxActionKind::SplitPaneRight,
        v1::TmuxActionKind::RenameSession,
        v1::TmuxActionKind::SelectSession,
        v1::TmuxActionKind::CloseSession,
    ] {
        assert!(!bootstraps_server(kind, "tmux:none"), "{kind:?}");
    }
}

#[test]
fn every_painted_workflow_resolves_one_authoritative_pane() {
    let snapshot = topology();
    let cases = [
        (
            v1::TmuxActionKind::CreateSession,
            v1::TmuxAction::default(),
            v1::TmuxActionResult {
                session_id: "$1".into(),
                ..Default::default()
            },
            "%1",
        ),
        (
            v1::TmuxActionKind::CreateWindow,
            v1::TmuxAction::default(),
            v1::TmuxActionResult {
                window_id: "@2".into(),
                ..Default::default()
            },
            "%2",
        ),
        (
            v1::TmuxActionKind::SelectWindow,
            v1::TmuxAction {
                window_id: "@1".into(),
                ..Default::default()
            },
            v1::TmuxActionResult::default(),
            "%1",
        ),
        (
            v1::TmuxActionKind::SplitPaneRight,
            v1::TmuxAction::default(),
            v1::TmuxActionResult {
                pane_id: "%2".into(),
                ..Default::default()
            },
            "%2",
        ),
    ];
    for (kind, action, result, expected) in cases {
        assert_eq!(
            interaction_pane_id(kind, &action, &result, &snapshot).as_deref(),
            Some(expected),
            "{kind:?}"
        );
    }
}

#[test]
fn action_discovery_probes_identity_only_for_failed_create_session_discovery() {
    let discovered = (topology(), "tmux:live".to_owned());
    let resolved = normalize_pre_action_discovery(
        v1::TmuxActionKind::CreateWindow,
        Ok(discovered.clone()),
        || panic!("successful discovery must not fork an identity probe"),
    )
    .unwrap();
    assert_eq!(resolved, discovered);

    let resolved = normalize_pre_action_discovery(
        v1::TmuxActionKind::CreateSession,
        Err(anyhow::anyhow!("server unavailable")),
        || "tmux:none".into(),
    )
    .unwrap();
    assert_eq!(
        resolved,
        (tmux_control::TmuxSnapshot::default(), "tmux:none".into())
    );

    let rejected = normalize_pre_action_discovery(
        v1::TmuxActionKind::CreateWindow,
        Err(anyhow::anyhow!("server unavailable")),
        || panic!("a non-bootstrap action must not fork an identity probe"),
    );
    assert!(rejected.is_err());
}

#[test]
fn action_epoch_barrier_precedes_the_single_post_discovery() {
    let order = std::cell::RefCell::new(Vec::new());
    let expected = topology();
    let (snapshot, identity, epoch) = finalize_action(
        v1::TmuxActionKind::CreateWindow,
        || {
            order.borrow_mut().push("action-epoch-barrier");
            Some(7)
        },
        || {
            order.borrow_mut().push("post-discovery");
            Ok((expected.clone(), "tmux:live".into()))
        },
        || panic!("successful post-discovery must not fork an identity probe"),
    )
    .unwrap();
    assert_eq!(*order.borrow(), ["action-epoch-barrier", "post-discovery"]);
    assert_eq!(snapshot, expected);
    assert_eq!(identity, "tmux:live");
    assert_eq!(epoch, Some(7));
}

#[test]
fn failed_epoch_capture_still_runs_authoritative_post_discovery() {
    let discovered = std::cell::Cell::new(false);
    let expected = topology();
    let (snapshot, identity, epoch) = finalize_action(
        v1::TmuxActionKind::CreateWindow,
        || None,
        || {
            discovered.set(true);
            Ok((expected.clone(), "tmux:live".into()))
        },
        || panic!("successful post-discovery must not fork an identity probe"),
    )
    .unwrap();
    assert!(discovered.get());
    assert_eq!(snapshot, expected);
    assert_eq!(identity, "tmux:live");
    assert_eq!(epoch, None);
}

#[test]
fn post_command_discovery_failures_are_outcome_unknown() {
    let discovery_error = finalize_action(
        v1::TmuxActionKind::CreateWindow,
        || Some(7),
        || Err(anyhow::anyhow!("tmux unavailable")),
        || "tmux:still-running".into(),
    )
    .unwrap_err();
    assert!(discovery_error.to_string().starts_with("outcome unknown"));
}

#[test]
fn only_bootstrap_and_last_close_may_change_the_server_identity() {
    // The bootstrap goes from no server to a real one.
    assert!(identity_transition_allowed(
        v1::TmuxActionKind::CreateSession,
        true,
        "tmux:none",
        "tmux:12345"
    ));
    // A bootstrap that left no server behind failed, however tmux exited.
    assert!(!identity_transition_allowed(
        v1::TmuxActionKind::CreateSession,
        true,
        "tmux:none",
        "tmux:none"
    ));
    // Closing the last object legitimately ends the server.
    assert!(identity_transition_allowed(
        v1::TmuxActionKind::CloseSession,
        false,
        "tmux:12345",
        "tmux:none"
    ));
    // Any other identity change means the outcome is unknown.
    assert!(!identity_transition_allowed(
        v1::TmuxActionKind::RenameSession,
        false,
        "tmux:12345",
        "tmux:67890"
    ));
    assert!(!identity_transition_allowed(
        v1::TmuxActionKind::CreateSession,
        false,
        "tmux:12345",
        "tmux:67890"
    ));
    // An unchanged identity is always fine.
    assert!(identity_transition_allowed(
        v1::TmuxActionKind::RenameWindow,
        false,
        "tmux:12345",
        "tmux:12345"
    ));
}

#[test]
fn destructive_actions_require_explicit_confirmation() {
    assert!(require_confirmation(v1::TmuxActionKind::ClosePane, false).is_err());
    assert!(require_confirmation(v1::TmuxActionKind::ClosePane, true).is_ok());
    assert!(require_confirmation(v1::TmuxActionKind::RenameWindow, true).is_ok());
}

#[test]
fn closing_the_last_tmux_object_commits_authoritative_empty_topology() {
    for kind in [
        v1::TmuxActionKind::CloseSession,
        v1::TmuxActionKind::CloseWindow,
        v1::TmuxActionKind::ClosePane,
    ] {
        let (snapshot, identity) =
            normalize_post_action(kind, Err(anyhow::anyhow!("no server")), || {
                "tmux:none".into()
            })
            .unwrap();
        assert_eq!(snapshot, tmux_control::TmuxSnapshot::default());
        assert_eq!(identity, "tmux:none");
    }
    assert!(
        normalize_post_action(
            v1::TmuxActionKind::RenameSession,
            Err(anyhow::anyhow!("no server")),
            || "tmux:none".into(),
        )
        .is_err()
    );
    assert!(
        normalize_post_action(
            v1::TmuxActionKind::CloseSession,
            Err(anyhow::anyhow!("discovery failed")),
            || "tmux:still-running".into(),
        )
        .is_err()
    );
}

#[test]
fn sparse_window_indices_use_relative_identity_not_numeric_gaps() {
    // Presentation indices [1, 3, 5] correspond to positions [0, 1, 2].
    assert_eq!(
        relative_reorder_position(2, 1, 3, v1::WindowRelativePosition::Before).unwrap(),
        1
    );
    assert_eq!(
        relative_reorder_position(0, 1, 3, v1::WindowRelativePosition::After).unwrap(),
        1
    );
    assert!(relative_reorder_position(0, 1, 3, v1::WindowRelativePosition::Unspecified).is_err());
}

#[test]
fn external_interleave_fails_reorder_postcondition() {
    let committed = tmux_control::TmuxSnapshot {
        windows: vec![
            window("@1", 1, false),
            window("@2", 3, false),
            window("@3", 5, false),
        ],
        ..Default::default()
    };
    assert!(window_reorder_postcondition(
        &committed,
        "$1",
        "@2",
        "@3",
        v1::WindowRelativePosition::Before,
    ));
    let interleaved = tmux_control::TmuxSnapshot {
        windows: vec![
            window("@1", 1, false),
            window("@2", 3, false),
            window("@4", 4, false),
            window("@3", 5, false),
        ],
        ..Default::default()
    };
    assert!(!window_reorder_postcondition(
        &interleaved,
        "$1",
        "@2",
        "@3",
        v1::WindowRelativePosition::Before,
    ));
    let command = window_reorder_command("@2", &["@3", "@4"]);
    let arguments: Vec<_> = command
        .get_args()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        arguments.iter().filter(|argument| *argument == ";").count(),
        1
    );
    assert_eq!(
        arguments
            .iter()
            .filter(|argument| *argument == "swap-window")
            .count(),
        2,
        "all swaps must be submitted as one tmux command list"
    );
}

#[test]
fn every_action_has_an_identity_relative_authoritative_postcondition() {
    let before = topology();
    let check = |kind, action: v1::TmuxAction, result, after| {
        assert!(action_postcondition(
            kind, &action, &result, &before, &after
        ));
    };

    let mut after = before.clone();
    after.sessions.push(session("$3", "new", 2));
    check(
        v1::TmuxActionKind::CreateSession,
        v1::TmuxAction {
            name: "new".into(),
            ..Default::default()
        },
        v1::TmuxActionResult {
            session_id: "$3".into(),
            ..Default::default()
        },
        after,
    );
    let mut after = before.clone();
    after.sessions[0].name = "renamed".into();
    check(
        v1::TmuxActionKind::RenameSession,
        v1::TmuxAction {
            session_id: "$1".into(),
            name: "renamed".into(),
            ..Default::default()
        },
        Default::default(),
        after,
    );
    let mut after = before.clone();
    after.sessions[0].order = 1;
    after.sessions[1].order = 0;
    check(
        v1::TmuxActionKind::ReorderSession,
        v1::TmuxAction {
            session_id: "$2".into(),
            index: 0,
            ..Default::default()
        },
        Default::default(),
        after,
    );
    check(
        v1::TmuxActionKind::SelectSession,
        v1::TmuxAction {
            session_id: "$1".into(),
            ..Default::default()
        },
        Default::default(),
        before.clone(),
    );
    let mut after = before.clone();
    after.sessions.retain(|item| item.id != "$2");
    check(
        v1::TmuxActionKind::CloseSession,
        v1::TmuxAction {
            session_id: "$2".into(),
            ..Default::default()
        },
        Default::default(),
        after,
    );

    let mut after = before.clone();
    after.windows[0].active = false;
    let mut created = window("@3", 2, true);
    created.name = "new".into();
    after.windows.push(created);
    check(
        v1::TmuxActionKind::CreateWindow,
        v1::TmuxAction {
            session_id: "$1".into(),
            name: "new".into(),
            ..Default::default()
        },
        v1::TmuxActionResult {
            window_id: "@3".into(),
            ..Default::default()
        },
        after,
    );
    let mut after = before.clone();
    after.windows[0].name = "renamed".into();
    check(
        v1::TmuxActionKind::RenameWindow,
        v1::TmuxAction {
            window_id: "@1".into(),
            name: "renamed".into(),
            ..Default::default()
        },
        Default::default(),
        after,
    );
    let mut after = before.clone();
    after.windows[0].index = 1;
    after.windows[1].index = 0;
    check(
        v1::TmuxActionKind::ReorderWindow,
        v1::TmuxAction {
            session_id: "$1".into(),
            window_id: "@1".into(),
            target_window_id: "@2".into(),
            relative_position: v1::WindowRelativePosition::After.into(),
            ..Default::default()
        },
        Default::default(),
        after,
    );
    let mut after = before.clone();
    after.windows[0].active = false;
    after.windows[1].active = true;
    check(
        v1::TmuxActionKind::SelectWindow,
        v1::TmuxAction {
            window_id: "@2".into(),
            ..Default::default()
        },
        Default::default(),
        after,
    );
    let mut after = before.clone();
    after.windows.retain(|item| item.id != "@2");
    check(
        v1::TmuxActionKind::CloseWindow,
        v1::TmuxAction {
            window_id: "@2".into(),
            ..Default::default()
        },
        Default::default(),
        after,
    );

    for (kind, left, top) in [
        (v1::TmuxActionKind::SplitPaneRight, 40, 0),
        (v1::TmuxActionKind::SplitPaneDown, 0, 12),
    ] {
        let mut after = before.clone();
        let mut created = pane("%3", "@1", false);
        created.left = left;
        created.top = top;
        after.panes.push(created);
        check(
            kind,
            v1::TmuxAction {
                pane_id: "%1".into(),
                ..Default::default()
            },
            v1::TmuxActionResult {
                pane_id: "%3".into(),
                ..Default::default()
            },
            after,
        );
    }
    let mut after = before.clone();
    after.panes[0].active = false;
    after.panes[1].active = true;
    check(
        v1::TmuxActionKind::FocusPane,
        v1::TmuxAction {
            pane_id: "%2".into(),
            ..Default::default()
        },
        Default::default(),
        after,
    );
    for kind in [
        v1::TmuxActionKind::ResizePaneLeft,
        v1::TmuxActionKind::ResizePaneRight,
        v1::TmuxActionKind::ResizePaneUp,
        v1::TmuxActionKind::ResizePaneDown,
    ] {
        let mut after = before.clone();
        after.panes[0].width = 79;
        check(
            kind,
            v1::TmuxAction {
                pane_id: "%1".into(),
                ..Default::default()
            },
            Default::default(),
            after,
        );
    }
    let mut after = before.clone();
    after.windows[0].zoomed = true;
    check(
        v1::TmuxActionKind::ZoomPane,
        v1::TmuxAction {
            pane_id: "%1".into(),
            zoomed: true,
            ..Default::default()
        },
        Default::default(),
        after,
    );
    let mut after = before.clone();
    after.panes.retain(|item| item.id != "%1");
    check(
        v1::TmuxActionKind::ClosePane,
        v1::TmuxAction {
            pane_id: "%1".into(),
            ..Default::default()
        },
        Default::default(),
        after,
    );
}

#[test]
fn ordinary_client_undo_never_reports_false_success() {
    let before = topology();
    assert!(!action_postcondition(
        v1::TmuxActionKind::RenameWindow,
        &v1::TmuxAction {
            window_id: "@1".into(),
            name: "renamed".into(),
            ..Default::default()
        },
        &Default::default(),
        &before,
        &before,
    ));
    let mut focus_undone = before.clone();
    focus_undone.panes[0].active = false;
    assert!(!action_postcondition(
        v1::TmuxActionKind::FocusPane,
        &v1::TmuxAction {
            pane_id: "%1".into(),
            ..Default::default()
        },
        &Default::default(),
        &before,
        &focus_undone,
    ));
    assert!(!action_postcondition(
        v1::TmuxActionKind::ResizePaneRight,
        &v1::TmuxAction {
            pane_id: "%1".into(),
            ..Default::default()
        },
        &Default::default(),
        &before,
        &before,
    ));
    assert!(!action_postcondition(
        v1::TmuxActionKind::ZoomPane,
        &v1::TmuxAction {
            pane_id: "%1".into(),
            zoomed: true,
            ..Default::default()
        },
        &Default::default(),
        &before,
        &before,
    ));
}
