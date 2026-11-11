//! The Source Control panel and its commit form. Port of `GitSidebar.tsx`,
//! `GitCommitForm.tsx` and `useWorkspaceGit.ts`.
//!
//! The panel holds no watch of its own: it acquires the shared observation of
//! the active pane's repository and draws whatever that has. Stage and discard
//! are hover buttons on the row, as in VS Code's Source Control list; the
//! right-click menu carries the same actions with words, and the palette's row
//! commands act on the last row focused.

use std::{collections::HashMap, rc::Rc};

use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FontWeight,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, Role, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window,
    assets::IconName,
    base::TestSupportExt,
    component::{
        Disableable, Icon, Sizable,
        button::{Button, ButtonVariants, DropdownButton},
        h_flex,
        input::{Enter, Textarea, TextareaState},
        menu::{ContextMenuExt, PopupMenu},
        v_flex,
    },
    div,
    prelude::FluentBuilder,
};

use crate::ui::icons::Glyph;
use crate::{
    commands::{self, RowCommandSource, RowSurface, dialogs, registry::CommandId},
    context_menu::{Entry, Item, fill},
    fonts,
    terminal::integration::transfers::InternalPathDrag,
    theme::{
        self, Palette,
        metrics::{RADIUS_CONTROL, TYPE_LABEL, TYPE_META, TYPE_SECONDARY, ui},
    },
    ui::surface_error::SurfaceError,
};

use super::{
    client,
    model::{
        self, ChangeKind, CommandResult, GitScope, Live, Mutation, MutationKind, Outcome, RootInfo,
        StatusEntry, StatusSnapshot, Target, Verb,
    },
    store::{GitRepositories, RepositoryState},
};

/// Rows a group draws before "Show more", and how many each press adds.
const FIRST_ROWS: usize = 200;
const MORE_ROWS: usize = 500;

/// What the panel observes: the active pane's root and the live link it can
/// be reached over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PanelContext {
    pub root: Option<RootInfo>,
    pub link: Live,
    pub can_mutate: bool,
    /// The active root's scope, when the link can carry a request.
    pub scope: Option<GitScope>,
}

impl PanelContext {
    pub fn new(root: Option<RootInfo>, link: Live, can_mutate: bool) -> Self {
        let scope = root.as_ref().and_then(|root| model::git_scope(&link, root));
        Self {
            root,
            link,
            can_mutate,
            scope,
        }
    }
}

pub enum PanelEvent {
    /// For the app's status channel.
    Message(String),
    OpenDiff {
        entry: StatusEntry,
        target: Target,
        status: Rc<StatusSnapshot>,
        /// A single click's preview, which a double-click promotes.
        preview: bool,
    },
}

#[derive(Default)]
struct CommitForm {
    input: Option<Entity<TextareaState>>,
    output: Option<(CommandResult, Verb)>,
    error: Option<String>,
    /// What has already settled: "Committed." survives a push that then failed.
    note: Option<String>,
    /// What is happening right now. Always cleared when the work ends.
    progress: Option<String>,
    busy: bool,
    /// The message was committed; the field empties on the next draw.
    clear: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Work {
    Commit,
    CommitAndPush,
    Push,
}

/// A discard the user is being asked to confirm, with what it was decided on.
#[derive(Clone)]
pub(super) struct PendingDiscard {
    entry: StatusEntry,
    target: Target,
    status: Rc<StatusSnapshot>,
    key: String,
}

pub struct GitPanel {
    store: Entity<GitRepositories>,
    context: PanelContext,
    key: Option<String>,
    /// The row the palette means: the last one focused or right-clicked, by
    /// path, since a refresh replaces every entry.
    focused: Option<(String, Target)>,
    /// Whether the panel is on screen: only then do its rows answer the palette.
    shown: bool,
    /// What is being applied to each row, by path, while the host decides.
    pending: HashMap<String, &'static str>,
    limits: HashMap<&'static str, usize>,
    form: CommitForm,
    _store: Subscription,
}

impl EventEmitter<PanelEvent> for GitPanel {}

impl GitPanel {
    pub fn new(store: Entity<GitRepositories>, cx: &mut Context<Self>) -> Self {
        let observed = cx.observe(&store, |this, _, cx| {
            this.publish_rows(cx);
            cx.notify();
        });
        Self {
            store,
            context: PanelContext::default(),
            key: None,
            focused: None,
            shown: false,
            pending: HashMap::new(),
            limits: HashMap::new(),
            form: CommitForm::default(),
            _store: observed,
        }
    }

    /// Follows the active pane's root. A different repository scope releases
    /// the old observation and acquires the new one.
    pub fn set_context(&mut self, context: PanelContext, cx: &mut Context<Self>) {
        if context == self.context {
            return;
        }
        let observable = context
            .scope
            .clone()
            .filter(|_| context.root.as_ref().is_some_and(|root| root.git_worktree));
        let wanted = observable.as_ref().map(GitScope::key);
        if wanted != self.key {
            let store = self.store.clone();
            store.update(cx, |store, cx| {
                if let Some(old) = self.key.take() {
                    store.release(&old, cx);
                }
                self.key = observable.map(|scope| store.acquire(&scope, cx));
            });
            self.limits.clear();
            // Tauri unmounted the form with its repository: a message, error
            // or output never carries over to another one. Work in flight
            // keeps the form busy until it ends; it reports elsewhere.
            self.form = CommitForm {
                busy: self.form.busy,
                ..CommitForm::default()
            };
        }
        self.context = context;
        self.publish_rows(cx);
        cx.notify();
    }

    pub fn context(&self) -> &PanelContext {
        &self.context
    }

    pub fn store(&self) -> &Entity<GitRepositories> {
        &self.store
    }

    /// The active repository's status, for whatever else follows it.
    pub fn status(&self, cx: &App) -> Option<Rc<StatusSnapshot>> {
        self.state(cx).status
    }

    fn state(&self, cx: &App) -> RepositoryState {
        self.key
            .as_ref()
            .map_or_else(RepositoryState::default, |key| {
                self.store.read(cx).state(key)
            })
    }

    /// Whether this repository can be written to at all; a row decides the rest.
    fn unavailable(&self, state: &RepositoryState) -> bool {
        !self.context.can_mutate
            || self.context.scope.is_none()
            || self.context.root.is_none()
            || self.key.is_none()
            || !state
                .status
                .as_ref()
                .is_some_and(|status| status.authoritative)
    }

    fn message(&mut self, message: String, cx: &mut Context<Self>) {
        cx.emit(PanelEvent::Message(message));
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.key.clone() {
            let refreshing = self.store.update(cx, |store, cx| store.refresh(&key, cx));
            cx.background_executor().spawn(refreshing).detach();
        }
    }

    fn open(&mut self, entry: &StatusEntry, target: Target, preview: bool, cx: &mut Context<Self>) {
        if let Some(status) = self.state(cx).status {
            cx.emit(PanelEvent::OpenDiff {
                entry: entry.clone(),
                target,
                status,
                preview,
            });
        }
    }

    pub(super) fn focus_row(
        &mut self,
        entry: &StatusEntry,
        target: Target,
        cx: &mut Context<Self>,
    ) {
        let row = (entry.path.clone(), target);
        // Re-selecting the same row is not a state change.
        if self.focused.as_ref() != Some(&row) {
            self.focused = Some(row);
            self.publish_rows(cx);
        }
    }

    /// Stage, unstage or discard one file. A refreshed row shows success, so
    /// only a warning, an uncertain result, or a result for a repository no
    /// longer shown becomes a notice.
    pub(super) fn mutate_file(
        &mut self,
        entry: &StatusEntry,
        target: Target,
        kind: MutationKind,
        captured: Option<Rc<StatusSnapshot>>,
        cx: &mut Context<Self>,
    ) {
        let state = self.state(cx);
        let (Some(key), Some(scope)) = (self.key.clone(), self.context.scope.clone()) else {
            return;
        };
        let Some(status) = captured.or(state.status.clone()) else {
            return;
        };
        if self.unavailable(&state) {
            return;
        }
        let mut mutation = Mutation {
            kind,
            path: entry.path.clone(),
            original_path: entry.original_path.clone(),
            target,
            expected_status_generation: status.generation,
            expected_source_generation: status.source_generation.clone(),
            hunk_index: None,
            confirmation_token: None,
        };
        let repository_id = status.repository.id.clone();
        let (path, subject) = (
            entry.path.clone(),
            format!("{} {}", kind.label(), entry.display_path),
        );
        self.pending.insert(path.clone(), kind.pending_label());
        cx.notify();
        let transport = self.store.read(cx).transport();
        cx.spawn(async move |this, cx| {
            let result = async {
                if kind.is_discard() {
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
            this.update(cx, |this, cx| {
                this.pending.remove(&path);
                match result {
                    Ok(result) => {
                        this.accept(&key, &result, cx);
                        if this.key.as_deref() != Some(&key) || model::needs_notice(&result) {
                            this.message(model::result_message(&result, &subject), cx);
                        }
                    }
                    Err(error) => this.message(error, cx),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn accept(&mut self, key: &str, result: &CommandResult, cx: &mut Context<Self>) {
        if let Some(status) = result.status.clone() {
            self.store
                .update(cx, |store, cx| store.accept(key, status, cx));
        }
    }

    /// What a discard is decided on, while it can be asked for at all.
    pub(super) fn pending_discard(
        &self,
        entry: &StatusEntry,
        target: Target,
        cx: &App,
    ) -> Option<PendingDiscard> {
        let state = self.state(cx);
        if self.unavailable(&state) {
            return None;
        }
        Some(PendingDiscard {
            entry: entry.clone(),
            target,
            status: state.status?,
            key: self.key.clone()?,
        })
    }

    /// Asks before a discard, through the same dialog from every way in.
    fn request_discard(
        &mut self,
        entry: &StatusEntry,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pending) = self.pending_discard(entry, target, cx) else {
            return;
        };
        let this = cx.entity().downgrade();
        dialogs::open_confirmation(
            dialogs::Confirmation {
                title: if entry.untracked {
                    "Delete untracked file?"
                } else {
                    "Discard file changes?"
                }
                .into(),
                detail: format!(
                    "Discard {}? This cannot be undone by the app.",
                    entry.display_path
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

    /// The dialog may have stayed open across a reconnect or a pane switch;
    /// the discard is then for a repository connection that is gone.
    pub(super) fn confirm_discard(&mut self, pending: PendingDiscard, cx: &mut Context<Self>) {
        let same = self.key.as_deref() == Some(&pending.key)
            && self
                .state(cx)
                .status
                .as_ref()
                .is_some_and(|status| status.repository.id == pending.status.repository.id);
        if !same {
            return self.message(
                "Discard was cancelled because the repository connection changed.".into(),
                cx,
            );
        }
        self.mutate_file(
            &pending.entry,
            pending.target,
            MutationKind::DiscardFile,
            Some(pending.status),
            cx,
        );
    }

    fn row_action(
        &mut self,
        entry: &StatusEntry,
        target: Target,
        command: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match command {
            "git.openDiff" => self.open(entry, target, false, cx),
            "git.stage" => {
                self.mutate_file(entry, Target::Unstaged, MutationKind::StageFile, None, cx)
            }
            "git.unstage" => {
                self.mutate_file(entry, Target::Staged, MutationKind::UnstageFile, None, cx)
            }
            "git.discard" => self.request_discard(entry, target, window, cx),
            _ => {}
        }
    }

    /// The row commands the palette and shortcuts reach: the last row focused.
    /// A hidden panel has no selected row, as an unmounted sidebar had none.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.shown = shown;
        if !shown {
            self.focused = None;
        }
        self.publish_rows(cx);
    }

    fn publish_rows(&self, cx: &mut Context<Self>) {
        let state = self.state(cx);
        let focused = self
            .focused
            .as_ref()
            .filter(|_| self.shown)
            .and_then(|(path, target)| {
                let entry = state.status.as_ref()?.entry(path)?.clone();
                Some((entry, *target))
            });
        let source = focused.map(|(entry, target)| {
            let mut available: Vec<CommandId> = vec!["git.openDiff"];
            if !self.unavailable(&state) && !entry.conflicted && !entry.submodule {
                available.push(match target {
                    Target::Staged => "git.unstage",
                    Target::Unstaged => "git.stage",
                });
                available.push("git.discard");
            }
            let this = cx.entity().downgrade();
            RowCommandSource {
                available,
                run: Rc::new(move |command, window, cx| {
                    this.update(cx, |this, cx| {
                        this.row_action(&entry, target, command, window, cx)
                    })
                    .ok();
                }),
            }
        });
        commands::publish_row_commands(RowSurface::Git, source, cx);
    }

    // The commit form.

    fn input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<TextareaState> {
        if let Some(input) = &self.form.input {
            return input.clone();
        }
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Commit message…")
                .auto_grow(2, 8)
        });
        // Typing redraws only the field: nothing else here reads the message
        // until it is sent.
        self.form.input = Some(input.clone());
        input
    }

    pub(super) fn message_text(&self, cx: &App) -> String {
        self.form
            .input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    fn staged_count(&self, cx: &App) -> usize {
        self.state(cx).status.as_ref().map_or(0, |status| {
            model::group_entries(&status.entries).staged.len()
        })
    }

    pub(super) fn run(&mut self, work: Work, window: &mut Window, cx: &mut Context<Self>) {
        if self.form.busy {
            return;
        }
        self.form.error = None;
        self.form.output = None;
        self.form.note = None;
        self.form.progress = None;
        let message = self.message_text(cx);
        let commits = work != Work::Push;
        if commits && message.trim().is_empty() {
            self.form.error = Some("Enter a commit message.".into());
            cx.notify();
            return;
        }
        let state = self.state(cx);
        let (Some(key), Some(scope), Some(status)) = (
            self.key.clone(),
            self.context.scope.clone(),
            state.status.clone(),
        ) else {
            if !commits {
                self.form.error = Some("Git is not available right now.".into());
            }
            cx.notify();
            return;
        };
        if self.unavailable(&state) {
            if !commits {
                self.form.error = Some("Git is not available right now.".into());
            }
            cx.notify();
            return;
        }
        self.form.busy = true;
        if !commits {
            self.form.progress = Some("Pushing…".into());
        }
        cx.notify();
        let transport = self.store.read(cx).transport();
        let repository_id = status.repository.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let mut committed = false;
            if commits {
                let result = client::commit(
                    transport.clone(),
                    scope.clone(),
                    repository_id.clone(),
                    status.generation,
                    message,
                )
                .await;
                committed = this
                    .update(cx, |this, cx| this.settled_commit(&key, result, work, cx))
                    .unwrap_or(false);
            }
            if work == Work::Push || committed {
                // The push is a mutation of its own and passes the guard its
                // button has, now: a chained commit may have left the status
                // resynchronizing, or the panel on another repository. It says
                // which status it follows, which the commit has moved.
                let generation = this
                    .update(cx, |this, cx| {
                        let state = this.state(cx);
                        let ready =
                            this.key.as_deref() == Some(key.as_str()) && !this.unavailable(&state);
                        let generation = state.status.filter(|_| ready).map(|s| s.generation);
                        if generation.is_none() {
                            let error = "Git is not available right now.".to_owned();
                            if this.key.as_deref() == Some(key.as_str()) {
                                this.form.error = Some(error);
                            } else {
                                this.message(error, cx);
                            }
                        }
                        generation
                    })
                    .ok()
                    .flatten();
                let Some(generation) = generation else {
                    this.update(cx, |this, cx| {
                        this.form.busy = false;
                        this.form.progress = None;
                        cx.notify();
                    })
                    .ok();
                    return;
                };
                let result = client::push(transport, scope, repository_id, generation).await;
                this.update(cx, |this, cx| {
                    this.settled_push(&key, result, committed, cx)
                })
                .ok();
            }
            this.update(cx, |this, cx| {
                // Whatever happened, nothing is in flight any more.
                this.form.busy = false;
                this.form.progress = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Whether the commit happened, which is the only reason to then push.
    fn settled_commit(
        &mut self,
        key: &str,
        result: Result<CommandResult, String>,
        work: Work,
        cx: &mut Context<Self>,
    ) -> bool {
        let result = match result {
            Ok(result) => result,
            Err(error) if self.key.as_deref() == Some(key) => {
                self.form.error = Some(error);
                return false;
            }
            Err(error) => {
                self.message(error, cx);
                return false;
            }
        };
        self.accept(key, &result, cx);
        let applied = result.applied();
        if self.key.as_deref() != Some(key) {
            // The form that asked is no longer this repository's.
            self.message(model::result_message(&result, "Commit created."), cx);
            return applied && work == Work::CommitAndPush;
        }
        match result.outcome {
            Outcome::Applied => {
                // The field is cleared where a window is at hand: the next draw.
                self.form.clear = true;
            }
            Outcome::PartialOrUnknown => {
                self.form.error =
                    Some("Commit outcome is uncertain; inspect HEAD before retrying.".into());
            }
            _ => self.form.error = Some("Git did not create a commit.".into()),
        }
        self.form.output = Some((result, Verb::Commit));
        if applied && work == Work::CommitAndPush {
            self.form.note = Some("Committed.".into());
            self.form.progress = Some("Pushing…".into());
        }
        cx.notify();
        applied && work == Work::CommitAndPush
    }

    /// A push is never reported as having worked on the strength of the
    /// request returning: the host says whether the remote accepted it.
    fn settled_push(
        &mut self,
        key: &str,
        result: Result<CommandResult, String>,
        after_commit: bool,
        cx: &mut Context<Self>,
    ) {
        let result = match result {
            Ok(result) => result,
            Err(error) if self.key.as_deref() == Some(key) => {
                self.form.error = Some(error);
                return;
            }
            Err(error) => return self.message(error, cx),
        };
        self.accept(key, &result, cx);
        if self.key.as_deref() != Some(key) {
            let pushed = model::pushed_to(&result);
            return self.message(model::result_message(&result, &pushed), cx);
        }
        match result.outcome {
            Outcome::Applied => {
                let already = if after_commit { "Committed. " } else { "" };
                self.form.note = Some(format!("{already}{}", model::pushed_to(&result)));
            }
            Outcome::PartialOrUnknown => {
                self.form.error =
                    Some("Push outcome is unknown; check the remote before retrying.".into());
            }
            _ => self.form.error = Some("The remote did not accept the push.".into()),
        }
        self.form.output = Some((result, Verb::Push));
        cx.notify();
    }
}

fn quiet(text: impl Into<SharedString>, palette: &Palette) -> gpui_kit::Div {
    div()
        .px(ui(12.))
        .py(ui(10.))
        .text_size(ui(TYPE_SECONDARY))
        .text_color(palette.chrome_faint)
        .child(text.into())
}

/// `.surface-error` with no disclosure: a state the app words itself.
fn alert_line(id: &'static str, text: String, palette: &Palette) -> impl IntoElement {
    div()
        .id(id)
        .role(Role::Alert)
        .px(ui(12.))
        .py(ui(5.))
        .bg(palette.danger_wash)
        .text_color(palette.danger_ink)
        .text_size(ui(TYPE_SECONDARY))
        .child(text)
}

fn state_color(kind: ChangeKind, conflicted: bool, palette: &Palette) -> gpui_kit::Rgba {
    if conflicted {
        return palette.chrome_faint;
    }
    match kind {
        ChangeKind::Modified => palette.state_working,
        ChangeKind::Added | ChangeKind::Copied => palette.term_2,
        ChangeKind::Deleted => palette.danger,
        ChangeKind::Untracked => palette.term_4,
        ChangeKind::Renamed | ChangeKind::TypeChanged => palette.term_5,
        _ => palette.chrome_faint,
    }
}

/// What a dragged row looks like under the pointer.
struct PathGhost(SharedString);

impl Render for PathGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        div()
            .px(ui(8.))
            .py(ui(3.))
            .rounded(ui(RADIUS_CONTROL))
            .bg(palette.chrome_raised)
            .border_1()
            .border_color(palette.chrome_border)
            .font_family(fonts::MONO)
            .text_size(ui(12.))
            .text_color(palette.chrome_ink)
            .child(self.0.clone())
    }
}

impl GitPanel {
    fn render_empty(
        &self,
        detail: String,
        retry: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        v_flex()
            .id("git-panel")
            .role(Role::Region)
            .aria_label("Source Control")
            .size_full()
            .child(quiet(detail, palette).when(retry, |this| {
                this.child(
                    Button::new("git-retry")
                        .ghost()
                        .xsmall()
                        .compact()
                        .label("Retry")
                        .text_color(palette.accent)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
            }))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_group(
        &self,
        id: &'static str,
        title: &'static str,
        entries: &[&StatusEntry],
        target: Target,
        mutable: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if entries.is_empty() {
            return None;
        }
        let limit = self.limits.get(id).copied().unwrap_or(FIRST_ROWS);
        let shown = &entries[..entries.len().min(limit)];
        let remaining = entries.len() - shown.len();
        let host = self
            .context
            .scope
            .as_ref()
            .map(|scope| (scope.host_profile_id.clone(), scope.server_identity.clone()));
        let rows = shown
            .iter()
            .map(|entry| self.render_row(entry, target, mutable, host.clone(), palette, cx));
        Some(
            v_flex()
                .id(id)
                .role(Role::List)
                .aria_label(title)
                .child(
                    div()
                        .px(ui(12.))
                        .pt(ui(8.))
                        .pb(ui(3.))
                        .text_size(ui(TYPE_LABEL))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(palette.chrome_faint)
                        .child(format!("{} · {}", title.to_uppercase(), entries.len())),
                )
                .children(rows)
                .when(remaining > 0, |this| {
                    this.child(
                        div().px(ui(12.)).py(ui(4.)).child(
                            Button::new(SharedString::from(format!("{id}-more")))
                                .outline()
                                .xsmall()
                                .w_full()
                                .label(format!("Show {} more…", remaining.min(MORE_ROWS)))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let limit = this.limits.entry(id).or_insert(FIRST_ROWS);
                                    *limit += MORE_ROWS;
                                    cx.notify();
                                })),
                        ),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_row(
        &self,
        entry: &StatusEntry,
        target: Target,
        mutable: bool,
        host: Option<(String, String)>,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let staged = target == Target::Staged;
        let key = format!("{}-{}", model::target_name(target), entry.path);
        let group = SharedString::from(format!("git-row-group-{key}"));
        let (name, dir) = model::split_display_path(&entry.display_path);
        let title = model::status_title(entry, target);
        let code = model::status_code(entry, target);
        let color = state_color(model::kind_for(entry, target), entry.conflicted, palette);
        // A conflict is resolved elsewhere and a submodule pointer is
        // read-only, so neither offers the buttons at all.
        let offers_actions = mutable && !entry.conflicted && !entry.submodule;
        let tooltip = SharedString::from(format!("{} · {title}", entry.display_path));
        let weak = cx.entity().downgrade();
        let (open, focus, right) = (entry.clone(), entry.clone(), entry.clone());
        let drag = entry
            .absolute_path
            .clone()
            .zip(host)
            .map(|(path, (profile, server))| InternalPathDrag {
                host_profile_id: profile,
                server_identity: server,
                path,
            });
        let ghost = SharedString::from(name.to_owned());
        let button = h_flex()
            .id(SharedString::from(format!("git-row-{key}")))
            .test_support()
            .role(Role::Button)
            .aria_label(entry.display_path.clone())
            .w_full()
            .items_center()
            .gap(ui(6.))
            .px(ui(12.))
            .py(ui(3.))
            .font_family(fonts::MONO)
            .text_size(ui(12.))
            .text_color(palette.chrome_ink)
            .group_hover(group.clone(), |style| style.bg(palette.chrome_hover))
            // The conflict bar is the item's, and takes its width from the
            // padding so the row stays aligned with the others.
            .when(entry.conflicted, |this| this.pl(ui(10.)))
            .tooltip({
                let tooltip = tooltip.clone();
                move |window, cx| {
                    if crate::context_menu::is_open(window, cx) {
                        cx.new(|_| gpui_kit::EmptyView).into()
                    } else {
                        gpui_kit::component::tooltip::Tooltip::new(tooltip.clone())
                            .build(window, cx)
                    }
                }
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| this.focus_row(&focus, target, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| this.focus_row(&right, target, cx)),
            )
            .on_click(
                cx.listener(move |this, event: &gpui_kit::ClickEvent, _, cx| {
                    this.open(&open, target, event.click_count() < 2, cx)
                }),
            )
            .when_some(drag, |this, drag| {
                this.on_drag(drag, move |_, _, _, cx| {
                    cx.new(|_| PathGhost(ghost.clone()))
                })
            })
            .child(
                Icon::new(IconName::File)
                    .size(ui(12.))
                    .flex_none()
                    .text_color(palette.chrome_dim),
            )
            .child(div().min_w_0().truncate().child(name.to_owned()))
            .when(!dir.is_empty(), |this| {
                this.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(ui(TYPE_META))
                        .text_color(palette.chrome_faint)
                        .child(dir.to_owned()),
                )
            })
            .child(
                div()
                    .ml_auto()
                    .w(ui(10.))
                    .flex_none()
                    .text_center()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .when(offers_actions, |this| {
                        this.group_hover(group.clone(), |style| style.invisible())
                    })
                    .child(code),
            );
        let action = |id: String, icon: Glyph, label: &'static str| {
            Button::new(SharedString::from(id))
                .ghost()
                .xsmall()
                .compact()
                .size(ui(20.))
                .accessibility_label(label)
                .tooltip(label)
                .icon(Icon::new(icon).size(ui(12.)).text_color(palette.chrome_dim))
        };
        let actions = offers_actions.then(|| {
            let (stage, discard) = (entry.clone(), entry.clone());
            h_flex()
                .absolute()
                .right(ui(6.))
                .top_0()
                .bottom_0()
                .items_center()
                .invisible()
                .group_hover(group.clone(), |style| style.visible())
                // The actions sit over the row: a press on them is not a press
                // on the row, which would open the diff.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    h_flex()
                        .gap(ui(2.))
                        .px(ui(2.))
                        .rounded(ui(RADIUS_CONTROL))
                        .bg(palette.chrome_hover)
                        .child(
                            action(
                                format!("git-stage-{key}"),
                                if staged { Glyph::Minus } else { Glyph::Plus },
                                if staged { "Unstage file" } else { "Stage file" },
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if staged {
                                        this.mutate_file(
                                            &stage,
                                            Target::Staged,
                                            MutationKind::UnstageFile,
                                            None,
                                            cx,
                                        )
                                    } else {
                                        this.mutate_file(
                                            &stage,
                                            Target::Unstaged,
                                            MutationKind::StageFile,
                                            None,
                                            cx,
                                        )
                                    }
                                },
                            )),
                        )
                        .child(
                            action(
                                format!("git-discard-{key}"),
                                Glyph::Discard,
                                if entry.untracked {
                                    "Delete untracked file"
                                } else {
                                    "Discard changes"
                                },
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.request_discard(&discard, target, window, cx)
                                },
                            )),
                        ),
                )
        });
        let inset = if entry.conflicted { 28. } else { 30. };
        let note = |text: String| {
            div()
                .pl(ui(inset))
                .pr(ui(12.))
                .pb(ui(3.))
                .text_size(ui(TYPE_META))
                .text_color(palette.chrome_faint)
                .child(text)
        };
        let copied =
            entry.index_kind == ChangeKind::Copied || entry.worktree_kind == ChangeKind::Copied;
        let menu_entry = entry.clone();
        let menu = move |menu: PopupMenu, window: &mut Window, cx: &mut Context<PopupMenu>| {
            let entries = weak
                .upgrade()
                .map(|panel| panel.read(cx).menu_entries(&menu_entry, target, &panel, cx))
                .unwrap_or_default();
            fill(menu, &entries, window, cx)
        };
        v_flex()
            .id(SharedString::from(format!("git-item-{key}")))
            .group(group)
            .role(Role::ListItem)
            .when(entry.conflicted, |this| {
                this.border_l_2().border_color(palette.danger)
            })
            .child(
                div()
                    .relative()
                    .child(button)
                    .children(actions)
                    .context_menu(menu),
            )
            .when_some(self.pending.get(&entry.path), |this, label| {
                this.child(note((*label).to_owned()))
            })
            .when(entry.submodule, |this| {
                this.child(note(format!(
                    "submodule {} · actions unavailable",
                    entry.submodule_state
                )))
            })
            .when_some(entry.display_original_path.clone(), |this, original| {
                this.child(note(format!(
                    "{} from {original}",
                    if copied { "copied" } else { "renamed" }
                )))
            })
            .when(entry.symlink, |this| {
                this.child(note("symbolic link".into()))
            })
            .when(entry.conflicted, |this| {
                this.child(note(format!("conflict {}", entry.conflict_code)))
            })
            .into_any_element()
    }

    /// The labeled way to the row's actions, and the keyboard's.
    fn menu_entries(
        &self,
        entry: &StatusEntry,
        target: Target,
        panel: &Entity<Self>,
        cx: &App,
    ) -> Vec<Entry> {
        let run = |command: CommandId| {
            let (panel, entry) = (panel.downgrade(), entry.clone());
            move |window: &mut Window, cx: &mut App| {
                panel
                    .update(cx, |this, cx| {
                        this.row_action(&entry, target, command, window, cx)
                    })
                    .ok();
            }
        };
        let mut entries: Vec<Entry> = vec![Item::new("Open diff", run("git.openDiff")).into()];
        let state = self.state(cx);
        if !self.unavailable(&state) && !entry.conflicted {
            entries.push(
                match target {
                    Target::Staged => Item::new("Unstage", run("git.unstage")),
                    Target::Unstaged => Item::new("Stage", run("git.stage")),
                }
                .disabled(entry.submodule)
                .into(),
            );
            entries.push(Entry::Separator);
            entries.push(
                Item::new(
                    if entry.untracked {
                        "Delete untracked file…"
                    } else {
                        "Discard changes…"
                    },
                    run("git.discard"),
                )
                .destructive()
                .disabled(entry.submodule)
                .into(),
            );
        }
        entries
    }

    fn render_form(
        &mut self,
        status: &StatusSnapshot,
        unavailable: bool,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let input = self.input(window, cx);
        if std::mem::take(&mut self.form.clear) {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        let staged = self.staged_count(cx);
        let busy = self.form.busy;
        let nothing_staged = staged == 0;
        let can_push = !unavailable && !status.repository.initial;
        let status_line = [self.form.note.clone(), self.form.progress.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let commit = DropdownButton::new("git-commit")
            .button(
                Button::new("git-commit-submit")
                    .primary()
                    .xsmall()
                    .flex_1()
                    .min_w_0()
                    .label(format!("Commit {staged} staged"))
                    .disabled(unavailable || busy || nothing_staged)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.run(Work::Commit, window, cx)),
                    ),
            )
            .menu_accessibility_label("More commit actions")
            .disabled(unavailable || busy)
            .flex_1()
            .min_w_0()
            .dropdown_menu({
                let this = cx.entity().downgrade();
                let disabled = nothing_staged || !can_push;
                move |menu, window, cx| {
                    let this = this.clone();
                    let entries = [Item::new("Commit & Push", move |window, cx| {
                        this.update(cx, |this, cx| this.run(Work::CommitAndPush, window, cx))
                            .ok();
                    })
                    .disabled(disabled)
                    .into()];
                    fill(menu, &entries, window, cx)
                }
            });
        v_flex()
            .id("git-commit")
            .flex_none()
            .gap(ui(6.))
            .px(ui(12.))
            .py(ui(8.))
            .border_t_1()
            .border_color(palette.chrome_hairline)
            .child(
                div()
                    .capture_action(cx.listener(move |this, action: &Enter, window, cx| {
                        if !action.secondary {
                            return;
                        }
                        // The same condition the Commit button carries.
                        cx.stop_propagation();
                        if !unavailable && !this.form.busy && this.staged_count(cx) > 0 {
                            this.run(Work::Commit, window, cx);
                        }
                    }))
                    .child(
                        Textarea::new(&input)
                            .aria_label("Commit message")
                            .disabled(unavailable)
                            .text_size(ui(12.)),
                    ),
            )
            .child(
                h_flex().gap(ui(6.)).child(commit).child(
                    Button::new("git-push")
                        .outline()
                        .xsmall()
                        .label("Push")
                        .accessibility_label("Push to upstream")
                        .disabled(unavailable || busy || !can_push)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.run(Work::Push, window, cx)),
                        ),
                ),
            )
            .when(!status_line.is_empty(), |this| {
                this.child(
                    div()
                        .id("git-commit-status")
                        .role(Role::Status)
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(palette.chrome_dim)
                        .child(status_line),
                )
            })
            .when_some(self.form.error.clone(), |this, error| {
                this.child(SurfaceError::new("git-commit-error", error))
            })
            .when_some(self.form.output.as_ref(), |this, (result, verb)| {
                let failed = !result.applied() || result.refresh_failed;
                this.child(
                    div()
                        .id("git-output")
                        .role(Role::Log)
                        .aria_label(format!("Git {} output", verb.name().to_lowercase()))
                        .max_h(ui(96.))
                        .overflow_y_scroll()
                        .p(ui(6.))
                        .rounded(ui(3.))
                        .bg(palette.chrome_bg)
                        .font_family(fonts::MONO)
                        .text_size(ui(TYPE_META))
                        .text_color(if failed {
                            palette.danger_ink
                        } else {
                            palette.term_2
                        })
                        .child(model::command_details(result, *verb)),
                )
            })
            .into_any_element()
    }
}

impl Render for GitPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        let state = self.state(cx);
        let Some(root) = &self.context.root else {
            return self.render_empty(
                "Select a terminal pane to discover its repository.".into(),
                false,
                &palette,
                cx,
            );
        };
        if !root.git_worktree {
            return self.render_empty(
                "The active pane is outside a Git worktree.".into(),
                false,
                &palette,
                cx,
            );
        }
        let Some(status) = state.status.clone() else {
            let (detail, retry) = match &state.error {
                Some(error) => (format!("Git unavailable: {error}"), true),
                None if state.loading && self.key.is_some() => {
                    ("Reading Git status…".into(), false)
                }
                None => ("Git status is unavailable.".into(), true),
            };
            return self.render_empty(detail, retry, &palette, cx);
        };
        if status.oversized {
            let reason = status.error.clone().unwrap_or_else(|| {
                "The host bounded this snapshot to keep the terminal connection responsive.".into()
            });
            return self.render_empty(
                format!(
                    "Repository status is too large. {reason} {} entries were detected.",
                    status.total_entry_count
                ),
                true,
                &palette,
                cx,
            );
        }
        let unavailable = self.unavailable(&state);
        let groups = model::group_entries(&status.entries);
        let lists: Vec<AnyElement> = [
            (
                "git-conflicts",
                "Merge changes",
                &groups.conflicts,
                Target::Unstaged,
            ),
            ("git-staged", "Staged", &groups.staged, Target::Staged),
            ("git-changes", "Changes", &groups.unstaged, Target::Unstaged),
            (
                "git-untracked",
                "Untracked",
                &groups.untracked,
                Target::Unstaged,
            ),
        ]
        .into_iter()
        .filter_map(|(id, title, entries, target)| {
            self.render_group(id, title, entries, target, !unavailable, &palette, cx)
        })
        .collect();
        let form = self.render_form(&status, unavailable, &palette, window, cx);
        v_flex()
            .id("git-panel")
            .test_support()
            .role(Role::Region)
            .aria_label("Source Control")
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap(ui(6.))
                    .px(ui(12.))
                    .py(ui(7.))
                    .border_b_1()
                    .border_color(palette.chrome_hairline)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(ui(1.))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(ui(12.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(palette.chrome_ink)
                                    .child(model::head_label(&status.repository).to_owned()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .font_family(fonts::MONO)
                                    .text_size(ui(TYPE_META))
                                    .text_color(palette.chrome_faint)
                                    .child(status.repository.worktree_root.clone()),
                            ),
                    )
                    .child(
                        Button::new("git-refresh")
                            .ghost()
                            .xsmall()
                            .compact()
                            .w(ui(22.))
                            .accessibility_label("Refresh Git status")
                            .tooltip("Refresh Git status")
                            .icon(
                                Icon::new(Glyph::Refresh)
                                    .size(ui(14.))
                                    .text_color(palette.chrome_dim),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .when_some(state.error.clone(), |this, error| {
                this.child(SurfaceError::new("git-error", error))
            })
            .when(status.copy_detection_incomplete, |this| {
                this.child(
                    div()
                        .id("git-copy-note")
                        .role(Role::Status)
                        .px(ui(12.))
                        .py(ui(5.))
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(palette.chrome_dim)
                        .child("Copy detection was bounded for this large change set; some copies may appear as additions."),
                )
            })
            .when(!status.authoritative, |this| {
                this.child(alert_line(
                    "git-resync",
                    "Git status is resynchronizing. Mutations are disabled.".into(),
                    &palette,
                ))
            })
            .child(
                v_flex()
                    .id("git-status-groups")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(lists)
                    .when(status.entries.is_empty(), |this| {
                        this.child(quiet("Working tree clean.", &palette))
                    }),
            )
            .child(form)
            .into_any_element()
    }
}

#[cfg(test)]
impl GitPanel {
    pub(super) fn set_message(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let input = self.input(window, cx);
        let text = text.to_owned();
        input.update(cx, |input, cx| input.set_value(text, window, cx));
    }

    /// The commit form's error, settled note and progress line.
    pub(super) fn form_state(&self) -> (Option<String>, Option<String>, Option<String>) {
        (
            self.form.error.clone(),
            self.form.note.clone(),
            self.form.progress.clone(),
        )
    }
}
