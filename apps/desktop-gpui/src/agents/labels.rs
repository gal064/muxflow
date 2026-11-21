//! Agent labels. Port of `@muxflow/client-core`'s `agentLabels.ts`; the mobile
//! app keeps its own TypeScript copy, so the cases below are ported as Rust
//! tests rather than shared fixtures.

use std::sync::LazyLock;

use regex::Regex;

use super::model::{AdapterDescriptor, AgentRecord};

static UUID_LIKE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}|[0-9a-f]{24,})$")
        .expect("valid pattern")
});
static GENERIC_TAB_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:agent|codex|claude(?:[- ]code)?|pi)$").expect("valid pattern")
});
/// Claude Code and Codex prefix their tmux window titles with a status marker
/// (Claude's asterisk frames, Codex's braille spinner, quarter circles, check
/// and cross marks, and Codex's `[ . ]` / `[ ! ]` action-required frames),
/// sometimes with either variation selector. The app draws its own state
/// indicators, so the prefix is a second ticker saying the same thing, and the
/// UI font lacks several of these glyphs. pi starts its title with its own name
/// instead, which the adapter mark already says. Only these known prefixes are
/// stripped; a title someone deliberately starts with an emoji keeps it.
static AGENT_STATUS_GLYPHS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(?:(?:\[ [.!] \]|π -|[\x{00B7}\x{2713}-\x{2718}\x{2722}\x{2733}\x{2736}\x{273B}\x{273D}\x{25D0}-\x{25D3}\x{2800}-\x{28FF}][\x{FE0E}\x{FE0F}]?)\s*)+",
    )
    .expect("valid pattern")
});

pub fn without_status_glyphs(title: &str) -> &str {
    let stripped = match AGENT_STATUS_GLYPHS.find(title) {
        Some(prefix) => &title[prefix.end()..],
        None => title,
    };
    stripped.trim()
}

/// A tmux window title with any leading agent status ticker removed. A title
/// that is nothing but the ticker keeps it: a blank tab is worse than a frame.
pub fn strip_agent_status_glyphs(title: &str) -> &str {
    match without_status_glyphs(title) {
        "" => title.trim(),
        name => name,
    }
}

/// The agent pane's own title, then the live tmux tab name, unless either is
/// only a generic or machine identifier. The pane comes first because a window
/// is named after one pane: agents sharing a split would otherwise all read the
/// same name.
pub fn agent_session_label(agent: &AgentRecord, adapters: &[AdapterDescriptor]) -> String {
    for title in [&agent.pane_title, &agent.window_name] {
        let name = without_status_glyphs(title);
        if !name.is_empty() && !UUID_LIKE.is_match(name) && !GENERIC_TAB_NAME.is_match(name) {
            return name.to_owned();
        }
    }
    let assigned = agent.display_name.trim();
    if !assigned.is_empty() && !UUID_LIKE.is_match(assigned) {
        return assigned.to_owned();
    }
    adapters
        .iter()
        .find(|adapter| adapter.id == agent.adapter_id && !adapter.display_name.is_empty())
        .map(|adapter| adapter.display_name.clone())
        .unwrap_or_else(|| adapter_fallback(&agent.adapter_id).to_owned())
}

pub fn adapter_fallback(adapter_id: &str) -> &'static str {
    match adapter_id {
        "codex" => "Codex",
        "claude-code" => "Claude",
        "pi" => "Pi",
        _ => "Agent",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::model::HookWiring;
    use crate::agents::model::fixtures::agent;

    fn named(window_name: &str, pane_title: &str, display_name: &str) -> AgentRecord {
        AgentRecord {
            window_name: window_name.into(),
            pane_title: pane_title.into(),
            display_name: display_name.into(),
            ..agent()
        }
    }

    #[test]
    fn removes_each_agent_status_ticker_the_clis_actually_emit() {
        for glyph in [
            "·", "✢", "✳", "✶", "✻", "✽", "◐", "◓", "◑", "◒", "⠦", "⠋", "⣷", "✓", "✗", "[ . ]",
            "[ ! ]",
        ] {
            assert_eq!(
                strip_agent_status_glyphs(&format!("{glyph} Fix tests")),
                "Fix tests",
                "{glyph}"
            );
        }
        assert_eq!(
            strip_agent_status_glyphs("✳\u{FE0F} Fix tests"),
            "Fix tests"
        );
        assert_eq!(
            strip_agent_status_glyphs("✳\u{FE0E} Fix tests"),
            "Fix tests"
        );
        assert_eq!(strip_agent_status_glyphs("✳ ✶ Fix tests"), "Fix tests");
        assert_eq!(
            strip_agent_status_glyphs("✓ Update customize.py for oma… · gpt-5.6-sol · ~/dev"),
            "Update customize.py for oma… · gpt-5.6-sol · ~/dev"
        );
        for marker in ["[ . ]", "[ ! ]"] {
            assert_eq!(
                strip_agent_status_glyphs(&format!(
                    "{marker} Action Required | 01a03cb0-7122-7410-a867-f5c95d64faa5"
                )),
                "Action Required | 01a03cb0-7122-7410-a867-f5c95d64faa5"
            );
        }
        assert_eq!(
            strip_agent_status_glyphs("π - aaa - muxflow"),
            "aaa - muxflow"
        );
        assert_eq!(strip_agent_status_glyphs("π - muxflow"), "muxflow");
    }

    #[test]
    fn leaves_deliberate_names_alone() {
        assert_eq!(strip_agent_status_glyphs("🚀 deploy"), "🚀 deploy");
        assert_eq!(strip_agent_status_glyphs("Fix tests"), "Fix tests");
        assert_eq!(
            strip_agent_status_glyphs("release ✳ notes"),
            "release ✳ notes"
        );
        assert_eq!(
            strip_agent_status_glyphs("π-calculus notes"),
            "π-calculus notes"
        );
        assert_eq!(strip_agent_status_glyphs("✳"), "✳");
    }

    #[test]
    fn labels_an_agent_by_its_tab_name_with_the_ticker_removed() {
        assert_eq!(
            agent_session_label(&named("✳ Plan rollout", "", "Codex one"), &[]),
            "Plan rollout"
        );
        assert_eq!(
            agent_session_label(&named("⠦ codex", "", "Nightly triage"), &[]),
            "Nightly triage"
        );
    }

    #[test]
    fn labels_an_agent_by_its_own_pane_title_ahead_of_the_tab_it_shares() {
        assert_eq!(
            agent_session_label(&named("✳ Plan rollout", "✳ Ship release", ""), &[]),
            "Ship release"
        );
        assert_eq!(
            agent_session_label(&named("claude", "⠋ Ship release", ""), &[]),
            "Ship release"
        );
        for pane_title in ["", "⠋", "✳ Claude Code", "codex"] {
            assert_eq!(
                agent_session_label(&named("✳ Plan rollout", pane_title, ""), &[]),
                "Plan rollout",
                "{pane_title}"
            );
        }
    }

    #[test]
    fn treats_claudes_own_pre_topic_title_as_generic() {
        assert_eq!(
            agent_session_label(&named("✳ Claude Code", "", "Nightly triage"), &[]),
            "Nightly triage"
        );
    }

    #[test]
    fn falls_through_a_title_that_is_only_a_ticker_frame() {
        assert_eq!(
            agent_session_label(&named("⠋", "", "Nightly triage"), &[]),
            "Nightly triage"
        );
        let mut codex = crate::agents::model::fixtures::adapter("codex", HookWiring::Wired);
        codex.display_name = "Codex CLI".into();
        assert_eq!(
            agent_session_label(&named("⠋", "", ""), &[codex]),
            "Codex CLI"
        );
        assert_eq!(agent_session_label(&named("⠋", "", ""), &[]), "Codex");
    }
}
