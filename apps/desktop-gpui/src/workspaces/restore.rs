//! Remembering which workspaces each host had open, and reopening them when
//! that host's tmux server comes back without them. Port of
//! `features/sessionRestore/model.ts` and `app/useWorkspaceRestore.ts`.
//!
//! The whole feature is two rules over the same snapshot data:
//!
//! - **Capture**, after each host's first settled snapshot: fold what the
//!   snapshot says is open, and in which directories, into the persisted
//!   record.
//! - **Restore**, once per host link: create every remembered workspace whose
//!   *name* is absent from the snapshot (sessions the user closed are gone
//!   from the record, so they are never resurrected), and land on the
//!   remembered workspace on the host on screen.
//!
//! What it deliberately does not touch: a workspace that exists. tmux sessions
//! outlive this app by design, so an ordinary restart restores nothing but the
//! selection; the creates only fire against a server that lost them, after a
//! reboot or a `kill-server`. And capture never runs before that link's
//! restore has settled, because on a fresh server the first snapshot is the
//! empty one that would otherwise overwrite the very record the restore is
//! about to act on.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use desktop_core::app_state::{
    AppStateStore, WorkspaceRestoreHost, WorkspaceRestoreTab, WorkspaceRestoreWorkspace,
};
use tmux_control::TmuxSnapshot;

use super::model::ordered_sessions;

/// The host's own ceiling on a tmux name (`validate_name` in the host's `command.rs`).
const MAX_TMUX_NAME_LENGTH: usize = 200;
/// The caps `app_state` validates a saved record against; a capture past them
/// would refuse the whole file, so it stops at the cap instead.
const MAX_TABS_PER_WORKSPACE: usize = 64;
const MAX_WORKSPACES_PER_HOST: usize = 256;

/// The workspaces a host had open, with the directory of every tab.
///
/// A tab is captured by the *active* pane's directory, the one the user was
/// last looking at in that tab; a split's other panes are deliberately not
/// remembered, because recreating a split tree is a different, larger feature
/// and a half-restored one is worse than an honest single terminal.
///
/// A path tmux could not resolve (empty, or not absolute) restores at tmux's
/// own choice rather than at a guessed one, and a workspace name the host
/// could never create (control characters, or past its length ceiling) is
/// dropped rather than saved into a record whose restore is guaranteed to fail.
pub fn capture(
    snapshot: &TmuxSnapshot,
    selected_session_name: Option<&str>,
) -> WorkspaceRestoreHost {
    let mut workspaces: Vec<WorkspaceRestoreWorkspace> = Vec::new();
    for session in ordered_sessions(&snapshot.sessions) {
        if workspaces.len() >= MAX_WORKSPACES_PER_HOST {
            break;
        }
        let Some(name) = restorable_name(&session.name) else {
            continue;
        };
        let mut tabs: Vec<WorkspaceRestoreTab> = Vec::new();
        let mut windows: Vec<&tmux_control::Window> = snapshot
            .windows
            .iter()
            .filter(|window| window.session_id == session.id)
            .collect();
        windows.sort_by_key(|window| window.index);
        for window in windows {
            if tabs.len() >= MAX_TABS_PER_WORKSPACE {
                break;
            }
            let pane = snapshot
                .panes
                .iter()
                .find(|pane| pane.window_id == window.id && pane.active)
                .or_else(|| {
                    snapshot
                        .panes
                        .iter()
                        .find(|pane| pane.window_id == window.id)
                });
            let directory = pane
                .and_then(|pane| restorable_directory(&pane.current_path))
                .map(str::to_owned);
            tabs.push(WorkspaceRestoreTab { directory });
        }
        workspaces.push(WorkspaceRestoreWorkspace {
            name: name.to_owned(),
            tabs,
        });
    }
    WorkspaceRestoreHost {
        workspaces,
        selected_session_name: selected_session_name
            .and_then(|name| restorable_name(name))
            .map(str::to_owned),
    }
}

/// What to create on a host that is missing remembered workspaces.
///
/// Matching is by name against the *live* snapshot, so this is safe to run
/// whenever it is convenient: workspaces that are already open (the ordinary
/// case, since tmux sessions outlive the app) come back planned as nothing,
/// and a second run after a first that half-succeeded creates only what is
/// still missing.
pub struct Plan {
    pub name: String,
    /// The first entry starts the session; the rest are the further tabs.
    pub directories: Vec<Option<String>>,
}

pub fn plan(remembered: Option<&WorkspaceRestoreHost>, snapshot: &TmuxSnapshot) -> Vec<Plan> {
    let Some(remembered) = remembered else {
        return Vec::new();
    };
    let open: HashSet<&str> = snapshot
        .sessions
        .iter()
        .map(|session| session.name.as_str())
        .collect();
    remembered
        .workspaces
        .iter()
        .filter(|workspace| !open.contains(workspace.name.as_str()))
        .map(|workspace| Plan {
            name: workspace.name.clone(),
            directories: workspace
                .tabs
                .iter()
                .map(|tab| tab.directory.clone())
                .collect(),
        })
        .collect()
}

/// Whether a capture changes the record. A host with nothing open and no
/// remembered selection is a record that should not exist at all, so an
/// absent entry and an empty capture agree.
pub fn record_changed(
    remembered: Option<&WorkspaceRestoreHost>,
    captured: &WorkspaceRestoreHost,
) -> bool {
    match remembered {
        None => !(captured.workspaces.is_empty() && captured.selected_session_name.is_none()),
        Some(remembered) => remembered != captured,
    }
}

/// Whether a capture is the empty one that deletes its host's entry.
pub fn is_empty(captured: &WorkspaceRestoreHost) -> bool {
    captured.workspaces.is_empty() && captured.selected_session_name.is_none()
}

/// The tracker [`super::Workspaces`] keeps: the loaded record, which links'
/// epochs have run their restore, and which restores are in flight. Capture
/// waits for both marks, exactly as the React hook's two refs did.
#[derive(Default)]
pub struct Tracker {
    /// The newest captured record per host profile, loaded once at startup.
    pub records: BTreeMap<String, WorkspaceRestoreHost>,
    /// Whether the saved record has been read yet. Nothing runs before it has.
    pub loaded: bool,
    /// Per link: the connection epoch whose restore has started.
    pub restored_epochs: HashMap<String, u64>,
    /// The links whose restore is still creating.
    pub in_flight: HashSet<String>,
    /// The store the record persists through.
    pub store: Option<Arc<AppStateStore>>,
}

impl Tracker {
    /// Whether this link's restore has settled: started for this very epoch,
    /// and nothing in flight. Capture only runs against such a link.
    pub fn restore_settled(&self, profile_id: &str, epoch: u64) -> bool {
        self.restored_epochs.get(profile_id) == Some(&epoch) && !self.in_flight.contains(profile_id)
    }

    /// Whether this link's restore should start now, taking the marks when it
    /// does, so a second tick while one runs does not start a second.
    pub fn begin_restore(&mut self, profile_id: &str, epoch: u64) -> bool {
        if self.restored_epochs.get(profile_id) == Some(&epoch)
            || self.in_flight.contains(profile_id)
        {
            return false;
        }
        self.restored_epochs.insert(profile_id.to_owned(), epoch);
        self.in_flight.insert(profile_id.to_owned());
        true
    }
}

fn restorable_name(name: &str) -> Option<&str> {
    if name.is_empty() || name.chars().count() > MAX_TMUX_NAME_LENGTH || has_control_character(name)
    {
        return None;
    }
    Some(name)
}

/// Only an absolute path is worth restoring. tmux reports an empty
/// `pane_current_path` when it cannot read one, and the host's create refuses
/// anything not absolute anyway, so remembering it would only guarantee a
/// refusal at restore time where tmux's own choice would have worked.
fn restorable_directory(path: &str) -> Option<&str> {
    if !path.starts_with('/') || has_control_character(path) {
        return None;
    }
    Some(path)
}

fn has_control_character(value: &str) -> bool {
    value.chars().any(|ch| ch.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tmux_control::{Pane, Session, Window};

    fn session(id: &str, name: &str, order: u32) -> Session {
        Session {
            id: id.into(),
            name: name.into(),
            window_count: 0,
            attached_clients: 0,
            order,
            pinned: false,
        }
    }

    fn tab(id: &str, session_id: &str, index: u32) -> Window {
        Window {
            id: id.into(),
            session_id: session_id.into(),
            index,
            name: format!("w{index}"),
            active: false,
            layout: String::new(),
            zoomed: false,
            pinned: false,
        }
    }

    fn pane(id: &str, window_id: &str, current_path: &str, active: bool) -> Pane {
        Pane {
            id: id.into(),
            session_id: String::new(),
            window_id: window_id.into(),
            index: 0,
            active,
            width: 80,
            height: 24,
            left: 0,
            top: 0,
            current_path: current_path.into(),
            current_command: "bash".into(),
            pane_pid: 1,
            start_command: String::new(),
            title: String::new(),
            chat_adapter_id: String::new(),
        }
    }

    fn snapshot(sessions: Vec<Session>, windows: Vec<Window>, panes: Vec<Pane>) -> TmuxSnapshot {
        TmuxSnapshot {
            sessions,
            windows,
            panes,
        }
    }

    #[test]
    fn captures_workspaces_in_list_order_with_the_active_pane_directory_of_every_tab() {
        let captured = capture(
            &snapshot(
                vec![session("$2", "beta", 1), session("$1", "alpha", 0)],
                vec![tab("@2", "$1", 1), tab("@1", "$1", 0), tab("@3", "$2", 0)],
                vec![
                    // Tab order follows the window index, not the arrival order.
                    pane("%2", "@2", "/work/api", true),
                    pane("%1", "@1", "/work", true),
                    pane("%1b", "@1", "/work/split", false),
                    pane("%3", "@3", "/srv", true),
                ],
            ),
            Some("beta"),
        );
        assert_eq!(
            captured,
            WorkspaceRestoreHost {
                workspaces: vec![
                    WorkspaceRestoreWorkspace {
                        name: "alpha".into(),
                        tabs: vec![
                            WorkspaceRestoreTab {
                                directory: Some("/work".into())
                            },
                            WorkspaceRestoreTab {
                                directory: Some("/work/api".into())
                            },
                        ],
                    },
                    WorkspaceRestoreWorkspace {
                        name: "beta".into(),
                        tabs: vec![WorkspaceRestoreTab {
                            directory: Some("/srv".into())
                        }],
                    },
                ],
                selected_session_name: Some("beta".into()),
            }
        );
    }

    #[test]
    fn falls_back_to_any_pane_of_the_tab_when_none_reports_itself_active() {
        let captured = capture(
            &snapshot(
                vec![session("$1", "only", 0)],
                vec![tab("@1", "$1", 0)],
                vec![pane("%1", "@1", "/fallback", false)],
            ),
            None,
        );
        assert_eq!(
            captured.workspaces[0].tabs[0].directory.as_deref(),
            Some("/fallback")
        );
        // No remembered selection is an absent field, not an empty one.
        assert_eq!(captured.selected_session_name, None);
    }

    #[test]
    fn leaves_tabs_tmux_cannot_locate_and_unrestorable_names_out() {
        let captured = capture(
            &snapshot(
                vec![
                    session("$1", "fine", 0),
                    session("$2", "bad\u{7}name", 1),
                    session("$3", "", 2),
                ],
                vec![tab("@1", "$1", 0), tab("@2", "$1", 1), tab("@3", "$1", 2)],
                vec![
                    // An empty path is tmux saying it could not read one; a
                    // relative one is not a path the host's create accepts.
                    pane("%1", "@1", "", true),
                    pane("%2", "@2", "relative/path", true),
                    pane("%3", "@3", "/good", true),
                ],
            ),
            Some("bad\u{7}name"),
        );
        assert_eq!(captured.workspaces.len(), 1);
        assert_eq!(captured.workspaces[0].name, "fine");
        assert_eq!(captured.workspaces[0].tabs[0].directory, None);
        assert_eq!(captured.workspaces[0].tabs[1].directory, None);
        assert_eq!(
            captured.workspaces[0].tabs[2].directory.as_deref(),
            Some("/good")
        );
        // An impossible name is dropped from the selection too.
        assert_eq!(captured.selected_session_name, None);
    }

    #[test]
    fn plans_only_the_remembered_workspaces_the_snapshot_does_not_have() {
        let remembered = WorkspaceRestoreHost {
            workspaces: vec![
                WorkspaceRestoreWorkspace {
                    name: "gone".into(),
                    tabs: vec![
                        WorkspaceRestoreTab {
                            directory: Some("/one".into()),
                        },
                        WorkspaceRestoreTab { directory: None },
                        WorkspaceRestoreTab {
                            directory: Some("/three".into()),
                        },
                    ],
                },
                WorkspaceRestoreWorkspace {
                    name: "alive".into(),
                    tabs: vec![WorkspaceRestoreTab {
                        directory: Some("/live".into()),
                    }],
                },
            ],
            selected_session_name: Some("gone".into()),
        };
        let live = snapshot(vec![session("$1", "alive", 0)], vec![], vec![]);
        let planned = plan(Some(&remembered), &live);
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].name, "gone");
        assert_eq!(
            planned[0].directories,
            vec![Some("/one".to_owned()), None, Some("/three".to_owned())]
        );
        // Nothing remembered plans nothing; a full snapshot also plans nothing.
        assert!(plan(None, &live).is_empty());
        assert!(
            plan(
                Some(&remembered),
                &snapshot(
                    vec![session("$1", "alive", 0), session("$2", "gone", 1)],
                    vec![],
                    vec![]
                )
            )
            .is_empty()
        );
    }

    #[test]
    fn an_absent_entry_and_an_empty_capture_agree() {
        let empty = WorkspaceRestoreHost::default();
        assert!(!record_changed(None, &empty));
        assert!(is_empty(&empty));
        let with_one = WorkspaceRestoreHost {
            workspaces: vec![WorkspaceRestoreWorkspace {
                name: "x".into(),
                tabs: vec![WorkspaceRestoreTab { directory: None }],
            }],
            selected_session_name: None,
        };
        assert!(record_changed(None, &with_one));
        assert!(!record_changed(Some(&with_one), &with_one));
        assert!(!is_empty(&with_one));
    }

    #[test]
    fn restore_starts_once_per_epoch_and_capture_waits_for_it_to_settle() {
        let mut tracker = Tracker::default();
        assert!(tracker.begin_restore("$h", 3));
        // While in flight: no second restore, and no capture.
        assert!(!tracker.begin_restore("$h", 3));
        assert!(!tracker.restore_settled("$h", 3));
        tracker.in_flight.remove("$h");
        // Settled: capture may run, and no new restore starts for the epoch.
        assert!(tracker.restore_settled("$h", 3));
        assert!(!tracker.begin_restore("$h", 3));
        // A reconnect is a new epoch: restore runs again.
        assert!(tracker.begin_restore("$h", 4));
    }
}
