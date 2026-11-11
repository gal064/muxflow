//! Which agent transitions become a native notification, and what it says.
//! Port of `notifications.ts`'s policy; delivery is `desktop-core`'s.

use serde_json::{Value, json};

use super::labels::{agent_session_label, strip_agent_status_glyphs};
use super::model::{AdapterDescriptor, AgentRecord, AttentionKind, Generation, Lifecycle};

/// What the user is looking at, so a notification about it can be dropped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentFocus {
    pub host_profile_id: String,
    pub server_identity: String,
    pub session_id: String,
    pub window_id: String,
    pub pane_id: String,
    pub app_focused: bool,
    pub terminal_visible: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationRoute {
    pub host_profile_id: String,
    pub server_identity: String,
    pub session_id: String,
    pub session_name: String,
    pub window_id: String,
    pub window_name: String,
    pub pane_id: String,
    pub agent_id: String,
    pub attention_generation: Generation,
}

impl NotificationRoute {
    /// The route as `desktop-core` reads it, generation as a decimal string.
    pub fn to_json(&self) -> Value {
        json!({
            "hostProfile": self.host_profile_id,
            "serverIdentity": self.server_identity,
            "sessionId": self.session_id,
            "sessionName": self.session_name,
            "windowId": self.window_id,
            "windowName": self.window_name,
            "paneId": self.pane_id,
            "agentId": self.agent_id,
            "attentionGeneration": self.attention_generation.to_string(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentNativeNotification {
    pub event: AttentionKind,
    pub title: String,
    pub body: String,
    pub request_action: bool,
    pub present_in_foreground: bool,
    pub route: NotificationRoute,
}

impl AgentNativeNotification {
    /// `desktop-core`'s `AgentNotificationContent`.
    pub fn to_json(&self) -> Value {
        json!({
            "title": self.title,
            "body": self.body,
            "requestAction": self.request_action,
            "presentInForeground": self.present_in_foreground,
            "route": self.route.to_json(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Emitted,
    SuppressedReplay,
    SuppressedDuplicate,
    SuppressedFocused,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationDecision {
    Emit(Box<AgentNativeNotification>),
    Ignore,
    Suppress(Outcome),
}

#[derive(Clone, Debug, Default)]
pub struct TransitionContext<'a> {
    pub focus: AgentFocus,
    pub replayed: bool,
    pub previously_notified: Option<Generation>,
    pub workspace_name: Option<String>,
    pub window_name: Option<String>,
    pub adapters: &'a [AdapterDescriptor],
    pub reconciled_snapshot: bool,
}

pub fn decide_agent_notification(
    previous: Option<&AgentRecord>,
    next: &AgentRecord,
    context: &TransitionContext<'_>,
) -> NotificationDecision {
    let attention_advanced =
        previous.is_some_and(|previous| next.attention_generation > previous.attention_generation);
    let persisted = attention_advanced.then_some(next.attention_kind).flatten();
    let previous_lifecycle = previous.map(|previous| previous.lifecycle);
    let event = persisted.or_else(|| {
        if next.lifecycle == Lifecycle::Blocked
            && (previous_lifecycle != Some(Lifecycle::Blocked)
                || (context.reconciled_snapshot && attention_advanced))
        {
            Some(AttentionKind::Blocked)
        } else if previous_lifecycle == Some(Lifecycle::Working)
            && next.lifecycle == Lifecycle::Idle
            && attention_advanced
        {
            Some(AttentionKind::Completed)
        } else {
            None
        }
    });
    let Some(event) = event else {
        return NotificationDecision::Ignore;
    };
    let generation = next.attention_generation;
    if context.replayed {
        return NotificationDecision::Suppress(Outcome::SuppressedReplay);
    }
    if context
        .previously_notified
        .is_some_and(|notified| notified >= generation)
    {
        return NotificationDecision::Suppress(Outcome::SuppressedDuplicate);
    }
    let focus = &context.focus;
    let focused_pane = focus.app_focused
        && focus.terminal_visible
        && focus.host_profile_id == next.host_profile_id
        && focus.server_identity == next.server_identity
        && focus.session_id == next.session_id
        && focus.window_id == next.window_id
        && focus.pane_id == next.pane_id;
    if focused_pane {
        return NotificationDecision::Suppress(Outcome::SuppressedFocused);
    }
    let window_name = context.window_name.as_deref().unwrap_or(&next.window_name);
    let workspace = safe_label(
        context
            .workspace_name
            .as_deref()
            .unwrap_or(&next.session_name),
        "Workspace",
    );
    // Stripped like every other surface names a tab: the ticker is a second
    // report of the state this notification is already about.
    let terminal = safe_label(strip_agent_status_glyphs(window_name), "Terminal");
    let labelled = AgentRecord {
        window_name: window_name.to_owned(),
        ..next.clone()
    };
    let agent = safe_label(&agent_session_label(&labelled, context.adapters), "Agent");
    let (verb, state) = match event {
        AttentionKind::Blocked => ("needs attention", "Blocked"),
        AttentionKind::Completed => ("finished", "Done"),
    };
    NotificationDecision::Emit(Box::new(AgentNativeNotification {
        event,
        title: format!("{agent} {verb}"),
        // Deliberately excludes prompt, output, paths, and hook payloads.
        body: if next.pane_id.is_empty() {
            format!("Unmapped · {state} · Review in Agents")
        } else {
            format!("{workspace} · {terminal} · {state}")
        },
        request_action: !next.pane_id.is_empty(),
        // Anything that survives the focus rule is about a pane the user cannot
        // see, so it is worth showing even with the app in front.
        present_in_foreground: true,
        route: NotificationRoute {
            host_profile_id: next.host_profile_id.clone(),
            server_identity: next.server_identity.clone(),
            session_id: next.session_id.clone(),
            session_name: next.session_name.clone(),
            window_id: next.window_id.clone(),
            window_name: next.window_name.clone(),
            pane_id: next.pane_id.clone(),
            agent_id: next.id.clone(),
            attention_generation: generation,
        },
    }))
}

const MAX_LABEL_BYTES: usize = 96;

/// One line, collapsed whitespace, at most 96 bytes on a character boundary.
fn safe_label(value: &str, fallback: &str) -> String {
    let normalized = value
        .split(|character: char| character.is_control() || character.is_whitespace())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut result = String::new();
    for character in normalized.chars() {
        if result.len() + character.len_utf8() > MAX_LABEL_BYTES {
            break;
        }
        result.push(character);
    }
    if result.is_empty() {
        fallback.to_owned()
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::model::fixtures::agent;

    fn focus() -> AgentFocus {
        AgentFocus {
            host_profile_id: "local".into(),
            server_identity: "server-a".into(),
            session_id: "$1".into(),
            window_id: "@1".into(),
            pane_id: "%1".into(),
            app_focused: false,
            terminal_visible: true,
        }
    }

    fn context() -> TransitionContext<'static> {
        TransitionContext {
            focus: focus(),
            ..TransitionContext::default()
        }
    }

    fn blocked_at(generation: u64) -> AgentRecord {
        AgentRecord {
            lifecycle: Lifecycle::Blocked,
            attention_generation: generation,
            lifecycle_generation: 4,
            ..agent()
        }
    }

    fn emitted(decision: NotificationDecision) -> AgentNativeNotification {
        match decision {
            NotificationDecision::Emit(notification) => *notification,
            other => panic!("expected an emit, got {other:?}"),
        }
    }

    #[test]
    fn emits_only_background_blocked_and_working_to_idle_completion_transitions() {
        emitted(decide_agent_notification(
            Some(&agent()),
            &blocked_at(4),
            &context(),
        ));
        let before = AgentRecord {
            attention_generation: 4,
            ..agent()
        };
        let after = AgentRecord {
            lifecycle: Lifecycle::Idle,
            attention_generation: 5,
            lifecycle_generation: 4,
            ..agent()
        };
        assert_eq!(
            emitted(decide_agent_notification(Some(&before), &after, &context())).event,
            AttentionKind::Completed
        );
        assert_eq!(
            decide_agent_notification(None, &after, &context()),
            NotificationDecision::Ignore
        );
    }

    #[test]
    fn suppresses_focused_replayed_and_duplicate_generations() {
        let next = blocked_at(4);
        let mut focused = context();
        focused.focus.app_focused = true;
        assert_eq!(
            decide_agent_notification(Some(&agent()), &next, &focused),
            NotificationDecision::Suppress(Outcome::SuppressedFocused)
        );
        let replayed = TransitionContext {
            replayed: true,
            ..context()
        };
        assert_eq!(
            decide_agent_notification(Some(&agent()), &next, &replayed),
            NotificationDecision::Suppress(Outcome::SuppressedReplay)
        );
        let duplicate = TransitionContext {
            previously_notified: Some(4),
            ..context()
        };
        assert_eq!(
            decide_agent_notification(Some(&agent()), &next, &duplicate),
            NotificationDecision::Suppress(Outcome::SuppressedDuplicate)
        );
    }

    #[test]
    fn uses_the_persisted_attention_cause_after_a_complete_offline_lifecycle_cycle() {
        let at = |generation| AgentRecord {
            lifecycle: Lifecycle::Idle,
            attention_generation: generation,
            attention_kind: Some(AttentionKind::Completed),
            ..agent()
        };
        let reconciled = TransitionContext {
            reconciled_snapshot: true,
            ..context()
        };
        let notification = emitted(decide_agent_notification(Some(&at(4)), &at(6), &reconciled));
        assert_eq!(notification.event, AttentionKind::Completed);
    }

    #[test]
    fn keeps_prompt_output_path_and_native_hook_identity_out_of_lock_screen_text() {
        let secret = AgentRecord {
            display_name: "Codex\nAgent".into(),
            native_session_id: "secret-native".into(),
            ..blocked_at(4)
        };
        let named = TransitionContext {
            workspace_name: Some("work\nspace".into()),
            window_name: Some("agent".into()),
            ..context()
        };
        let notification = emitted(decide_agent_notification(Some(&agent()), &secret, &named));
        assert_eq!(notification.body, "work space · agent · Blocked");
        assert_eq!(notification.title, "Codex Agent needs attention");
        assert!(!notification.to_json().to_string().contains("secret-native"));
    }

    #[test]
    fn names_the_terminal_the_way_every_other_surface_names_it() {
        let named = TransitionContext {
            window_name: Some("✳ Fix tests".into()),
            ..context()
        };
        let notification = emitted(decide_agent_notification(
            Some(&agent()),
            &blocked_at(4),
            &named,
        ));
        assert!(notification.body.contains("· Fix tests ·"));
        assert_eq!(notification.title, "Fix tests needs attention");
    }

    #[test]
    fn shows_unmapped_attention_explicitly_without_requesting_a_native_action() {
        let unmapped = AgentRecord {
            attention_kind: Some(AttentionKind::Blocked),
            session_id: String::new(),
            session_name: String::new(),
            window_id: String::new(),
            window_name: String::new(),
            pane_id: String::new(),
            ..blocked_at(4)
        };
        let notification = emitted(decide_agent_notification(
            Some(&agent()),
            &unmapped,
            &context(),
        ));
        assert!(notification.body.contains("Unmapped"));
        assert!(!notification.request_action);
        assert_eq!(notification.route.pane_id, "");
        let wire = notification.to_json();
        assert_eq!(wire["requestAction"], false);
    }

    #[test]
    fn tells_the_os_to_show_what_it_decided_the_user_cannot_already_see() {
        let mut elsewhere = context();
        elsewhere.focus.app_focused = true;
        elsewhere.focus.pane_id = "%other".into();
        let notification = emitted(decide_agent_notification(
            Some(&agent()),
            &blocked_at(4),
            &elsewhere,
        ));
        assert!(notification.present_in_foreground);
        assert_eq!(notification.to_json()["presentInForeground"], true);
    }

    #[test]
    fn carries_the_full_u64_generation_into_the_route() {
        let notification = emitted(decide_agent_notification(
            Some(&agent()),
            &blocked_at(u64::MAX - 1),
            &context(),
        ));
        assert_eq!(
            notification.to_json()["route"]["attentionGeneration"],
            "18446744073709551614"
        );
    }

    #[test]
    fn bounds_a_label_at_96_bytes_on_a_character_boundary() {
        let long = "é".repeat(60);
        let label = safe_label(&long, "Agent");
        assert_eq!(label.len(), 96);
        assert_eq!(safe_label(" \n\t", "Agent"), "Agent");
    }
}
