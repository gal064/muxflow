//! What the app is allowed to say about agent status on a host. Port of
//! `hookWiring.ts`.
//!
//! When every hook on a machine belongs to another tool, no lifecycle event
//! ever reaches the daemon, and process detections prove only that a process
//! exists. `reports` is the honest answer to "can this host tell me anything at
//! all"; when it is false, the agents section says so instead of implying
//! status it does not have. Every judgement comes from the host's own
//! observation.

use super::model::{AdapterDescriptor, HookWiring};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostHookWiring {
    /// At least one adapter's events reach this daemon.
    pub reports: bool,
    /// Adapters the host says an install would act on, in display order.
    pub setup_targets: Vec<AdapterDescriptor>,
    /// Why the host could not be read, when that is why it reports nothing.
    pub unreadable_reason: Option<String>,
    /// True while the host has not answered: no snapshot, or too old to say.
    pub unknown: bool,
}

pub fn host_hook_wiring(adapters: &[AdapterDescriptor]) -> HostHookWiring {
    // An agent that is not installed has nothing to wire; treating it as a gap
    // means prompting to create configuration for a vendor the user does not use.
    let hook_adapters: Vec<_> = adapters
        .iter()
        .filter(|adapter| adapter.supports_hooks && adapter.hook_wiring != HookWiring::Absent)
        .collect();
    HostHookWiring {
        reports: hook_adapters
            .iter()
            .any(|adapter| adapter.hook_wiring == HookWiring::Wired),
        setup_targets: hook_adapters
            .iter()
            .filter(|adapter| adapter.hook_setup_recommended)
            .map(|adapter| (*adapter).clone())
            .collect(),
        unreadable_reason: hook_adapters
            .iter()
            .find(|adapter| {
                adapter.hook_wiring == HookWiring::Unavailable
                    && !adapter.hook_wiring_detail.is_empty()
            })
            .map(|adapter| adapter.hook_wiring_detail.clone()),
        // No adapters at all is "we have not been told", not "nothing is
        // wired": every disconnected and pre-snapshot render looks like that.
        unknown: hook_adapters.is_empty()
            || hook_adapters
                .iter()
                .all(|adapter| adapter.hook_wiring == HookWiring::Unspecified),
    }
}

/// The line the agents section shows when the host cannot report status.
/// Deliberately one sentence, except when the host said why.
pub fn hook_wiring_notice(wiring: &HostHookWiring) -> Option<String> {
    if wiring.reports || wiring.unknown {
        return None;
    }
    if !wiring.setup_targets.is_empty() {
        return Some("Agent status unavailable on this host — set up hooks".into());
    }
    Some(match &wiring.unreadable_reason {
        Some(reason) => format!("Agent status unavailable on this host — {reason}"),
        None => "Agent status unavailable on this host".into(),
    })
}

/// True when the app should raise the one-time prompt by itself. The question
/// is about the host: a host where one adapter reports is a host that reports.
pub fn should_prompt_for_setup(wiring: &HostHookWiring) -> bool {
    !wiring.reports && !wiring.unknown && !wiring.setup_targets.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::model::fixtures::adapter;

    fn ids(wiring: &HostHookWiring) -> Vec<&str> {
        wiring
            .setup_targets
            .iter()
            .map(|item| item.id.as_str())
            .collect()
    }

    #[test]
    fn says_nothing_is_reported_when_every_adapters_events_go_somewhere_else() {
        let wiring = host_hook_wiring(&[
            adapter("claude-code", HookWiring::NotWired),
            adapter("codex", HookWiring::NotWired),
        ]);
        assert!(!wiring.reports);
        assert_eq!(ids(&wiring), ["claude-code", "codex"]);
        assert_eq!(
            hook_wiring_notice(&wiring).as_deref(),
            Some("Agent status unavailable on this host — set up hooks")
        );
    }

    #[test]
    fn treats_a_partial_install_as_something_to_finish_not_as_wired() {
        let wiring = host_hook_wiring(&[adapter("claude-code", HookWiring::Partial)]);
        assert!(!wiring.reports);
        assert_eq!(ids(&wiring), ["claude-code"]);
    }

    #[test]
    fn stays_quiet_once_any_adapter_reports_and_offers_to_finish_the_other() {
        let wiring = host_hook_wiring(&[
            adapter("claude-code", HookWiring::Wired),
            adapter("codex", HookWiring::NotWired),
        ]);
        assert!(wiring.reports);
        assert_eq!(hook_wiring_notice(&wiring), None);
        assert_eq!(ids(&wiring), ["codex"]);
    }

    #[test]
    fn never_offers_to_write_over_a_configuration_it_could_not_read() {
        let mut unreadable = adapter("claude-code", HookWiring::Unavailable);
        unreadable.hook_wiring_detail = "parse hook JSON configuration".into();
        let wiring = host_hook_wiring(&[unreadable]);
        assert!(wiring.setup_targets.is_empty());
        assert_eq!(
            wiring.unreadable_reason.as_deref(),
            Some("parse hook JSON configuration")
        );
        assert_eq!(
            hook_wiring_notice(&wiring).as_deref(),
            Some("Agent status unavailable on this host — parse hook JSON configuration")
        );
    }

    #[test]
    fn says_nothing_at_all_before_the_host_has_answered() {
        for adapters in [
            vec![],
            vec![adapter("claude-code", HookWiring::Unspecified)],
        ] {
            let wiring = host_hook_wiring(&adapters);
            assert!(wiring.unknown);
            assert!(!wiring.reports);
            assert_eq!(hook_wiring_notice(&wiring), None);
        }
    }

    #[test]
    fn ignores_adapters_that_have_no_hooks_to_wire() {
        let mut screen_only = adapter("screen-only", HookWiring::NotWired);
        screen_only.supports_hooks = false;
        let wiring = host_hook_wiring(&[screen_only]);
        assert!(wiring.unknown);
        assert!(wiring.setup_targets.is_empty());
    }

    #[test]
    fn never_offers_to_set_up_an_agent_that_is_not_installed_on_the_host() {
        let wiring = host_hook_wiring(&[
            adapter("claude-code", HookWiring::Wired),
            adapter("codex", HookWiring::Absent),
        ]);
        assert!(wiring.setup_targets.is_empty());
        assert!(wiring.reports);
        assert!(!should_prompt_for_setup(&wiring));
        let nothing_here = host_hook_wiring(&[
            adapter("claude-code", HookWiring::Absent),
            adapter("codex", HookWiring::Absent),
        ]);
        assert!(nothing_here.unknown);
        assert_eq!(hook_wiring_notice(&nothing_here), None);
    }

    #[test]
    fn only_raises_the_prompt_by_itself_when_the_whole_host_reports_nothing() {
        let partly = host_hook_wiring(&[
            adapter("claude-code", HookWiring::Wired),
            adapter("codex", HookWiring::NotWired),
        ]);
        assert!(!should_prompt_for_setup(&partly));
        assert_eq!(ids(&partly), ["codex"]);
        let silent = host_hook_wiring(&[
            adapter("claude-code", HookWiring::NotWired),
            adapter("codex", HookWiring::NotWired),
        ]);
        assert!(should_prompt_for_setup(&silent));
        assert!(!should_prompt_for_setup(&host_hook_wiring(&[])));
        assert!(!should_prompt_for_setup(&host_hook_wiring(&[adapter(
            "claude-code",
            HookWiring::Unspecified
        )])));
    }
}
