//! A host rejection turned into a sentence, with the diagnostic kept. Port of
//! `ui/errorSummary.ts`.
//!
//! The host fails closed and says so precisely, `file_write_preflight_rejected:
//! Permission denied (os error 13)`, which helps whoever debugs it and tells a
//! person nothing about what to do next. So a surface shows a sentence first and
//! the diagnostic behind a disclosure, never dropped. It is a fallback chain, not
//! a table with an "unknown" arm: an unmapped rejection still gets a readable
//! first line.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Summary {
    /// One sentence a person can act on, never empty.
    pub summary: String,
    /// The original text, only when it says more than the summary.
    pub detail: Option<String>,
}

type Table = LazyLock<Vec<(Regex, &'static str)>>;

fn table(rows: &[(&str, &'static str)]) -> Vec<(Regex, &'static str)> {
    rows.iter()
        .map(|(pattern, summary)| (Regex::new(pattern).expect("valid pattern"), *summary))
        .collect()
}

/// The `<domain>_<verb>` codes the host prefixes its refusals with that reach a
/// surface; any other code falls through to the reason or the text itself.
static CODE_SUMMARY: Table = LazyLock::new(|| {
    table(&[
        (
            r"^file_mutation_(rejected|task_failed)$",
            "The host refused that file change.",
        ),
        (
            r"^file_read_rejected$",
            "The host refused to read that file.",
        ),
        (
            r"^file_write_.*_rejected$",
            "The host refused to save that file.",
        ),
        (
            r"^(directory_rejected|unwatch_rejected)$",
            "The host refused that folder request.",
        ),
        (r"^download_.*_rejected$", "The host refused that download."),
        (r"^upload_.*_rejected$", "The host refused that upload."),
        (
            r"^upload_outcome_unavailable$",
            "The host could not confirm how that upload ended.",
        ),
        (r"^git_rejected$", "Git refused that change."),
        (r"^tmux_action_rejected$", "tmux refused that action."),
        (
            r"^tmux_unavailable$",
            "The tmux server is not reachable right now.",
        ),
        (
            r"^terminal_.*_rejected$",
            "The terminal connection refused that request.",
        ),
        (
            r"^mutation_rejected$",
            "The connection to the host is not accepting changes right now.",
        ),
        (
            r"^connection_unavailable$",
            "The connection to the host is still reconnecting.",
        ),
        (
            r"^confirmation_required$",
            "That change needs an explicit confirmation first.",
        ),
        (r"^cancelled$", "That request was cancelled."),
    ])
});

/// Codes more specific than anything their reason can say, read before the
/// reasons: `Permission denied (publickey)` is Git authentication, not the
/// filesystem answer that phrase otherwise gets.
static CODE_OVERRIDE_SUMMARY: Table = LazyLock::new(|| {
    table(&[
        (
            r"^git_auth_failed$",
            "Push needs authentication that Muxflow cannot provide interactively; run git push in a terminal once, then retry.",
        ),
        (
            r"^git_push_rejected$",
            "The remote rejected the push (non-fast-forward?). Pull or rebase first.",
        ),
        (
            r"^git_no_upstream$",
            "This branch has no upstream yet; run git push -u in a terminal once, then retry.",
        ),
    ])
});

/// What a reason means in practice, read before the code because it says
/// more about what to do.
static REASON_SUMMARY: Table = LazyLock::new(|| {
    table(&[
        (
            r"(?i)tmux executable was not found|MUXFLOW_TMUX_PATH",
            "Muxflow could not find tmux on this host. Install tmux or set MUXFLOW_TMUX_PATH to its absolute path.",
        ),
        (
            r"(?i)file name too long|os error 63",
            "That name is longer than this filesystem allows. Use a shorter one.",
        ),
        (
            r"(?i)mutation parent changed|no-follow directory|unsafe component|outside the active root|escapes the active root|leaf changed or is unsafe",
            "The name has to be a plain file name inside the folder you chose — no “/”, no “..”, and no path through a symlink.",
        ),
        (
            r"(?i)permission denied|os error 13|read-only file system|os error 30",
            "The host account is not allowed to write there.",
        ),
        (
            r"(?i)no space left|os error 28",
            "The host has run out of disk space.",
        ),
        (
            r"(?i)file exists|already exists|os error 17",
            "Something is already at that path. Allow overwrite, or choose another name.",
        ),
        // Configuration, not a file just clicked: Settings helps, a refresh
        // does not.
        (
            r"(?i)workspace start directory",
            "The start directory configured for new workspaces is not a directory on this host. Change it under Settings → Workspace.",
        ),
        (
            r"(?i)no such file or directory|os error 2|does not exist|is no longer available",
            "That path is no longer on the host. Refresh and try again.",
        ),
        (
            r"(?i)not a directory|os error 20",
            "Part of that path is a file, not a folder.",
        ),
        (
            r"(?i)directory not empty|os error 66",
            "That folder still has files in it; confirm the non-empty replacement to continue.",
        ),
        (
            r"(?i)nul byte",
            "That name contains a character the filesystem cannot store.",
        ),
        (
            r"(?i)timed out|timeout",
            "The host did not answer in time. Check the connection and try again.",
        ),
        (
            r"(?i)disconnected or reconciling",
            "The connection to the host is still reconnecting.",
        ),
        (
            r"(?i)server is unavailable|connection (closed|lost)|not connected",
            "The connection to the host is down. Reconnect and try again.",
        ),
    ])
});

static CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([a-z][a-z0-9_]*):\s*([\s\S]+)$").expect("valid pattern"));
static STRUCTURED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"_(rejected|unavailable|failed)$").expect("valid pattern"));
static WRAPPER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:Error|TypeError|AggregateError)\s*:\s*").expect("valid pattern")
});
static SENTENCE_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[.!?](\s|$)").expect("valid pattern"));

/// How much of an unmapped message reads as a summary before it is a wall.
const SUMMARY_LIMIT: usize = 140;

pub fn summarize(raw: &str) -> Summary {
    let text = raw.trim();
    if text.is_empty() {
        return Summary {
            summary: "Something went wrong.".into(),
            detail: None,
        };
    }
    let mut stripped = text;
    while let Some(found) = WRAPPER.find(stripped) {
        stripped = &stripped[found.end()..];
    }
    let split = CODE.captures(stripped);
    let code = split.as_ref().map(|split| split.get(1).unwrap().as_str());
    let reason = split
        .as_ref()
        .map(|split| split.get(2).unwrap().as_str().trim())
        .unwrap_or(stripped);
    // Only a structured host diagnostic is rewritten. The same channel carries
    // sentences the app wrote itself, which name the pane or file that failed.
    let structured = code.is_some_and(|code| {
        STRUCTURED.is_match(code)
            || matched(&CODE_SUMMARY, code).is_some()
            || matched(&CODE_OVERRIDE_SUMMARY, code).is_some()
    });
    let summary = match code.filter(|_| structured) {
        Some(code) => matched(&CODE_OVERRIDE_SUMMARY, code)
            .or_else(|| matched(&REASON_SUMMARY, reason))
            .or_else(|| matched(&CODE_SUMMARY, code))
            .map(str::to_owned)
            .unwrap_or_else(|| first_sentence(reason)),
        None => first_sentence(stripped),
    };
    let detail = (summary != text).then(|| text.to_owned());
    Summary { summary, detail }
}

fn matched(table: &[(Regex, &'static str)], value: &str) -> Option<&'static str> {
    table
        .iter()
        .find(|(pattern, _)| pattern.is_match(value))
        .map(|(_, summary)| *summary)
}

/// The first line, ended at a sentence boundary when one is in range. Lengths
/// count characters, as the original's string lengths did for this text.
fn first_sentence(text: &str) -> String {
    let line = text.split('\n').next().unwrap_or_default().trim();
    let chars = |s: &str| s.chars().count();
    let candidate = match SENTENCE_END.find(line) {
        Some(stop) if chars(&line[..stop.start()]) < SUMMARY_LIMIT => &line[..stop.start() + 1],
        _ => line,
    };
    if chars(candidate) <= SUMMARY_LIMIT {
        return candidate.to_owned();
    }
    let at = |n: usize| {
        candidate
            .char_indices()
            .nth(n)
            .map_or(candidate.len(), |(at, _)| at)
    };
    // The last space at or before the limit, as `lastIndexOf(" ", limit)`.
    let limit = at(SUMMARY_LIMIT);
    let cut = candidate[..at(SUMMARY_LIMIT + 1)]
        .rfind(' ')
        .filter(|cut| chars(&candidate[..*cut]) > 40)
        .unwrap_or(limit);
    format!("{}…", candidate[..cut].trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_the_recorded_rejections_with_something_a_person_can_act_on() {
        let raw = "file_mutation_rejected: mutation parent changed or is not a no-follow directory";
        let slash = summarize(raw);
        assert!(slash.summary.contains("plain file name"));
        assert_eq!(slash.detail.as_deref(), Some(raw));
        let long = summarize("file_mutation_rejected: File name too long (os error 63)");
        assert_eq!(
            long.summary,
            "That name is longer than this filesystem allows. Use a shorter one."
        );
        assert!(long.detail.unwrap().contains("os error 63"));
    }

    #[test]
    fn explains_a_refused_save_and_a_settling_connection() {
        assert_eq!(
            summarize("file_write_preflight_rejected: Permission denied (os error 13)").summary,
            "The host account is not allowed to write there."
        );
        assert_eq!(
            summarize("file_write_commit_rejected: stale generation").summary,
            "The host refused to save that file."
        );
        assert_eq!(
            summarize("connection_unavailable: host connection is disconnected or reconciling")
                .summary,
            "The connection to the host is still reconnecting."
        );
    }

    #[test]
    fn never_loses_the_diagnostic_it_summarizes() {
        let raw = "git_rejected: fatal: pathspec 'x' did not match any files";
        assert_eq!(summarize(raw).detail.as_deref(), Some(raw));
    }

    #[test]
    fn falls_back_to_the_code_family_when_the_reason_is_unrecognized() {
        assert_eq!(
            summarize("tmux_action_rejected: window is in an unexpected layout").summary,
            "tmux refused that action."
        );
        assert_eq!(
            summarize("upload_preflight_rejected: quota policy 7").summary,
            "The host refused that upload."
        );
    }

    #[test]
    fn distinguishes_a_missing_tmux_executable_from_an_absent_server() {
        let missing = summarize(
            "tmux_unavailable: tmux executable was not found; install tmux or set MUXFLOW_TMUX_PATH to an absolute executable path",
        );
        assert!(missing.summary.contains("Install tmux"));
        assert!(missing.summary.contains("MUXFLOW_TMUX_PATH"));
        assert!(
            missing
                .detail
                .unwrap()
                .contains("tmux executable was not found")
        );
        assert_eq!(
            summarize("tmux_unavailable: tmux server is unavailable").summary,
            "The connection to the host is down. Reconnect and try again."
        );
    }

    #[test]
    fn falls_back_to_the_message_itself_and_never_to_an_empty_line() {
        let moon = summarize("The moon is in the wrong phase.");
        assert_eq!(moon.summary, "The moon is in the wrong phase.");
        assert_eq!(moon.detail, None);
        assert_eq!(summarize("   ").summary, "Something went wrong.");
    }

    #[test]
    fn leaves_a_sentence_the_app_wrote_itself_as_written() {
        let own = "Could not mark %117 hidden: timed out";
        assert_eq!(
            summarize(own),
            Summary {
                summary: own.into(),
                detail: None
            }
        );
        let agent = "Agent Codex has no exact pane match; navigation is unavailable.";
        assert_eq!(summarize(agent).summary, agent);
        assert_eq!(
            summarize("terminal_visibility_rejected: timed out").summary,
            "The host did not answer in time. Check the connection and try again."
        );
    }

    #[test]
    fn strips_wrappers_and_keeps_only_the_first_sentence_in_front() {
        let result = summarize(
            "Error: Error: something odd happened. And then more detail nobody needs first.",
        );
        assert_eq!(result.summary, "something odd happened.");
        assert!(
            result
                .detail
                .unwrap()
                .contains("more detail nobody needs first")
        );
    }

    #[test]
    fn bounds_a_single_sentence_wall_of_text_at_a_word_boundary() {
        let wall = format!("x {}end", "word ".repeat(80));
        let result = summarize(&wall);
        assert!(result.summary.chars().count() <= 141);
        assert!(result.summary.ends_with('…'));
        assert!(!result.summary.contains("wor…"));
        assert_eq!(result.detail.as_deref(), Some(wall.trim()));
    }

    #[test]
    fn answers_a_push_refusal_with_what_to_do() {
        let auth = summarize("git_auth_failed: git@github.com: Permission denied (publickey).");
        assert!(auth.summary.contains("run git push in a terminal once"));
        assert!(auth.detail.unwrap().contains("publickey"));
        let rejected =
            summarize("git_push_rejected: ! [rejected] master -> master (non-fast-forward)");
        assert_eq!(
            rejected.summary,
            "The remote rejected the push (non-fast-forward?). Pull or rebase first."
        );
        let missing = summarize(
            "git_no_upstream: no upstream branch is configured for the current branch; run `git push -u` in a terminal once",
        );
        assert_eq!(
            missing.summary,
            "This branch has no upstream yet; run git push -u in a terminal once, then retry."
        );
        assert!(missing.detail.unwrap().contains("git push -u"));
    }

    #[test]
    fn keeps_a_multi_line_diagnostics_first_line_in_front() {
        let raw = "commit failed\npre-commit hook output\nline 2";
        let result = summarize(raw);
        assert_eq!(result.summary, "commit failed");
        assert_eq!(result.detail.as_deref(), Some(raw));
    }

    #[test]
    fn points_a_refused_start_directory_at_settings() {
        let text = summarize(
            "tmux_action_rejected: workspace start directory /nope does not exist or is not a directory",
        );
        assert!(text.summary.contains("Settings"));
        assert!(!text.summary.contains("Refresh"));
    }
}
