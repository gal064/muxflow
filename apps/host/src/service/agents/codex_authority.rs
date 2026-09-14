use std::{cmp::Ordering, collections::BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SessionAuthority {
    Current,
    Replacement,
    Move,
    Resume,
    Dismissed,
    Superseded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SessionPosition {
    Current,
    Predecessor { all_newer_terminal: bool },
    BoundElsewhere,
    KnownUnbound,
    Unknown,
}

pub(super) fn session(
    native_session_id: &str,
    position: SessionPosition,
    pane_owner_native_session_id: Option<&str>,
    route_verified: bool,
    event_name: &str,
    child_event: bool,
) -> SessionAuthority {
    let explicit_claim = matches!(event_name, "SessionStart" | "UserPromptSubmit");
    if native_session_id.is_empty()
        && pane_owner_native_session_id.is_some_and(|owner| !owner.is_empty())
    {
        return SessionAuthority::Superseded;
    }
    match pane_owner_native_session_id {
        Some(owner) if owner == native_session_id || owner.is_empty() => SessionAuthority::Current,
        Some(_) => match position {
            SessionPosition::Predecessor { .. } if event_name == "SessionEnd" => {
                SessionAuthority::Dismissed
            }
            SessionPosition::Predecessor { all_newer_terminal }
                if !child_event && (explicit_claim || all_newer_terminal) =>
            {
                SessionAuthority::Resume
            }
            SessionPosition::Predecessor { .. } => SessionAuthority::Superseded,
            SessionPosition::BoundElsewhere if route_verified => SessionAuthority::Move,
            _ if explicit_claim => SessionAuthority::Replacement,
            _ => SessionAuthority::Superseded,
        },
        None if position == SessionPosition::BoundElsewhere && route_verified => {
            SessionAuthority::Move
        }
        None if matches!(position, SessionPosition::BoundElsewhere) => SessionAuthority::Superseded,
        None => SessionAuthority::Current,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum RootTurnAuthority {
    Unscoped,
    Current,
    Successor,
    Stale,
    SessionTerminal,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct RootTurnInput<'a> {
    pub event_name: &'a str,
    pub turn_id: Option<&'a str>,
    pub active_turn_id: &'a str,
    pub terminal_turn_ids: &'a BTreeSet<String>,
    pub previous_hook_terminal: bool,
    pub child_event: bool,
}

pub(super) fn root_turn(input: RootTurnInput<'_>) -> RootTurnAuthority {
    if input.child_event {
        return RootTurnAuthority::Unscoped;
    }
    if matches!(input.event_name, "Interrupt" | "SessionEnd") {
        return RootTurnAuthority::SessionTerminal;
    }
    let Some(turn_id) = input.turn_id else {
        return RootTurnAuthority::Unscoped;
    };
    let terminal = input.event_name == "Stop";
    let activity = matches!(
        input.event_name,
        "UserPromptSubmit" | "PermissionRequest" | "PreToolUse" | "PostToolUse"
    );
    if !terminal && !activity {
        return RootTurnAuthority::Unscoped;
    }
    if input.terminal_turn_ids.contains(turn_id)
        || turn_order(turn_id, input.active_turn_id) == Some(Ordering::Less)
    {
        return RootTurnAuthority::Stale;
    }
    if input.active_turn_id == turn_id || terminal && input.active_turn_id.is_empty() {
        return RootTurnAuthority::Current;
    }
    if activity
        || input.previous_hook_terminal
        || input.terminal_turn_ids.contains(input.active_turn_id)
        || turn_order(turn_id, input.active_turn_id) == Some(Ordering::Greater)
    {
        RootTurnAuthority::Successor
    } else {
        RootTurnAuthority::Stale
    }
}

pub(super) fn turn_order(candidate: &str, active: &str) -> Option<Ordering> {
    let candidate = uuid::Uuid::parse_str(candidate).ok()?;
    let active = uuid::Uuid::parse_str(active).ok()?;
    (candidate.get_version_num() == 7 && active.get_version_num() == 7)
        .then(|| candidate.as_u128().cmp(&active.as_u128()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root<'a>(
        event_name: &'a str,
        turn_id: Option<&'a str>,
        active_turn_id: &'a str,
        terminal_turn_ids: &'a BTreeSet<String>,
        previous_hook_terminal: bool,
    ) -> RootTurnInput<'a> {
        RootTurnInput {
            event_name,
            turn_id,
            active_turn_id,
            terminal_turn_ids,
            previous_hook_terminal,
            child_event: false,
        }
    }

    #[test]
    fn a_foreign_session_needs_an_explicit_start_to_replace_the_pane_owner() {
        assert_eq!(
            session(
                "fork",
                SessionPosition::Unknown,
                Some("root"),
                true,
                "Stop",
                false,
            ),
            SessionAuthority::Superseded
        );
        assert_eq!(
            session(
                "fork",
                SessionPosition::Unknown,
                Some("root"),
                true,
                "UserPromptSubmit",
                false,
            ),
            SessionAuthority::Replacement
        );
        assert_eq!(
            session(
                "root",
                SessionPosition::BoundElsewhere,
                Some("fork"),
                true,
                "PreToolUse",
                false,
            ),
            SessionAuthority::Move
        );
        assert_eq!(
            session(
                "root",
                SessionPosition::BoundElsewhere,
                Some("fork"),
                false,
                "PreToolUse",
                false,
            ),
            SessionAuthority::Superseded
        );
        assert_eq!(
            session(
                "",
                SessionPosition::Unknown,
                Some("root"),
                true,
                "Stop",
                false,
            ),
            SessionAuthority::Superseded
        );
        assert_eq!(
            session(
                "root",
                SessionPosition::BoundElsewhere,
                None,
                false,
                "PreToolUse",
                false,
            ),
            SessionAuthority::Superseded
        );
        for event_name in ["SessionStart", "UserPromptSubmit"] {
            assert_eq!(
                session(
                    "root",
                    SessionPosition::Predecessor {
                        all_newer_terminal: true,
                    },
                    Some("side"),
                    true,
                    event_name,
                    true,
                ),
                SessionAuthority::Superseded
            );
        }
    }

    #[test]
    fn only_a_terminal_foreground_allows_implicit_predecessor_resume() {
        assert_eq!(
            session(
                "root",
                SessionPosition::Predecessor {
                    all_newer_terminal: false,
                },
                Some("side"),
                true,
                "PostToolUse",
                false,
            ),
            SessionAuthority::Superseded
        );
        assert_eq!(
            session(
                "root",
                SessionPosition::Predecessor {
                    all_newer_terminal: true,
                },
                Some("side"),
                false,
                "PostToolUse",
                false,
            ),
            SessionAuthority::Resume
        );
        assert_eq!(
            session(
                "root",
                SessionPosition::Predecessor {
                    all_newer_terminal: false,
                },
                Some("side"),
                false,
                "UserPromptSubmit",
                false,
            ),
            SessionAuthority::Resume
        );
        assert_eq!(
            session(
                "root",
                SessionPosition::Predecessor {
                    all_newer_terminal: false,
                },
                Some("side"),
                true,
                "SessionEnd",
                false,
            ),
            SessionAuthority::Dismissed
        );
        assert_eq!(
            session(
                "root",
                SessionPosition::Predecessor {
                    all_newer_terminal: true,
                },
                Some("side"),
                true,
                "SubagentStop",
                true,
            ),
            SessionAuthority::Superseded
        );
    }

    #[test]
    fn root_turns_distinguish_successors_from_stale_activity() {
        let active = "018f0000-0000-7000-8000-000000000002";
        let older = "018f0000-0000-7000-8000-000000000001";
        let newer = "018f0000-0000-7000-8000-000000000003";
        let terminal = BTreeSet::from([older.to_owned()]);

        assert_eq!(
            root_turn(root("PreToolUse", Some(newer), active, &terminal, false)),
            RootTurnAuthority::Successor
        );
        assert_eq!(
            root_turn(root(
                "UserPromptSubmit",
                Some(newer),
                active,
                &terminal,
                false,
            )),
            RootTurnAuthority::Successor
        );
        assert_eq!(
            root_turn(root("Stop", Some(older), active, &terminal, false)),
            RootTurnAuthority::Stale
        );
        assert_eq!(
            root_turn(root("Stop", Some(active), active, &terminal, false)),
            RootTurnAuthority::Current
        );
    }

    #[test]
    fn interrupt_and_session_end_are_session_authoritative() {
        let terminal = BTreeSet::new();
        for event_name in ["Interrupt", "SessionEnd"] {
            assert_eq!(
                root_turn(root(event_name, None, "active", &terminal, false)),
                RootTurnAuthority::SessionTerminal
            );
        }
    }

    #[test]
    fn an_unknown_event_cannot_claim_root_turn_ownership() {
        let terminal = BTreeSet::new();
        assert_eq!(
            root_turn(root(
                "private future event",
                Some("018f0000-0000-7000-8000-000000000003"),
                "018f0000-0000-7000-8000-000000000002",
                &terminal,
                false,
            )),
            RootTurnAuthority::Unscoped
        );
    }
}
