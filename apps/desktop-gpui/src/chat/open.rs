//! Where a click in a chat leads: a file edit to its Git diff tab, a link to
//! its file or to the browser.

use gpui_kit::App;
use tmux_control::Pane;

use crate::{editor, git};

use super::links;

/// `path` as the provider named it, made absolute against the pane's
/// directory. A path under `~` has no absolute form here.
fn absolute(pane: &Pane, path: &str) -> Option<String> {
    if path.starts_with('/') {
        return Some(path.to_owned());
    }
    if path.starts_with('~') || pane.current_path.is_empty() {
        return None;
    }
    let path = path.strip_prefix("./").unwrap_or(path);
    Some(format!(
        "{}/{path}",
        pane.current_path.trim_end_matches('/')
    ))
}

/// Opens the file an edit row names in the Git diff tab. A file Git shows no
/// change to, or one outside the repository on screen, opens as a file.
pub(super) fn changes(pane: &Pane, path: &str, cx: &mut App) {
    if !diff(pane, path, cx) {
        editor::open_terminal_path(&pane.id, &links::resolvable(path), None, cx);
    }
}

fn diff(pane: &Pane, path: &str, cx: &mut App) -> bool {
    let (Some(status), Some(absolute)) = (git::active_status(cx), absolute(pane, path)) else {
        return false;
    };
    let changed = status
        .entries
        .iter()
        .any(|entry| entry.absolute_path.as_deref() == Some(absolute.as_str()));
    if changed {
        git::open_file_diff(&absolute, cx);
    }
    changed
}

/// Follows a link in an agent's text. A file opens resolved for `pane`; any
/// web address opens in the browser: the click is the user's deliberate gesture.
pub(super) fn link(pane_id: &str, href: &str, cx: &mut App) {
    if let Some(path) = links::file_target(href) {
        editor::open_terminal_path(pane_id, &links::resolvable(&path), None, cx);
        return;
    }
    let url = href.to_owned();
    let starts = |prefix: &str| {
        url.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(prefix))
    };
    // Only what the browser is handed elsewhere in the app.
    if !(starts("https://") || starts("http://")) {
        editor::report(format!("This link cannot be opened: {url}"), cx);
        return;
    }
    if let Err(error) = desktop_core::external_links::open_external_link(url, true) {
        editor::report(error, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(cwd: &str) -> Pane {
        Pane {
            id: "%1".into(),
            session_id: "$1".into(),
            window_id: "@1".into(),
            index: 0,
            active: true,
            width: 80,
            height: 24,
            left: 0,
            top: 0,
            current_path: cwd.into(),
            current_command: "claude".into(),
            pane_pid: 1,
            start_command: String::new(),
            title: String::new(),
            chat_adapter_id: "claude".into(),
        }
    }

    #[test]
    fn an_edited_path_is_made_absolute_against_the_panes_directory() {
        let pane = pane("/work/repo/");
        assert_eq!(absolute(&pane, "/etc/hosts").as_deref(), Some("/etc/hosts"));
        assert_eq!(
            absolute(&pane, "src/lib.rs").as_deref(),
            Some("/work/repo/src/lib.rs")
        );
        assert_eq!(
            absolute(&pane, "./src/lib.rs").as_deref(),
            Some("/work/repo/src/lib.rs")
        );
        assert_eq!(absolute(&pane, "~/notes.md"), None);
        assert_eq!(absolute(&self::pane(""), "src/lib.rs"), None);
    }
}
