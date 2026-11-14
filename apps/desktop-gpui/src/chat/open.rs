//! Where a click in a chat leads: a file edit to its Git diff tab, a link to
//! its file or, once confirmed, to the browser.

use std::{cell::RefCell, rc::Rc};

use gpui_kit::{App, Window};
use tmux_control::Pane;

use crate::{
    agents::view::ActiveRoot,
    commands::dialogs::{Confirmation, open_confirmation},
    editor,
    git::{
        self,
        model::{ChangeKind, DiffTabPlace, StatusEntry, Target, open_git_diff_tab},
    },
};

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

/// The side of a changed file a diff tab shows first: what is not staged yet,
/// when there is any.
fn side(entry: &StatusEntry) -> Target {
    if entry.untracked || entry.conflicted || entry.worktree_kind != ChangeKind::None {
        Target::Unstaged
    } else {
        Target::Staged
    }
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
    let Some(entry) = status
        .entries
        .iter()
        .find(|entry| entry.absolute_path.as_deref() == Some(absolute.as_str()))
        .cloned()
    else {
        return false;
    };
    let Some(root) = cx
        .try_global::<ActiveRoot>()
        .map(|root| (root.path.clone(), root.token.clone()))
    else {
        return false;
    };
    let Some(workspaces) = crate::workspaces::current(cx) else {
        return false;
    };
    let view = workspaces.read(cx).view(cx);
    let (Some(session), Some(server)) = (view.session.clone(), view.server.clone()) else {
        return false;
    };
    // The diff tab belongs to the workspace on screen: an edit made in a
    // pane of another one opens as a file there instead.
    if session.id != pane.session_id {
        return false;
    }
    let host = view.host.clone();
    let opened = Rc::new(RefCell::new(None));
    workspaces.update(cx, |workspaces, cx| {
        let opened = opened.clone();
        let session = session.clone();
        workspaces.update_app_state(
            move |state| {
                let place = DiffTabPlace {
                    host: &host,
                    server: &server,
                    session_id: &session.id,
                    session_name: &session.name,
                    root_path: &root.0,
                    root_token: &root.1,
                };
                let id =
                    open_git_diff_tab(state, &place, &entry, side(&entry), &status, true, || {
                        uuid::Uuid::new_v4().to_string()
                    });
                *opened.borrow_mut() = Some(id);
                true
            },
            cx,
        );
    });
    let Some(id) = opened.borrow_mut().take() else {
        return false;
    };
    workspaces.update(cx, |workspaces, cx| {
        workspaces.select_app_tab(&session.id, Some(&id), cx);
    });
    true
}

/// Follows a link in an agent's text. A file opens resolved for `pane`; any
/// other address is shown first and opened in the browser only once confirmed.
pub(super) fn link(pane_id: &str, href: &str, window: &mut Window, cx: &mut App) {
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
    open_confirmation(
        Confirmation {
            title: "Open external link?".into(),
            detail: url.clone().into(),
            confirm_label: "Open link".into(),
            destructive: false,
            on_confirm: Rc::new(move |_, cx| {
                if let Err(error) =
                    desktop_core::external_links::open_external_link(url.clone(), true)
                {
                    editor::report(error, cx);
                }
            }),
        },
        window,
        cx,
    );
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
