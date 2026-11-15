//! The Git diff tab: one per workspace, showing one file at a time. Port of
//! `GitDiffSurface.tsx`, `useSharedGitDiff.ts` and `GitDiffEditor.tsx`.
//!
//! The tab joins the shared observation of its repository and reads its diff
//! only when that status moves past the one its diff was read against. It
//! shows the hunks of Git's own patch, each one a section of Kit's `Diff` that
//! collapses on its own, so a hunk index sent back to the host names the hunk
//! on screen. "Show whole file" draws the whole file instead, wrapped, side by
//! side from 900 pixels wide and inline below that, as the Monaco diff editor
//! did. Only a hunk is staged, unstaged or discarded from here: whole-file
//! actions are the Source Control row's. The default-branch scope is read only.

use std::{cell::Cell, rc::Rc, time::Duration};

use desktop_core::app_state::AppTabRecord;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, FocusHandle, FontWeight,
    InteractiveElement, IntoElement, KeyBinding, ParentElement, Pixels, Render, Role, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, actions,
    base::TestSupportExt,
    canvas,
    component::{
        Disableable, Sizable,
        button::Button,
        diff::{
            Diff as DiffElement, DiffDecoration, DiffFile, DiffHunkSeparator, DiffLineRange,
            DiffMode, DiffSide, DiffState,
        },
        h_flex, v_flex,
    },
    div,
    prelude::FluentBuilder,
};

use crate::{
    commands::dialogs,
    fonts,
    perf::{PaintReporter, PaintTicket},
    theme::{
        self, Palette,
        metrics::{BAR_HEIGHT, TYPE_LABEL, TYPE_META, ui},
    },
    ui::{
        surface_error::SurfaceError,
        toolbar::{Tone, toolbar_button},
    },
};

use super::{
    client,
    model::{
        self, CommandResult, Diff, DiffRequest, GitScope, Hunk, Mutation, MutationKind, RootInfo,
        Shown, StatusSnapshot, Target,
    },
    panel::GitPanel,
    store::GitRepositories,
};

actions!(git_diff, [CollapseHunks, ExpandHunks]);

/// Focus anywhere in the diff tab.
const CONTEXT: &str = "GitDiff";

/// VS Code's fold-all and unfold-all chords on macOS, for every hunk at once.
/// Linux takes Ctrl+Alt: Ctrl+Shift+[ and ] are the app's previous and next tab.
pub(super) fn init(cx: &mut App) {
    cx.bind_keys([
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-cmd-[", CollapseHunks, Some(CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-cmd-]", ExpandHunks, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-alt-[", CollapseHunks, Some(CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("ctrl-alt-]", ExpandHunks, Some(CONTEXT)),
    ]);
}

/// Kit could not read the patch built from the two sides.
const UNREADABLE: &str = "This diff could not be displayed.";

/// Monaco's own line backgrounds, the same in both themes: the whole removed or added
/// line, where Kit tints it at 12% of the theme's red or green.
const REMOVED_LINE: (f32, f32, f32) = (255., 0., 0.);
const ADDED_LINE: (f32, f32, f32) = (155., 185., 85.);
const LINE_ALPHA: f32 = 0.2;

/// The removed and added lines of a one-hunk patch, as runs of lines on their own side.
fn changed_lines(path: &SharedString, patch: &str) -> Vec<DiffDecoration> {
    let colour = |(r, g, b): (f32, f32, f32)| {
        gpui_kit::rgba(u32::from_be_bytes([
            r as u8,
            g as u8,
            b as u8,
            (LINE_ALPHA * 255.) as u8,
        ]))
    };
    let (mut old, mut new) = (0usize, 0usize);
    let mut runs: Vec<(DiffSide, usize, usize)> = Vec::new();
    let mut push = |side: DiffSide, line: usize| match runs.last_mut() {
        Some((last, _, end)) if *last == side && *end + 1 == line => *end = line,
        _ => runs.push((side, line, line)),
    };
    let mut in_hunk = false;
    for line in patch.lines() {
        if let Some(header) = line.strip_prefix("@@ -") {
            let start = |part: &str| part.split([',', ' ']).next()?.parse::<usize>().ok();
            old = start(header).unwrap_or(1);
            new = header
                .split_once('+')
                .and_then(|(_, rest)| start(rest))
                .unwrap_or(1);
            in_hunk = true;
        } else if !in_hunk {
            continue;
        } else if line.starts_with('-') {
            push(DiffSide::Original, old);
            old += 1;
        } else if line.starts_with('+') {
            push(DiffSide::Modified, new);
            new += 1;
        } else if line.starts_with(' ') || line.is_empty() {
            old += 1;
            new += 1;
        }
    }
    runs.into_iter()
        .map(|(side, start, end)| {
            let tint = if side == DiffSide::Original {
                REMOVED_LINE
            } else {
                ADDED_LINE
            };
            DiffDecoration::new(DiffLineRange::new(path.clone(), side, start, end))
                .with_background(colour(tint))
        })
        .collect()
}

/// The diff body, built off the UI thread.
enum Built {
    Blocked(&'static str),
    Unreadable,
    Files(Vec<DiffFile>, Vec<DiffDecoration>),
    /// One section per hunk of Git's patch.
    Hunks(Vec<DiffFile>, Vec<DiffDecoration>, Vec<Hunk>),
}

enum Body {
    Blocked(&'static str),
    Drawn(Entity<DiffState>),
}

const DIFFERENT_REPOSITORY: &str =
    "This diff belongs to a different repository. Return to its workspace or close the tab.";

/// The width at which Monaco put the two sides beside each other.
const SIDE_BY_SIDE_WIDTH: f32 = 900.;

/// Frames the diff draws transparent while its list measures its rows.
const SETTLE_FRAMES: u8 = 3;

/// How long a load stays silent before saying it is loading, so a fast open
/// shows the frame and then the content.
const LOADING_DELAY: Duration = Duration::from_millis(150);

/// A hunk discard being confirmed, with what it was decided on.
#[derive(Clone)]
struct PendingDiscard {
    hunk_index: u32,
    diff: Rc<Diff>,
    status: Rc<StatusSnapshot>,
    root_token: String,
    terminal_epoch: u64,
}

pub struct GitDiffView {
    store: Entity<GitRepositories>,
    panel: Entity<GitPanel>,
    tab: AppTabRecord,
    /// The observation this tab holds, and the scope it was acquired for.
    acquired: Option<(String, GitScope)>,
    diff: Option<Rc<Diff>>,
    status: Option<Rc<StatusSnapshot>>,
    /// What the diff's text became, once built off the UI thread.
    body: Option<Body>,
    build_task: Option<Task<()>>,
    building_shown: bool,
    loading: bool,
    /// Whether a load has run long enough to say so.
    loading_shown: bool,
    /// This tab's own failure; the shared observation's is held apart so
    /// clearing one never clears the other.
    error: Option<String>,
    shared_error: Option<String>,
    /// The generation the last load set out from or was answered with, so a
    /// watch event describing the same status does not read it twice.
    requested: Option<u64>,
    /// A command owns its own reload; while one runs, the status it publishes
    /// must not start a second one.
    commanding: bool,
    busy: bool,
    serial: u64,
    /// The read in flight. Dropping it cancels the read at the host.
    load_task: Option<Task<()>>,
    /// The width the diff was last drawn at.
    width: Rc<Cell<Option<Pixels>>>,
    /// Frames drawn since the diff was built; it stays transparent for the first few.
    settled: Rc<Cell<u8>>,
    /// The changed lines' backgrounds, which Kit's own faint tint stands in for.
    marks: Rc<Vec<DiffDecoration>>,
    /// The hunks drawn, in the host's order. Empty for the whole file.
    hunks: Rc<Vec<Hunk>>,
    /// The lines of the hunks collapsed when the body was last drawn.
    collapsed: Rc<Vec<String>>,
    /// "Show whole file": today's full-file diff instead of the hunks.
    whole: bool,
    focus: FocusHandle,
    /// `workflow.git.diffPaint`: from a read to its diff on screen. Empty
    /// unless the perf probe is on.
    paint: PaintReporter,
    _delay: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

impl GitDiffView {
    pub fn new(panel: Entity<GitPanel>, tab: AppTabRecord, cx: &mut Context<Self>) -> Self {
        let store = panel.read(cx).store().clone();
        let subscriptions = [
            cx.observe(&store, |this, _, cx| this.status_moved(cx)),
            cx.observe(&panel, |this, _, cx| this.bind(cx)),
        ];
        cx.on_release(|this, cx| {
            if let Some((key, _)) = this.acquired.take() {
                this.store.update(cx, |store, cx| store.release(&key, cx));
            }
        })
        .detach();
        let mut view = Self {
            store,
            panel,
            tab,
            acquired: None,
            diff: None,
            status: None,
            body: None,
            build_task: None,
            building_shown: false,
            loading: true,
            loading_shown: false,
            error: None,
            shared_error: None,
            requested: None,
            commanding: false,
            busy: false,
            serial: 0,
            load_task: None,
            width: Rc::default(),
            settled: Rc::default(),
            marks: Rc::default(),
            hunks: Rc::default(),
            collapsed: Rc::default(),
            whole: false,
            focus: cx.focus_handle(),
            paint: PaintReporter::default(),
            _delay: None,
            _subscriptions: subscriptions,
        };
        view.bind(cx);
        view
    }

    /// The tab record as it is now: a repointed tab reads its new file.
    pub fn set_tab(&mut self, tab: &AppTabRecord, cx: &mut Context<Self>) {
        if self.tab == *tab {
            return;
        }
        let same_file = self.tab.git_repository_id == tab.git_repository_id
            && self.tab.git_path == tab.git_path
            && self.tab.git_original_path == tab.git_original_path
            && self.tab.git_target == tab.git_target;
        self.tab = tab.clone();
        if !same_file {
            // A different file under the same observation owes a read now; the
            // observation itself is kept.
            self.serial += 1;
            self.load_task = None;
            self.requested = None;
            self.status = None;
            self.error = None;
            self.whole = false;
            self.show(None, cx);
            self.collapsed = Rc::default();
        }
        self.bind(cx);
        self.status_moved(cx);
        cx.notify();
    }

    /// The tab's own root when it saved one, else the active pane's.
    fn root(&self, cx: &App) -> Option<RootInfo> {
        match (&self.tab.root_path, &self.tab.root_token) {
            (Some(path), Some(token)) => Some(RootInfo {
                path: path.clone(),
                token: token.clone(),
                git_worktree: true,
            }),
            _ => self.panel.read(cx).context().root.clone(),
        }
    }

    /// Only the tab's own host and tmux server can answer for it.
    fn scope(&self, cx: &App) -> Option<GitScope> {
        let root = self.root(cx)?;
        let link = &self.panel.read(cx).context().link;
        let own = link.host_profile_id == self.tab.host_profile_id
            && link.server_identity.as_deref() == Some(self.tab.server_identity.as_str());
        own.then(|| model::git_scope(link, &root)).flatten()
    }

    fn can_write(&self, cx: &App) -> bool {
        self.panel.read(cx).context().can_mutate
    }

    /// Holds the observation the tab's scope names, and only that one.
    fn bind(&mut self, cx: &mut Context<Self>) {
        let scope = self
            .scope(cx)
            .filter(|_| self.tab.git_repository_id.is_some());
        let key = scope.as_ref().map(GitScope::key);
        if key == self.acquired.as_ref().map(|(key, _)| key.clone()) {
            return;
        }
        self.release(cx);
        if let Some(scope) = scope {
            let key = self.store.update(cx, |store, cx| store.acquire(&scope, cx));
            self.acquired = Some((key, scope));
            // Silent at first and then "Loading…", while the observation boots.
            self.set_loading(true, cx);
            self.status_moved(cx);
        }
        cx.notify();
    }

    fn release(&mut self, cx: &mut Context<Self>) {
        self.serial += 1;
        self.load_task = None;
        self.requested = None;
        if let Some((key, _)) = self.acquired.take() {
            self.store.update(cx, |store, cx| store.release(&key, cx));
        }
    }

    /// Loading follows the shared status and never runs ahead of it: a read
    /// before the repository is observed would be repeated at once.
    fn status_moved(&mut self, cx: &mut Context<Self>) {
        let Some((key, _)) = &self.acquired else {
            return;
        };
        let next = self.store.read(cx).state(key);
        if self.shared_error != next.error {
            self.shared_error = next.error.clone();
            cx.notify();
        }
        // The observation has answered, even if the answer is a failure.
        if !next.loading && self.load_task.is_none() {
            self.set_loading(false, cx);
        }
        let Some(status) = next.status else {
            return;
        };
        if self.commanding || Some(status.generation) == self.requested {
            return;
        }
        self.load(false, cx);
    }

    fn set_loading(&mut self, loading: bool, cx: &mut Context<Self>) {
        // The first load starts out loading but has no timer yet.
        if self.loading == loading && (!loading || self._delay.is_some()) {
            return;
        }
        self.loading = loading;
        self.loading_shown = false;
        self._delay = loading.then(|| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LOADING_DELAY).await;
                this.update(cx, |this, cx| {
                    this.loading_shown = this.loading;
                    cx.notify();
                })
                .ok();
            })
        });
        cx.notify();
    }

    /// One round trip: the answer names the status it was read against, so
    /// the tab never asks for status first.
    fn load(&mut self, clear_stale: bool, cx: &mut Context<Self>) {
        let Some((key, scope)) = self.acquired.clone() else {
            return;
        };
        let (Some(repository_id), Some(path), Some(target)) = (
            self.tab.git_repository_id.clone(),
            self.tab.git_path.clone(),
            self.tab.git_target,
        ) else {
            return;
        };
        self.serial += 1;
        let serial = self.serial;
        self.paint.abandon();
        let paint = PaintTicket::start(&["workflow.git.diffPaint"]);
        let shared = self.store.read(cx).state(&key).status;
        // Claimed before the read, so a watch event describing the same
        // status cannot start a second read of it.
        self.requested = shared.as_ref().map(|status| status.generation);
        // A superseded read stops at the host, bulk bodies included.
        self.load_task = None;
        self.set_loading(true, cx);
        if clear_stale {
            self.show(None, cx);
            self.status = None;
            self.error = None;
        }
        if let Some(shared) = shared {
            // A saved tab whose root now resolves elsewhere has nothing to ask.
            if shared.repository.id != repository_id {
                self.error = Some(DIFFERENT_REPOSITORY.into());
                return self.set_loading(false, cx);
            }
            // The shared status already says this file has no such change. It
            // says nothing about the default branch.
            if target != Target::Base && !model::entry_still_changed(&shared, &path, target) {
                self.status = Some(shared);
                self.show(None, cx);
                self.error = None;
                return self.set_loading(false, cx);
            }
        }
        let request = DiffRequest {
            repository_id: repository_id.clone(),
            path: path.clone(),
            original_path: self.tab.git_original_path.clone(),
            target,
        };
        let transport = self.store.read(cx).transport();
        let whole = self.whole;
        self.load_task = Some(cx.spawn(async move |this, cx| {
            let answer = client::diff(transport, scope, request, whole).await;
            this.update(cx, |this, cx| {
                if serial != this.serial {
                    return;
                }
                this.load_task = None;
                match answer {
                    Ok((_, status)) if status.repository.id != repository_id => {
                        // The attempted generation is kept: a failure the
                        // repository has not moved past is not retried on
                        // every publication. Retry is the person's.
                        this.error = Some(DIFFERENT_REPOSITORY.into());
                    }
                    Ok((diff, status)) => {
                        // The answer may be newer than the status the read
                        // set out from; claiming it keeps it from looking stale,
                        // and the shared observation follows it.
                        this.requested = Some(status.generation);
                        this.store
                            .update(cx, |store, cx| store.accept(&key, status.clone(), cx));
                        let still = if target == Target::Base {
                            has_changes(&diff)
                        } else {
                            model::entry_still_changed(&status, &path, target)
                        };
                        this.status = Some(Rc::new(status));
                        if still {
                            this.paint.hold(serial, paint);
                            this.paint.note_committed(serial);
                        }
                        this.show(still.then_some(diff), cx);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                this.set_loading(false, cx);
                cx.notify();
            })
            .ok();
        }));
    }

    fn show(&mut self, diff: Option<Diff>, cx: &mut Context<Self>) {
        // A reload that found the same text keeps the drawn diff, and with it
        // the scroll position, as Monaco did.
        if let (Some(new), Some(old), Some(Body::Drawn(_))) = (&diff, &self.diff, &self.body)
            && same_text(new, old)
        {
            self.diff = diff.map(Rc::new);
            return cx.notify();
        }
        let mode = match &self.body {
            Some(Body::Drawn(state)) => state.read(cx).mode(),
            _ => DiffMode::Unified,
        };
        // A hunk collapsed before a re-read, a stage say, stays collapsed. Its
        // lines name it: the `@@` numbers and the index move when another
        // hunk is staged. Kept across the cleared body a reload shows first.
        // The whole file has no hunks to say anything about.
        if let (Some(Body::Drawn(state)), false) = (&self.body, self.hunks.is_empty()) {
            let state = state.read(cx);
            self.collapsed = Rc::new(
                self.hunks
                    .iter()
                    .filter(|hunk| state.is_file_collapsed(&hunk.key))
                    .map(|hunk| hunk_lines(hunk).to_owned())
                    .collect(),
            );
        }
        let collapsed = self.collapsed.clone();
        self.body = None;
        self.building_shown = false;
        // Monaco diffed in a worker: the decode, the diff and the parse of a
        // file of several megabytes stay off the UI thread here too.
        let whole = self.whole;
        self.build_task = diff.clone().map(|diff| {
            let built = cx
                .background_executor()
                .spawn(async move { build(&diff, whole) });
            cx.spawn(async move |this, cx| {
                // Silent at first, as a load is.
                let delay = cx.background_executor().timer(LOADING_DELAY);
                let shown = this.clone();
                let mut built = std::pin::pin!(built);
                let built = match futures::future::select(delay, built.as_mut()).await {
                    futures::future::Either::Left(_) => {
                        shown
                            .update(cx, |this, cx| {
                                this.building_shown = true;
                                cx.notify();
                            })
                            .ok();
                        built.await
                    }
                    futures::future::Either::Right((built, _)) => built,
                };
                this.update(cx, |this, cx| {
                    this.settled.set(0);
                    this.hunks = Rc::default();
                    this.body = Some(match built {
                        Built::Blocked(detail) => Body::Blocked(detail),
                        Built::Unreadable => Body::Blocked(UNREADABLE),
                        Built::Files(files, marks) => {
                            this.marks = Rc::new(marks);
                            Body::Drawn(cx.new(|cx| {
                                DiffState::new(files, cx)
                                    .with_mode(mode)
                                    .with_context_lines(None)
                            }))
                        }
                        Built::Hunks(files, marks, hunks) => {
                            this.marks = Rc::new(marks);
                            let state = cx.new(|cx| {
                                let mut state = DiffState::new(files, cx)
                                    .with_mode(mode)
                                    .with_context_lines(None);
                                for hunk in &hunks {
                                    if collapsed.iter().any(|lines| lines == hunk_lines(hunk)) {
                                        state.set_file_collapsed(&hunk.key, true, cx);
                                    }
                                }
                                state
                            });
                            this.hunks = Rc::new(hunks);
                            Body::Drawn(state)
                        }
                    });
                    cx.notify();
                })
                .ok();
            })
        });
        self.diff = diff.map(Rc::new);
        cx.notify();
    }

    /// "Show whole file" and back. The whole file needs both sides, which a
    /// hunks read left on the host when they were large: then it reads again.
    fn toggle_whole(&mut self, cx: &mut Context<Self>) {
        self.whole = !self.whole;
        // A read in flight was asked for hunks only: ask again with the sides.
        // Forgetting the drawn diff keeps its answer from passing as unchanged.
        if self.whole && self.load_task.is_some() {
            self.diff = None;
            return self.load(false, cx);
        }
        let Some(diff) = self.diff.clone() else {
            return cx.notify();
        };
        let missing = (diff.old_ref.is_some() && diff.old_content.is_empty())
            || (diff.new_ref.is_some() && diff.new_content.is_empty());
        if self.whole && missing {
            return self.load(true, cx);
        }
        self.diff = None;
        self.show(Some((*diff).clone()), cx);
    }

    /// Whether every hunk is collapsed.
    fn all_collapsed(&self, cx: &App) -> bool {
        let Some(Body::Drawn(state)) = &self.body else {
            return false;
        };
        let state = state.read(cx);
        !self.hunks.is_empty()
            && self
                .hunks
                .iter()
                .all(|hunk| state.is_file_collapsed(&hunk.key))
    }

    fn set_all_collapsed(&mut self, collapsed: bool, cx: &mut Context<Self>) {
        let Some(Body::Drawn(state)) = &self.body else {
            return;
        };
        let hunks = self.hunks.clone();
        state.update(cx, |state, cx| {
            for hunk in hunks.iter() {
                state.set_file_collapsed(&hunk.key, collapsed, cx);
            }
        });
        cx.notify();
    }

    /// What Refresh and Retry mean: re-read the repository, then decide. A
    /// refresh that moved the status has already started the read.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some((key, _)) = self.acquired.clone() else {
            return;
        };
        let before = self.requested;
        let refreshing = self.store.update(cx, |store, cx| store.refresh(&key, cx));
        cx.spawn(async move |this, cx| {
            refreshing.await;
            this.update(cx, |this, cx| {
                let now = this.store.read(cx).state(&key).status.map(|s| s.generation);
                if now == before {
                    this.load(true, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn mutate(&mut self, kind: MutationKind, hunk_index: u32, cx: &mut Context<Self>) {
        let (Some(diff), Some(status)) = (self.diff.clone(), self.status.clone()) else {
            return;
        };
        if !status.authoritative || !self.can_write(cx) {
            return;
        }
        let mutation = Mutation {
            kind,
            path: diff.path.clone(),
            original_path: diff.original_path.clone(),
            target: diff.target,
            expected_status_generation: status.generation,
            expected_source_generation: diff.source_generation.clone(),
            hunk_index: Some(hunk_index),
            confirmation_token: None,
        };
        self.command(mutation, status.repository.id.clone(), cx);
    }

    /// One request and the single reload it owes. Only a result that says
    /// more than the redrawn diff does becomes a notice.
    fn command(&mut self, mut mutation: Mutation, repository_id: String, cx: &mut Context<Self>) {
        let Some((key, scope)) = self.acquired.clone() else {
            return;
        };
        self.busy = true;
        self.commanding = true;
        cx.notify();
        let (store, transport) = (self.store.clone(), self.store.read(cx).transport());
        cx.spawn(async move |this, cx| {
            let result = async {
                if mutation.kind.is_discard() {
                    let token = client::prepare_discard(
                        transport.clone(),
                        scope.clone(),
                        repository_id.clone(),
                        mutation.clone(),
                    )
                    .await?;
                    mutation.confirmation_token = Some(token);
                }
                client::mutate(transport, scope, repository_id, mutation).await
            }
            .await;
            // The repository follows the command even if the tab has closed.
            if let Ok(CommandResult {
                status: Some(status),
                ..
            }) = &result
            {
                store.update(cx, |store, cx| store.accept(&key, status.clone(), cx));
            }
            let notice = this.update(cx, |this, cx| {
                this.busy = false;
                let notice = match &result {
                    Ok(result) => {
                        this.commanding = false;
                        this.load(true, cx);
                        model::needs_notice(result)
                    }
                    Err(error) => {
                        this.commanding = false;
                        this.error = Some(error.clone());
                        false
                    }
                };
                cx.notify();
                notice
            });
            // A tab closed meanwhile still says what happened.
            if let Ok(result) = result
                && notice.unwrap_or_else(|_| model::needs_notice(&result))
            {
                let message = model::result_message(&result, "Git change applied.");
                cx.update(|cx| super::report(message, cx));
            }
        })
        .detach();
    }

    fn request_discard(&mut self, hunk_index: u32, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(diff), Some(status), Some(root), Some(scope)) = (
            self.diff.clone(),
            self.status.clone(),
            self.root(cx),
            self.scope(cx),
        ) else {
            return;
        };
        let pending = PendingDiscard {
            hunk_index,
            diff: diff.clone(),
            status,
            root_token: root.token,
            terminal_epoch: scope.terminal_epoch,
        };
        let this = cx.entity().downgrade();
        dialogs::open_confirmation(
            dialogs::Confirmation {
                title: "Discard complete hunk?".into(),
                detail: format!(
                    "Discard hunk {} in {}? This cannot be undone by the app.",
                    hunk_index + 1,
                    diff.display_path
                )
                .into(),
                confirm_label: "Discard".into(),
                destructive: true,
                on_confirm: Rc::new(move |_, cx| {
                    let pending = pending.clone();
                    this.update(cx, |this, cx| this.confirm_discard(pending, cx))
                        .ok();
                }),
            },
            window,
            cx,
        );
    }

    fn confirm_discard(&mut self, pending: PendingDiscard, cx: &mut Context<Self>) {
        let (Some(root), Some(scope)) = (self.root(cx), self.scope(cx)) else {
            return;
        };
        if self.acquired.is_none() || !pending.status.authoritative {
            return;
        }
        if root.token != pending.root_token
            || scope.terminal_epoch != pending.terminal_epoch
            || self.tab.git_repository_id.as_deref() != Some(&pending.status.repository.id)
        {
            self.error =
                Some("Discard was cancelled because the repository connection changed.".into());
            return cx.notify();
        }
        let mutation = Mutation {
            kind: MutationKind::DiscardHunk,
            path: pending.diff.path.clone(),
            original_path: pending.diff.original_path.clone(),
            target: pending.diff.target,
            expected_status_generation: pending.status.generation,
            expected_source_generation: pending.diff.source_generation.clone(),
            hunk_index: Some(pending.hunk_index),
            confirmation_token: None,
        };
        self.command(mutation, pending.status.repository.id.clone(), cx);
    }
}

/// Whether a default-branch diff found anything: its patch, or a change Git
/// describes without one.
fn has_changes(diff: &Diff) -> bool {
    diff.hunk_count > 0
        || !diff.patch.is_empty()
        || diff.binary
        || diff.too_large
        || diff.old_missing != diff.new_missing
}

/// A diff the host sent without its patch, because the patch alone was more
/// than the control lane carries: it can only be shown whole.
fn patch_withheld(diff: &Diff) -> bool {
    diff.patch.is_empty() && diff.hunk_count > 0
}

/// The diff's body, off the UI thread: its hunks, or the whole file when asked
/// for, when the patch was withheld, or when there are no hunks to show (a
/// mode change, say).
fn build(diff: &Diff, whole: bool) -> Built {
    if diff.binary {
        return Built::Blocked("Binary changes cannot be displayed or edited as text.");
    }
    if !whole && !patch_withheld(diff) {
        let Ok(patch) = std::str::from_utf8(&diff.patch) else {
            return Built::Blocked(
                "This diff contains non-UTF-8 content and is shown safely as binary.",
            );
        };
        let hunks = model::split_hunks(&diff.display_path, patch);
        if !hunks.is_empty() {
            let mut files = Vec::with_capacity(hunks.len());
            let mut marks = Vec::new();
            for hunk in &hunks {
                let Ok(parsed) = DiffFile::parse(&hunk.patch) else {
                    return Built::Unreadable;
                };
                if let Some(file) = parsed.first() {
                    marks.extend(changed_lines(file.path(), &hunk.patch));
                }
                files.extend(parsed);
            }
            return Built::Hunks(files, marks, hunks);
        }
    }
    match model::shown_diff(diff) {
        Shown::Blocked(detail) => Built::Blocked(detail),
        Shown::Text { original, modified } => {
            let patch = model::whole_file_patch(&diff.display_path, &original, &modified);
            DiffFile::parse(&patch).map_or(Built::Unreadable, |files| {
                let marks = files
                    .first()
                    .map(|file| changed_lines(file.path(), &patch))
                    .unwrap_or_default();
                Built::Files(files, marks)
            })
        }
    }
}

fn same_text(a: &Diff, b: &Diff) -> bool {
    a.display_path == b.display_path
        && a.patch == b.patch
        && a.old_content == b.old_content
        && a.new_content == b.new_content
        && (a.old_missing, a.new_missing, a.binary, a.too_large)
            == (b.old_missing, b.new_missing, b.binary, b.too_large)
}

/// `.git-diff-empty`: a terminal state, with its way out when it has one.
fn empty(
    title: String,
    detail: String,
    retry: Option<AnyElement>,
    palette: &Palette,
) -> AnyElement {
    v_flex()
        .id("git-diff-empty")
        .test_support()
        .role(Role::TabPanel)
        .size_full()
        .items_center()
        .justify_center()
        .gap(ui(3.))
        .p(ui(30.))
        .bg(palette.chrome_bg)
        .text_color(palette.chrome_dim)
        .text_center()
        .child(
            div()
                .size(ui(56.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(ui(12.))
                .border_1()
                .border_color(palette.chrome_border)
                .bg(palette.chrome_raised)
                .font_family(fonts::MONO)
                .text_size(ui(16.))
                .child("±"),
        )
        .child(
            div()
                .mt(ui(7.))
                .text_size(ui(15.))
                .font_weight(FontWeight::BOLD)
                .text_color(palette.chrome_ink)
                .child(title),
        )
        .child(div().child(detail))
        .children(retry)
        .into_any_element()
}

fn target_label(target: Target, palette: &Palette) -> impl IntoElement {
    div()
        .flex_none()
        .px(ui(5.))
        .py(ui(2.))
        .rounded(ui(3.))
        .bg(palette.chrome_raised)
        .text_size(ui(TYPE_LABEL))
        .text_color(if target == Target::Staged {
            palette.term_2
        } else {
            palette.chrome_dim
        })
        .child(model::target_label(target).to_uppercase())
}

/// The file's line: its side, its path, and what the tab can do with it.
fn toolbar(
    target: Target,
    path: String,
    controls: Option<AnyElement>,
    palette: &Palette,
) -> impl IntoElement {
    h_flex()
        .id("git-diff-toolbar")
        .flex_none()
        .h(ui(BAR_HEIGHT))
        .px(ui(8.))
        .gap(ui(8.))
        .items_center()
        .min_w_0()
        .border_b_1()
        .border_color(palette.chrome_hairline)
        .child(target_label(target, palette))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(fonts::MONO)
                .text_size(ui(TYPE_META))
                .text_color(palette.chrome_dim)
                .child(path),
        )
        .children(controls)
}

/// The lines of a hunk under its `@@` line: what it changes, wherever it sits.
fn hunk_lines(hunk: &Hunk) -> &str {
    hunk.patch
        .split_once(hunk.header.as_str())
        .map_or(hunk.patch.as_str(), |(_, lines)| lines)
}

/// `+3 −1`, in the colours of the lines they count. A zero is left out, as in
/// the panel.
fn counts(added: usize, removed: usize, palette: &Palette) -> impl IntoElement {
    h_flex()
        .flex_none()
        .gap(ui(4.))
        .text_size(ui(TYPE_META))
        .when(added > 0, |this| {
            this.child(div().text_color(palette.term_2).child(format!("+{added}")))
        })
        .when(removed > 0, |this| {
            this.child(
                div()
                    .text_color(palette.danger)
                    .child(format!("−{removed}")),
            )
        })
}

fn banner(id: &'static str, role: Role, text: String, palette: &Palette) -> impl IntoElement {
    div()
        .id(id)
        .role(role)
        .px(ui(8.))
        .py(ui(5.))
        .border_b_1()
        .border_color(palette.danger)
        .bg(palette.danger_wash)
        .text_color(palette.danger_ink)
        .text_size(ui(11.))
        .child(text)
}

/// What a hunk's header offers, decided once per frame.
#[derive(Clone, Copy)]
struct HunkActions {
    /// The hunks of a staged diff are unstaged; any other side's are staged.
    staged: bool,
    enabled: bool,
}

/// A hunk's header inside Kit's section shell, beside its collapse control:
/// Git's `@@` line, the hunk's counts, and Stage and Discard where they apply.
fn hunk_header(
    hunk: &Hunk,
    index: u32,
    actions: Option<HunkActions>,
    view: WeakEntity<GitDiffView>,
    palette: &Palette,
) -> AnyElement {
    let id = |name: &str| SharedString::from(format!("git-hunk-{index}-{name}"));
    h_flex()
        .w_full()
        .min_w_0()
        .gap(ui(8.))
        .items_center()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(fonts::MONO)
                .text_size(ui(TYPE_META))
                .text_color(palette.chrome_faint)
                .child(hunk.header.clone()),
        )
        .child(counts(hunk.added, hunk.removed, palette))
        .when_some(actions, |this, actions| {
            let staged = actions.staged;
            let apply = view.clone();
            this.child(
                h_flex()
                    .id(id("actions"))
                    .role(Role::Toolbar)
                    .aria_label(format!("Hunk {} actions", index + 1))
                    .flex_none()
                    .gap(ui(4.))
                    .child(
                        toolbar_button(
                            Button::new(id("apply")).label(if staged {
                                "Unstage"
                            } else {
                                "Stage"
                            }),
                            Tone::Plain,
                            !actions.enabled,
                            palette,
                        )
                        .disabled(!actions.enabled)
                        .on_click(move |_, _, cx| {
                            let kind = if staged {
                                MutationKind::UnstageHunk
                            } else {
                                MutationKind::StageHunk
                            };
                            apply
                                .update(cx, |this, cx| this.mutate(kind, index, cx))
                                .ok();
                        }),
                    )
                    .child(
                        toolbar_button(
                            Button::new(id("discard")).label("Discard…"),
                            Tone::Danger,
                            !actions.enabled,
                            palette,
                        )
                        .disabled(!actions.enabled)
                        .on_click(move |_, window, cx| {
                            view.update(cx, |this, cx| this.request_discard(index, window, cx))
                                .ok();
                        }),
                    ),
            )
        })
        .into_any_element()
}

impl Render for GitDiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        // The diff a read answered is built and drawn in this frame.
        if self.body.is_some() {
            crate::perf::after_next_frame(self.paint.paintable(self.serial), window);
        }
        let title = self.tab.title.clone();
        let retry = |cx: &mut Context<Self>| {
            Button::new("git-diff-retry")
                .outline()
                .xsmall()
                .mt(ui(8.))
                .label("Retry")
                .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
                .into_any_element()
        };
        if self.scope(cx).is_none() {
            return empty(
                title,
                "Reconnect to reopen this Git diff.".into(),
                None,
                &palette,
            );
        }
        let (Some(_), Some(_), Some(target)) = (
            &self.tab.git_repository_id,
            &self.tab.git_path,
            self.tab.git_target,
        ) else {
            return empty(
                title,
                "This saved Git tab is missing its repository identity.".into(),
                None,
                &palette,
            );
        };
        let no_changes = if target == Target::Base {
            "This file has no changes against the default branch.".to_owned()
        } else {
            format!(
                "This file no longer has {} changes.",
                model::target_name(target)
            )
        };
        let error = self.error.clone().or(self.shared_error.clone());
        // An error outranks a spinner: a surface claiming to load while it
        // holds a failure has no way out.
        let Some(diff) = self.diff.clone() else {
            if let Some(error) = error {
                return empty(title, error, Some(retry(cx)), &palette);
            }
            if self.loading {
                // The frame the diff will appear in, so a read in flight does
                // not change the tab's shape on its way to the content.
                let path = model::tab_display_path(&self.tab).to_owned();
                return v_flex()
                    .id("git-diff-surface")
                    .test_support()
                    .role(Role::TabPanel)
                    .aria_label(format!("{} diff {path}", model::target_label(target)))
                    .size_full()
                    .bg(palette.chrome_bg)
                    .child(toolbar(target, path, None, &palette))
                    .child(
                        div()
                            .id("git-diff-loading")
                            .role(Role::Status)
                            .p(ui(12.))
                            .text_size(ui(TYPE_META))
                            .text_color(palette.chrome_faint)
                            .when(!self.loading_shown, |this| this.invisible())
                            .child("Loading…"),
                    )
                    .into_any_element();
            }
            return empty(title, no_changes, Some(retry(cx)), &palette);
        };
        let Some(status) = self.status.clone() else {
            return empty(title, no_changes, Some(retry(cx)), &palette);
        };
        // The status speaks for uncommitted changes only.
        let entry = (diff.target != Target::Base)
            .then(|| status.entry(&diff.path))
            .flatten();
        let block = entry.and_then(|entry| {
            if entry.submodule {
                Some("Submodule pointer changes are read-only in v1.")
            } else if entry.conflicted {
                Some("Resolve conflicts in the terminal before using Git actions.")
            } else {
                None
            }
        });
        let path_change = entry.is_some_and(|entry| {
            [entry.index_kind, entry.worktree_kind]
                .iter()
                .any(|kind| matches!(kind, model::ChangeKind::Renamed | model::ChangeKind::Copied))
        });
        let can_mutate = self.can_write(cx) && status.authoritative && block.is_none();
        let hunk_actions = diff.target != Target::Base
            && !diff.binary
            && !diff.too_large
            && block.is_none()
            && !path_change
            && diff.hunk_count > 0;
        let busy = self.busy;
        let withheld = patch_withheld(&diff);
        let showing_hunks = !self.hunks.is_empty();
        let all_collapsed = self.all_collapsed(cx);
        let (added, removed) = self.hunks.iter().fold((0, 0), |(added, removed), hunk| {
            (added + hunk.added, removed + hunk.removed)
        });
        let controls =
            h_flex()
                .flex_none()
                .gap(ui(6.))
                .items_center()
                .when(showing_hunks, |this| {
                    this.child(counts(added, removed, &palette)).child(
                        toolbar_button(
                            Button::new("git-diff-collapse-all").label(if all_collapsed {
                                "Expand all"
                            } else {
                                "Collapse all"
                            }),
                            Tone::Plain,
                            false,
                            &palette,
                        )
                        .tooltip(if cfg!(target_os = "macos") {
                            "Collapse all ⌥⌘[ · Expand all ⌥⌘]"
                        } else {
                            "Collapse all Ctrl+Alt+[ · Expand all Ctrl+Alt+]"
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.set_all_collapsed(!all_collapsed, cx)
                        })),
                    )
                })
                // A patch too large to send can only be shown whole.
                .when(!withheld && !diff.binary, |this| {
                    this.child(
                        toolbar_button(
                            Button::new("git-diff-whole").label(if self.whole {
                                "Show hunks"
                            } else {
                                "Show whole file"
                            }),
                            Tone::Plain,
                            false,
                            &palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_whole(cx))),
                    )
                })
                .into_any_element();
        let body: AnyElement = match &self.body {
            Some(Body::Blocked(detail)) => {
                empty(diff.display_path.clone(), (*detail).into(), None, &palette)
            }
            Some(Body::Drawn(content)) => {
                let state = content.clone();
                let width = rems_to_px(window);
                let laid_out = self.width.clone();
                let settled = self.settled.clone();
                let hidden = settled.get() < SETTLE_FRAMES;
                let marks = self.marks.clone();
                // Switched here, from the width the last frame drew at, and not
                // while painting: the list below counts the rows of the mode it
                // is built in, so a switch after that left it indexing rows of
                // the other mode, and the window panicked on narrowing.
                if let Some(last) = laid_out.get() {
                    let mode = if last >= width {
                        DiffMode::Split
                    } else {
                        DiffMode::Unified
                    };
                    if state.read(cx).mode() != mode {
                        state.update(cx, |state, cx| state.set_mode(mode, cx));
                        window.request_animation_frame();
                    }
                }
                let element = DiffElement::new(content)
                    // Its first frames hold an empty bordered frame, before the
                    // rows are measured.
                    .when(hidden, |diff| diff.opacity(0.))
                    .change_background(false)
                    .decorations(marks.iter().cloned())
                    // A section is one hunk, or the whole file as one: Git's
                    // `@@` line is the hunk's header, and the whole file's
                    // says nothing Monaco showed.
                    .hunk_separator(DiffHunkSeparator::Simple)
                    .line_number(true)
                    .syntax_highlight(true)
                    .soft_wrap(true)
                    .size_full();
                let element = if showing_hunks {
                    let hunks = self.hunks.clone();
                    let view = cx.entity().downgrade();
                    let actions = hunk_actions.then_some(HunkActions {
                        staged: diff.target == Target::Staged,
                        enabled: !busy && can_mutate,
                    });
                    let header_palette = palette;
                    element.render_header(move |file, _, _| {
                        // A hunk's key starts with its number, from 1
                        // (`<n>/<name>`).
                        let path: &str = file.path().as_ref();
                        let index = path
                            .split_once('/')
                            .and_then(|(n, _)| n.parse::<usize>().ok()?.checked_sub(1))
                            .filter(|&n| hunks.get(n).is_some_and(|hunk| hunk.key == path));
                        match index {
                            Some(index) => hunk_header(
                                &hunks[index],
                                index as u32,
                                actions,
                                view.clone(),
                                &header_palette,
                            ),
                            None => div().into_any_element(),
                        }
                    })
                } else {
                    element.header_visible(false)
                };
                div()
                    .relative()
                    .size_full()
                    .child(
                        canvas(
                            move |bounds: Bounds<Pixels>, window, _| {
                                // The diff wraps to the width it last drew at, so a
                                // change needs one more frame, as Monaco's automatic
                                // layout gave it. A notify while drawing schedules
                                // nothing; this does.
                                if laid_out.replace(Some(bounds.size.width))
                                    != Some(bounds.size.width)
                                {
                                    window.request_animation_frame();
                                }
                                if settled.get() < SETTLE_FRAMES {
                                    settled.set(settled.get() + 1);
                                    window.request_animation_frame();
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(element)
                    .into_any_element()
            }
            None => div()
                .id("git-diff-building")
                .role(Role::Status)
                .p(ui(12.))
                .text_size(ui(TYPE_META))
                .text_color(palette.chrome_faint)
                .when(!self.building_shown, |this| this.invisible())
                .child("Loading…")
                .into_any_element(),
        };
        // The whole file keeps today's bar of numbered hunk actions; the hunks
        // carry theirs in their headers.
        let bar = hunk_actions && !showing_hunks && matches!(self.body, Some(Body::Drawn(_)));
        v_flex()
            .id("git-diff-surface")
            .test_support()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &CollapseHunks, _, cx| this.set_all_collapsed(true, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ExpandHunks, _, cx| this.set_all_collapsed(false, cx)),
            )
            // The chords reach the tab from a press anywhere in it; a press
            // the diff's own text took focus for keeps it.
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if !this.focus.contains_focused(window, cx) {
                        window.focus(&this.focus, cx);
                    }
                }),
            )
            .role(Role::TabPanel)
            .aria_label(format!(
                "{} diff {}",
                model::target_label(diff.target),
                diff.display_path
            ))
            .size_full()
            .min_w_0()
            .min_h_0()
            .bg(palette.chrome_bg)
            .child(toolbar(
                diff.target,
                diff.display_path.clone(),
                Some(controls),
                &palette,
            ))
            .when_some(block, |this, block| {
                this.child(banner("git-diff-block", Role::Note, block.into(), &palette))
            })
            .when(withheld, |this| {
                this.child(
                    div()
                        .id("git-diff-whole-only")
                        .role(Role::Note)
                        .px(ui(8.))
                        .py(ui(5.))
                        .border_b_1()
                        .border_color(palette.chrome_hairline)
                        .text_size(ui(TYPE_META))
                        .text_color(palette.chrome_dim)
                        .child(
                            "This diff is too large to show as hunks, so the whole file is shown.",
                        ),
                )
            })
            .when_some(error, |this, error| {
                this.child(SurfaceError::new("git-diff-error", error))
            })
            .child(div().flex_1().min_h_0().min_w_0().child(body))
            .when(bar, |this| {
                let staged = diff.target == Target::Staged;
                this.child(
                    h_flex()
                        .id("git-hunk-actions")
                        .role(Role::Toolbar)
                        .aria_label("Complete hunk actions")
                        .flex_none()
                        .gap(ui(6.))
                        .px(ui(8.))
                        .py(ui(5.))
                        .overflow_x_scroll()
                        .border_t_1()
                        .border_color(palette.chrome_hairline)
                        .bg(palette.chrome_bg)
                        .children((0..diff.hunk_count).map(|index| {
                            let id =
                                |name: &str| SharedString::from(format!("git-hunk-{index}-{name}"));
                            h_flex()
                                .flex_none()
                                .items_center()
                                .gap(ui(4.))
                                .pl(ui(6.))
                                .border_l_1()
                                .border_color(palette.chrome_hairline)
                                .child(
                                    div()
                                        .text_size(ui(TYPE_META))
                                        .text_color(palette.chrome_dim)
                                        .child(format!("Hunk {}", index + 1)),
                                )
                                .child(
                                    toolbar_button(
                                        Button::new(id("apply")).label(if staged {
                                            "Unstage"
                                        } else {
                                            "Stage"
                                        }),
                                        Tone::Plain,
                                        busy || !can_mutate,
                                        &palette,
                                    )
                                    .disabled(busy || !can_mutate)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            let kind = if staged {
                                                MutationKind::UnstageHunk
                                            } else {
                                                MutationKind::StageHunk
                                            };
                                            this.mutate(kind, index, cx)
                                        },
                                    )),
                                )
                                .child(
                                    toolbar_button(
                                        Button::new(id("discard")).label("Discard…"),
                                        Tone::Danger,
                                        busy || !can_mutate,
                                        &palette,
                                    )
                                    .disabled(busy || !can_mutate)
                                    .on_click(cx.listener(
                                        move |this, _, window, cx| {
                                            this.request_discard(index, window, cx)
                                        },
                                    )),
                                )
                        })),
                )
            })
            .into_any_element()
    }
}

/// The side-by-side breakpoint in this window's pixels, following UI scale.
fn rems_to_px(window: &Window) -> Pixels {
    ui(SIDE_BY_SIDE_WIDTH).to_pixels(window.rem_size())
}

#[cfg(test)]
impl GitDiffView {
    /// Whether a diff is drawn, and the failure shown with it or instead.
    pub(super) fn shown(&self) -> (bool, Option<String>) {
        (
            self.diff.is_some(),
            self.error.clone().or(self.shared_error.clone()),
        )
    }

    pub(super) fn loading(&self) -> bool {
        self.loading
    }

    /// Whether the loading line is visible yet, not only laid out.
    pub(super) fn loading_shown(&self) -> bool {
        self.loading_shown
    }

    pub(super) fn stage_hunk(&mut self, index: u32, cx: &mut Context<Self>) {
        self.mutate(MutationKind::StageHunk, index, cx);
    }

    /// The `@@` lines of the hunks drawn; empty while the whole file is.
    pub(super) fn hunk_headers(&self) -> Vec<String> {
        self.hunks.iter().map(|hunk| hunk.header.clone()).collect()
    }

    pub(super) fn show_whole_file(&mut self, cx: &mut Context<Self>) {
        self.toggle_whole(cx);
    }

    pub(super) fn collapse_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        if let (Some(Body::Drawn(state)), Some(hunk)) = (&self.body, self.hunks.get(index)) {
            state.update(cx, |state, cx| {
                state.set_file_collapsed(&hunk.key, true, cx)
            });
        }
    }

    /// Which drawn hunks are collapsed, in order.
    pub(super) fn collapsed_hunks(&self, cx: &App) -> Vec<bool> {
        let Some(Body::Drawn(state)) = &self.body else {
            return Vec::new();
        };
        let state = state.read(cx);
        self.hunks
            .iter()
            .map(|hunk| state.is_file_collapsed(&hunk.key))
            .collect()
    }

    pub(super) fn collapse_all(&mut self, collapsed: bool, cx: &mut Context<Self>) -> bool {
        self.set_all_collapsed(collapsed, cx);
        self.all_collapsed(cx)
    }
}

#[cfg(test)]
mod line_tests {
    use super::*;

    #[test]
    fn removed_and_added_lines_are_marked_on_their_own_side() {
        let patch = "--- a/f\n+++ b/f\n@@ -1,4 +1,4 @@\n a\n-b\n-c\n+B\n d\n";
        let marks = changed_lines(&"f".into(), patch);
        let runs: Vec<_> = marks
            .iter()
            .map(|mark| {
                let range = mark.range();
                (range.side(), range.start(), range.end())
            })
            .collect();
        assert_eq!(
            runs,
            [(DiffSide::Original, 2, 3), (DiffSide::Modified, 2, 2)]
        );
    }
}
