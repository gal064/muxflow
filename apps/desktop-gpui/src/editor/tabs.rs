//! The file-tab edits saved in `app_state`: opening a file, VS Code's preview
//! slot, and a Markdown tab's view mode. Port of `openFileTab` and
//! `setMarkdownViewMode` in `features/shell/model.ts`.

use desktop_core::app_state::{AppTabKind, AppTabRecord, AppTabViewMode, PersistedAppState};
use tmux_control::Session;

use crate::workspaces::model::select_app_tab;

/// The name a file tab shows: the last part of its path.
pub fn file_tab_title(resource: &str) -> String {
    resource
        .split('/')
        .rfind(|part| !part.is_empty())
        .unwrap_or(resource)
        .to_owned()
}

/// How a file is being opened.
#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    /// A single click: the workspace's one disposable tab.
    pub preview: bool,
    /// A terminal link carries a fresh root capability for an open tab too.
    pub refresh_root: bool,
    /// The starting mode of a new Markdown tab.
    pub view_mode: Option<AppTabViewMode>,
}

/// VS Code's open rule, and the invariant behind it: a workspace has at most
/// one preview tab.
///
/// A preview reuses the preview slot in place, same id and position, so
/// browsing a directory leaves one tab behind. A pinned open, or the first
/// edit, promotes a tab and never demotes it. Returns the tab's id, selected.
#[allow(clippy::too_many_arguments)]
pub fn open_file_tab(
    state: &mut PersistedAppState,
    host: &str,
    server: &str,
    session: &Session,
    resource: &str,
    kind: AppTabKind,
    root: (&str, &str),
    options: OpenOptions,
) -> String {
    let in_workspace = |tab: &AppTabRecord| {
        tab.host_profile_id == host && tab.server_identity == server && tab.session_id == session.id
    };
    let (root_path, root_token) = root;
    if let Some(existing) = state
        .app_tabs
        .iter_mut()
        .find(|tab| in_workspace(tab) && tab.resource == resource)
    {
        if !options.preview && existing.preview == Some(true) {
            existing.preview = None;
        }
        if options.refresh_root {
            existing.root_path = Some(root_path.to_owned());
            existing.root_token = Some(root_token.to_owned());
        }
        let id = existing.id.clone();
        select_app_tab(state, host, server, session, Some(&id));
        return id;
    }
    // The configured default, read here and only here: an existing tab keeps
    // the mode the user put it in.
    let view_mode =
        (kind == AppTabKind::Markdown).then(|| options.view_mode.unwrap_or(AppTabViewMode::Split));
    let describe = |tab: &mut AppTabRecord| {
        tab.kind = kind;
        tab.resource = resource.to_owned();
        tab.title = file_tab_title(resource);
        tab.root_path = Some(root_path.to_owned());
        tab.root_token = Some(root_token.to_owned());
        tab.view_mode = view_mode;
    };
    // The slot is reused, not the record: only the tab's identity and place
    // survive. A preview Git diff is not this slot, since rewriting one into
    // a file would lose its repository, path and side.
    let reusable = options.preview.then(|| {
        state.app_tabs.iter_mut().find(|tab| {
            in_workspace(tab) && tab.preview == Some(true) && tab.kind != AppTabKind::GitDiff
        })
    });
    if let Some(Some(slot)) = reusable {
        let mut tab = AppTabRecord {
            id: slot.id.clone(),
            host_profile_id: slot.host_profile_id.clone(),
            server_identity: slot.server_identity.clone(),
            session_id: slot.session_id.clone(),
            session_name: slot.session_name.clone(),
            order: slot.order,
            preview: Some(true),
            ..blank()
        };
        describe(&mut tab);
        *slot = tab;
        let id = slot.id.clone();
        select_app_tab(state, host, server, session, Some(&id));
        return id;
    }
    let mut tab = AppTabRecord {
        id: uuid::Uuid::new_v4().to_string(),
        host_profile_id: host.to_owned(),
        server_identity: server.to_owned(),
        session_id: session.id.clone(),
        session_name: session.name.clone(),
        order: state
            .app_tabs
            .iter()
            .filter(|tab| in_workspace(tab))
            .count() as u32,
        preview: options.preview.then_some(true),
        ..blank()
    };
    describe(&mut tab);
    let id = tab.id.clone();
    state.app_tabs.push(tab);
    select_app_tab(state, host, server, session, Some(&id));
    id
}

fn blank() -> AppTabRecord {
    AppTabRecord {
        id: String::new(),
        host_profile_id: String::new(),
        server_identity: String::new(),
        session_id: String::new(),
        session_name: String::new(),
        kind: AppTabKind::File,
        resource: String::new(),
        root_path: None,
        root_token: None,
        preview: None,
        view_mode: None,
        git_repository_id: None,
        git_path: None,
        git_original_path: None,
        git_target: None,
        git_status_generation: None,
        git_source_generation: None,
        title: String::new(),
        order: 0,
    }
}

pub fn set_markdown_view_mode(
    state: &mut PersistedAppState,
    host: &str,
    tab_id: &str,
    mode: AppTabViewMode,
) -> bool {
    match state
        .app_tabs
        .iter_mut()
        .find(|tab| tab.host_profile_id == host && tab.id == tab_id)
    {
        Some(tab) if tab.view_mode != Some(mode) => {
            tab.view_mode = Some(mode);
            true
        }
        _ => false,
    }
}

/// A Markdown tab: `.md` and `.mdown`, as the terminal opens them.
pub fn kind_for_path(path: &str) -> AppTabKind {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".md") || lower.ends_with(".mdown") {
        AppTabKind::Markdown
    } else {
        AppTabKind::File
    }
}

#[cfg(test)]
mod tests {
    use desktop_core::app_state::{DockSide, GitDiffTarget};

    use super::*;
    use crate::workspaces::model::{dock_app_tab, fixtures};

    fn empty() -> PersistedAppState {
        let mut state = fixtures::state();
        state.app_tabs.clear();
        state
    }

    fn sessions() -> [Session; 2] {
        [
            fixtures::session("$0", "zero", 0),
            fixtures::session("$1", "one", 1),
        ]
    }

    fn ui(state: &PersistedAppState) -> &desktop_core::app_state::WorkspaceUiRecord {
        state
            .workspace_ui
            .iter()
            .find(|record| record.session_id == "$1")
            .unwrap()
    }

    fn open(state: &mut PersistedAppState, resource: &str, preview: bool) -> String {
        open_file_tab(
            state,
            "local",
            "server-a",
            &sessions()[1],
            resource,
            AppTabKind::File,
            ("/repo", "root-token"),
            OpenOptions {
                preview,
                ..OpenOptions::default()
            },
        )
    }

    #[test]
    fn opens_files_into_the_side_column_while_one_is_docked_browsing_previews_in_place() {
        let mut state = empty();
        let pinned = open(&mut state, "/repo/a.ts", false);
        dock_app_tab(
            &mut state,
            "local",
            "server-a",
            &sessions()[1],
            &pinned,
            DockSide::Right,
        );

        // A single click opens a preview beside the terminal; the docked file stays a tab.
        let preview = open(&mut state, "/repo/b.ts", true);
        assert_ne!(preview, pinned);
        assert_eq!(
            ui(&state).docked_app_tab_id.as_deref(),
            Some(preview.as_str())
        );
        assert_eq!(ui(&state).selected_app_tab_id, None);
        let resources = |state: &PersistedAppState| {
            state
                .app_tabs
                .iter()
                .map(|tab| tab.resource.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(resources(&state), ["/repo/a.ts", "/repo/b.ts"]);

        // The next single click rewrites that preview in place, still docked.
        assert_eq!(open(&mut state, "/repo/c.ts", true), preview);
        assert_eq!(
            ui(&state).docked_app_tab_id.as_deref(),
            Some(preview.as_str())
        );
        assert_eq!(ui(&state).dock_side, Some(DockSide::Right));
        assert_eq!(resources(&state), ["/repo/a.ts", "/repo/c.ts"]);

        // Reopening the first file docks it again.
        open(&mut state, "/repo/a.ts", false);
        assert_eq!(
            ui(&state).docked_app_tab_id.as_deref(),
            Some(pinned.as_str())
        );
    }

    #[test]
    fn a_pinned_open_promotes_a_preview_and_a_preview_open_never_demotes() {
        let mut state = empty();
        let id = open(&mut state, "/repo/a.ts", true);
        assert_eq!(state.app_tabs[0].preview, Some(true));
        assert_eq!(open(&mut state, "/repo/a.ts", false), id);
        assert_eq!(state.app_tabs[0].preview, None);
        open(&mut state, "/repo/a.ts", true);
        assert_eq!(state.app_tabs[0].preview, None);
        assert_eq!(state.app_tabs.len(), 1);
    }

    #[test]
    fn a_file_preview_does_not_take_a_preview_diffs_slot() {
        let mut state = empty();
        let mut diff = fixtures::tab("diff", "$1", 0);
        diff.kind = AppTabKind::GitDiff;
        diff.preview = Some(true);
        diff.git_target = Some(GitDiffTarget::Unstaged);
        state.app_tabs.push(diff);
        open(&mut state, "/repo/a.ts", true);
        assert_eq!(state.app_tabs.len(), 2);
        assert_eq!(state.app_tabs[0].kind, AppTabKind::GitDiff);
        assert_eq!(state.app_tabs[0].preview, Some(true));
    }

    #[test]
    fn opens_one_root_scoped_file_tab_and_preserves_it_across_root_changes() {
        let mut state = empty();
        let open_readme = |state: &mut PersistedAppState, root: (&str, &str)| {
            open_file_tab(
                state,
                "local",
                "server-a",
                &sessions()[1],
                "/repo/README.md",
                AppTabKind::Markdown,
                root,
                OpenOptions::default(),
            )
        };
        let id = open_readme(&mut state, ("/repo", "root-a"));
        assert_eq!(state.app_tabs.len(), 1);
        let tab = &state.app_tabs[0];
        assert_eq!(tab.root_path.as_deref(), Some("/repo"));
        assert_eq!(tab.root_token.as_deref(), Some("root-a"));
        assert_eq!(tab.view_mode, Some(AppTabViewMode::Split));
        assert_eq!(tab.title, "README.md");
        open_readme(&mut state, ("/other", "root-b"));
        assert_eq!(state.app_tabs.len(), 1);
        assert_eq!(state.app_tabs[0].root_token.as_deref(), Some("root-a"));
        assert!(set_markdown_view_mode(
            &mut state,
            "local",
            &id,
            AppTabViewMode::Preview
        ));
        assert_eq!(state.app_tabs[0].view_mode, Some(AppTabViewMode::Preview));
        assert!(!set_markdown_view_mode(
            &mut state,
            "local",
            &id,
            AppTabViewMode::Preview
        ));
    }

    #[test]
    fn refreshes_an_existing_terminal_opened_tab_capability_without_resetting_its_view() {
        let mut state = empty();
        let open_prompt = |state: &mut PersistedAppState, token: &str, refresh_root: bool| {
            open_file_tab(
                state,
                "local",
                "server-a",
                &sessions()[1],
                "/scratch/prompt.md",
                AppTabKind::Markdown,
                ("/scratch", token),
                OpenOptions {
                    refresh_root,
                    ..OpenOptions::default()
                },
            )
        };
        let id = open_prompt(&mut state, "file-v2:old", false);
        set_markdown_view_mode(&mut state, "local", &id, AppTabViewMode::Preview);
        open_prompt(&mut state, "file-v2:new", true);
        assert_eq!(state.app_tabs.len(), 1);
        let tab = &state.app_tabs[0];
        assert_eq!(tab.root_path.as_deref(), Some("/scratch"));
        assert_eq!(tab.root_token.as_deref(), Some("file-v2:new"));
        assert_eq!(tab.view_mode, Some(AppTabViewMode::Preview));
    }

    #[test]
    fn a_new_markdown_tab_starts_in_the_configured_mode() {
        let mut state = empty();
        open_file_tab(
            &mut state,
            "local",
            "server-a",
            &sessions()[1],
            "/repo/notes.md",
            AppTabKind::Markdown,
            ("/repo", "t"),
            OpenOptions {
                view_mode: Some(AppTabViewMode::Live),
                ..OpenOptions::default()
            },
        );
        assert_eq!(state.app_tabs[0].view_mode, Some(AppTabViewMode::Live));
        open(&mut state, "/repo/a.rs", false);
        assert_eq!(state.app_tabs[1].view_mode, None);
    }

    #[test]
    fn titles_and_kinds_come_from_the_path() {
        assert_eq!(file_tab_title("/repo/src/main.rs"), "main.rs");
        assert_eq!(file_tab_title("/repo/dir/"), "dir");
        assert_eq!(kind_for_path("/repo/README.MD"), AppTabKind::Markdown);
        assert_eq!(kind_for_path("/repo/notes.mdown"), AppTabKind::Markdown);
        assert_eq!(kind_for_path("/repo/notes.markdown"), AppTabKind::File);
    }
}
