//! One file tab on screen: its toolbar, the content ladder (loading, failed,
//! changed, binary, too large, text), the editor, and Monaco's find widget and
//! Go to Line box. Port of the drawing half of `features/shell/AppTabSurface.tsx`
//! and of `features/files/FileEditor.tsx`.

use std::{
    any::Any,
    cell::Cell,
    ops::Range,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use desktop_core::app_state::{AppTabKind, AppTabRecord, AppTabViewMode};
use gpui_kit::{
    AnyElement, App, AppContext, Context, Div, Entity, FocusHandle, Focusable, FontWeight, Hsla,
    Image, ImageFormat, InteractiveElement, IntoElement, KeyBinding, ParentElement, Pixels, Render,
    Role, SharedString, StatefulInteractiveElement, Styled, StyledImage, Subscription, Task,
    Window, actions,
    assets::IconName,
    base::TestSupportExt,
    canvas,
    component::{
        Disableable, Selectable, Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        input::{
            Editor, EditorState, Escape, Input, InputEvent, InputState, Position, RangeDecoration,
            RangeDecorationCollection, RangeDecorationStyle, Replace, RopeExt, Search,
        },
        v_flex,
    },
    div, img, point,
    prelude::FluentBuilder,
    px,
};

use super::{
    LiveFind, Root, WatchTarget, Watcher,
    document::{Content, Document, DocumentEvent, Live},
    find::{self, Query},
    io::{BinaryFile, FileIo, IMAGE_PREVIEW_LIMIT_BYTES},
    save_status::{SaveStatus, Visible},
    tabs::set_markdown_view_mode,
};
use crate::{
    fonts,
    theme::{
        self, Palette,
        metrics::{
            BAR_HEIGHT, NARROW_TERMINAL, RADIUS_CONTROL, TYPE_LABEL, TYPE_META, TYPE_SECONDARY,
            narrower_than, ui,
        },
    },
    ui::{
        surface_error::SurfaceError,
        toolbar::{Tone, toolbar_button},
    },
    workspaces::{self, model::pin_app_tab},
};

actions!(
    file_editor,
    [
        GoToLine,
        FindNext,
        FindPrevious,
        ToggleCase,
        ToggleWord,
        ToggleRegex
    ]
);

/// Focus anywhere in a file tab.
pub const CONTEXT: &str = "FileEditor";
/// Focus in the find bar.
pub(super) const FIELD_CONTEXT: &str = "FileFind";
/// A read that answers this fast shows no loading line at all.
const LOADING_DELAY: Duration = Duration::from_millis(150);

pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("ctrl-g", GoToLine, Some(CONTEXT)),
        KeyBinding::new("f3", FindNext, Some(CONTEXT)),
        KeyBinding::new("shift-f3", FindPrevious, Some(CONTEXT)),
        // Monaco's toggle chords. Plain alt types a character on macOS.
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-cmd-c", ToggleCase, Some(FIELD_CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-cmd-w", ToggleWord, Some(FIELD_CONTEXT)),
        #[cfg(target_os = "macos")]
        KeyBinding::new("alt-cmd-r", ToggleRegex, Some(FIELD_CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-c", ToggleCase, Some(FIELD_CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-w", ToggleWord, Some(FIELD_CONTEXT)),
        #[cfg(not(target_os = "macos"))]
        KeyBinding::new("alt-r", ToggleRegex, Some(FIELD_CONTEXT)),
        // The inputs bind it already; the widget's buttons need it too.
        KeyBinding::new("escape", Escape, Some(FIELD_CONTEXT)),
    ]);
}

/// The find widget's state while it is open.
struct FindBar {
    /// What it searches, for as long as it is open.
    target: Target,
    query: Entity<InputState>,
    replacement: Entity<InputState>,
    replacing: bool,
    toggles: Query,
    /// The buffer revision the matches were found in.
    searched: Option<(u64, Query)>,
    matches: Vec<Range<usize>>,
    error: Option<String>,
    /// The source editor's marks; the Live view marks its own.
    highlights: Option<RangeDecorationCollection>,
    /// The match the selection is on, marked apart from the others: the
    /// editor paints its selection only while it has focus.
    current: Option<(Option<Range<usize>>, RangeDecorationCollection)>,
    /// How tall it was drawn, for the Live view to reveal matches below it.
    height: Rc<Cell<Pixels>>,
    _subscriptions: Vec<Subscription>,
}

/// What the find bar searches: the source editor, or the Live view through
/// its seam. Offsets are each one's own.
#[derive(Clone)]
enum Target {
    Source(Entity<EditorState>),
    Live(FocusHandle, Rc<dyn LiveFind>),
}

impl PartialEq for Target {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Source(a), Self::Source(b)) => a == b,
            (Self::Live(_, a), Self::Live(_, b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Target {
    fn text(&self, cx: &App) -> String {
        match self {
            Self::Source(editor) => editor.read(cx).text().to_string(),
            Self::Live(_, live) => live.text(cx),
        }
    }

    fn selection(&self, cx: &App) -> Range<usize> {
        match self {
            Self::Source(editor) => editor.read(cx).selected_range(),
            Self::Live(_, live) => live.selection(cx),
        }
    }

    fn select(&self, range: Range<usize>, cx: &mut App) {
        match self {
            Self::Source(editor) => {
                editor.update(cx, |editor, cx| editor.set_selected_range(range, cx))
            }
            Self::Live(_, live) => live.select(range, cx),
        }
    }

    fn replace(&self, range: Range<usize>, with: String, window: &mut Window, cx: &mut App) {
        match self {
            Self::Source(editor) => editor.update(cx, |editor, cx| {
                editor.set_selected_range(range, cx);
                editor.replace(with, window, cx);
            }),
            Self::Live(_, live) => live.replace(range, &with, cx),
        }
    }

    /// One edit, so one undo puts it all back.
    fn replace_all(&self, text: String, len: usize, window: &mut Window, cx: &mut App) {
        match self {
            Self::Source(editor) => editor.update(cx, |editor, cx| {
                editor.set_selected_range(0..len, cx);
                editor.replace(text, window, cx);
                editor.set_selected_range(0..0, cx);
            }),
            Self::Live(_, live) => live.replace_all(&text, cx),
        }
    }

    fn focus(&self, window: &mut Window, cx: &mut App) {
        match self {
            Self::Source(editor) => editor.update(cx, |editor, cx| editor.focus(window, cx)),
            Self::Live(focus, _) => window.focus(focus, cx),
        }
    }
}

struct GoTo {
    input: Entity<InputState>,
    _subscription: Subscription,
}

pub struct DocumentPane {
    pub(super) record: AppTabRecord,
    root: Root,
    pub(super) document: Entity<Document>,
    pub(super) focus: FocusHandle,
    save_status: SaveStatus,
    status_wake: Option<(Instant, Task<()>)>,
    loading_shown: bool,
    loading_wake: Option<Task<()>>,
    /// The decoded image preview, kept while the file's generation is shown.
    image: Option<(String, Arc<Image>)>,
    /// The Live view's keyboard and what find searches in it, while the
    /// tab shows it.
    live: Option<(FocusHandle, Rc<dyn LiveFind>)>,
    find: Option<FindBar>,
    go_to: Option<GoTo>,
    /// The last query and toggles, for the next time find opens.
    remembered: Query,
    _watch: Option<Box<dyn Any>>,
    _subscriptions: Vec<Subscription>,
}

impl DocumentPane {
    pub(super) fn new(
        record: AppTabRecord,
        root: Root,
        io: Rc<dyn FileIo>,
        live: Rc<Live>,
        watcher: Option<Watcher>,
        cx: &mut Context<Self>,
    ) -> Self {
        let document = cx.new(|cx| {
            Document::new(
                record.id.clone(),
                record.host_profile_id.clone(),
                record.server_identity.clone(),
                record.resource.clone(),
                root.clone(),
                io,
                live,
                cx,
            )
        });
        let (host, tab_id) = (record.host_profile_id.clone(), record.id.clone());
        let subscriptions = vec![
            cx.observe(&document, |_, _, cx| cx.notify()),
            cx.subscribe(
                &document,
                move |_, _, event: &DocumentEvent, cx| match event {
                    // A preview tab stops being disposable the moment it is typed in.
                    DocumentEvent::Dirty => {
                        if let Some(workspaces) = workspaces::current(cx) {
                            let (host, tab_id) = (host.clone(), tab_id.clone());
                            workspaces.update(cx, |this, cx| {
                                this.update_app_state(
                                    move |state| pin_app_tab(state, &host, &tab_id),
                                    cx,
                                )
                            });
                        }
                    }
                },
            ),
        ];
        let watched = watched_path(&root, &record.resource);
        let watch = watcher.map(|watcher| {
            watcher(
                &WatchTarget {
                    host: record.host_profile_id.clone(),
                    server: record.server_identity.clone(),
                    root: root.clone(),
                    watched,
                    resource: record.resource.clone(),
                },
                cx,
            )
        });
        Self {
            record,
            root,
            document,
            focus: cx.focus_handle(),
            save_status: SaveStatus::default(),
            status_wake: None,
            loading_shown: false,
            loading_wake: None,
            image: None,
            live: None,
            find: None,
            go_to: None,
            remembered: Query::default(),
            _watch: watch,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn editor(&self, cx: &App) -> Option<Entity<EditorState>> {
        self.document.read(cx).existing_editor().cloned()
    }

    /// Where typing goes: the Live view while it is shown, else the editor.
    pub(super) fn input_focus(&self, cx: &App) -> Option<FocusHandle> {
        self.live
            .as_ref()
            .map(|(focus, _)| focus.clone())
            .or_else(|| {
                self.editor(cx)
                    .map(|editor| editor.read(cx).focus_handle(cx))
            })
    }

    /// The calm save status, and a wake-up for when it may change on its own.
    fn visible_save_state(&mut self, cx: &mut Context<Self>) -> Option<Visible> {
        let document = self.document.read(cx);
        let (state, writable) = (document.save_state(), document.can_write(cx));
        let (visible, due) = self.save_status.at(state, writable, Instant::now());
        match due {
            Some(due) if self.status_wake.as_ref().is_none_or(|(at, _)| *at != due) => {
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(due.saturating_duration_since(Instant::now()))
                        .await;
                    this.update(cx, |_, cx| cx.notify()).ok();
                });
                self.status_wake = Some((due, task));
            }
            Some(_) => {}
            None => self.status_wake = None,
        }
        visible
    }

    fn loading_line(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.loading_shown && self.loading_wake.is_none() {
            self.loading_wake = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(LOADING_DELAY).await;
                this.update(cx, |this, cx| {
                    this.loading_shown = true;
                    cx.notify();
                })
                .ok();
            }));
        }
        self.loading_shown
    }

    fn set_view_mode(&mut self, mode: AppTabViewMode, cx: &mut Context<Self>) {
        let Some(workspaces) = workspaces::current(cx) else {
            return;
        };
        // Only a person leaving the preview or Live for the editor starts a
        // new editor-paint interaction.
        if self.source_hidden(cx) && matches!(mode, AppTabViewMode::Source | AppTabViewMode::Split)
        {
            self.document
                .update(cx, |document, _| document.hold_editor_paint());
        }
        let (host, id) = (self.record.host_profile_id.clone(), self.record.id.clone());
        workspaces.update(cx, |this, cx| {
            this.update_app_state(
                move |state| set_markdown_view_mode(state, &host, &id, mode),
                cx,
            )
        });
    }

    fn download(&self, window: &mut Window, cx: &mut App) {
        if let Some(download) = super::download(cx) {
            download(&self.record, &self.root, window, cx);
        }
    }

    // Find and replace.

    /// The view the tab shows: its own, unless that view's surface has not
    /// arrived or the file is too large for Live. Either way the mode stays
    /// on record, so it comes back when it can.
    fn shown_mode(&self, cx: &App) -> AppTabViewMode {
        let markdown = self.record.kind == AppTabKind::Markdown;
        let live_unavailable = self.live_unavailable(cx);
        self.record
            .view_mode
            .filter(|mode| markdown && markdown_modes(cx).contains(mode))
            .filter(|mode| !(*mode == AppTabViewMode::Live && live_unavailable))
            .unwrap_or(AppTabViewMode::Source)
    }

    /// The file as read is over the Live view's limit, as in Tauri.
    fn live_unavailable(&self, cx: &App) -> bool {
        let read = self.document.read(cx).read_bytes();
        super::live_markdown(cx)
            .is_some_and(|live| read.is_some_and(|read| read > live.limit_bytes))
    }

    /// A Markdown tab showing its preview or Live view, where the source
    /// editor's boxes do not draw.
    fn source_hidden(&self, cx: &App) -> bool {
        matches!(
            self.shown_mode(cx),
            AppTabViewMode::Preview | AppTabViewMode::Live
        )
    }

    /// What find searches: the Live view while it is shown, else the source
    /// editor unless the preview hides it.
    fn target(&self, cx: &App) -> Option<Target> {
        if let Some((focus, live)) = &self.live {
            return Some(Target::Live(focus.clone(), live.clone()));
        }
        self.editor(cx)
            .filter(|_| !self.source_hidden(cx))
            .map(Target::Source)
    }

    fn query(&self, cx: &App) -> Option<Query> {
        let find = self.find.as_ref()?;
        Some(Query {
            text: find.query.read(cx).value().to_string(),
            ..find.toggles.clone()
        })
    }

    fn open_find(&mut self, replacing: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target(cx) else {
            return;
        };
        self.go_to = None;
        // What is selected seeds the query, unless it spans lines.
        let selected = {
            let range = target.selection(cx);
            let text = target.text(cx)[range].to_owned();
            (!text.is_empty() && !text.contains('\n')).then_some(text)
        };
        let writable = self.document.read(cx).can_write(cx);
        if self.find.as_ref().is_some_and(|find| find.target != target) {
            self.drop_find(cx);
        }
        if self.find.is_none() {
            let query = cx.new(|cx| InputState::new(window, cx).placeholder("Find"));
            let replacement = cx.new(|cx| InputState::new(window, cx).placeholder("Replace"));
            let mut subscriptions = vec![
                cx.subscribe_in(&query, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::Change => this.typed(cx),
                        InputEvent::PressEnter { shift: true, .. } => this.step(false, cx),
                        InputEvent::PressEnter { .. } => this.step(true, cx),
                        _ => {}
                    }
                    let _ = window;
                }),
                cx.subscribe_in(
                    &replacement,
                    window,
                    |this, _, event: &InputEvent, window, cx| {
                        if let InputEvent::PressEnter { secondary, .. } = event {
                            if *secondary {
                                this.replace_all(window, cx);
                            } else {
                                this.replace_one(window, cx);
                            }
                        }
                    },
                ),
            ];
            // The count follows the selection as the user moves through
            // the buffer, and the matches follow its edits.
            subscriptions.push(match &target {
                Target::Source(editor) => cx.observe(editor, |_, _, cx| cx.notify()),
                Target::Live(_, live) => {
                    let pane = cx.entity().downgrade();
                    live.observe(
                        Box::new(move |cx| {
                            pane.update(cx, |_, cx| cx.notify()).ok();
                        }),
                        cx,
                    )
                }
            });
            // Find opens on the query it closed with, as Monaco's does.
            let remembered = self.remembered.clone();
            query.update(cx, |input, cx| {
                input.set_value(remembered.text.clone(), window, cx)
            });
            self.find = Some(FindBar {
                target,
                query,
                replacement,
                replacing: false,
                toggles: Query {
                    text: String::new(),
                    ..remembered
                },
                searched: None,
                matches: Vec::new(),
                error: None,
                highlights: None,
                current: None,
                height: Rc::default(),
                _subscriptions: subscriptions,
            });
            self.refresh_matches(cx);
        }
        let Some(find) = &mut self.find else {
            return;
        };
        // Find keeps the replace row as it was; Replace opens it.
        if replacing {
            find.replacing = writable;
        }
        if let Some(selected) = selected {
            // Taken literally, as Monaco escapes a seed in regex mode.
            let selected = if find.toggles.regex {
                regex::escape(&selected)
            } else {
                selected
            };
            find.query
                .update(cx, |input, cx| input.set_value(selected, window, cx));
            self.typed(cx);
        }
        let Some(find) = &self.find else {
            return;
        };
        find.query.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    /// Closes the bar, keeping its query for the next time, and clears its
    /// marks.
    fn drop_find(&mut self, cx: &mut Context<Self>) -> Option<Target> {
        let find = self.find.take()?;
        self.remembered = Query {
            text: find.query.read(cx).value().to_string(),
            ..find.toggles
        };
        if let Some(highlights) = find.highlights {
            highlights.dispose(cx);
        }
        if let Some((_, current)) = find.current {
            current.dispose(cx);
        }
        if let Target::Live(_, live) = &find.target {
            live.highlight(None, cx);
            live.set_top_inset(Pixels::ZERO, cx);
        }
        cx.notify();
        Some(find.target)
    }

    fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(target) = self.drop_find(cx).or_else(|| self.target(cx)) {
            target.focus(window, cx);
        }
    }

    /// The query as typed with the toggles, matched against the text as it
    /// stands, and highlighted.
    fn refresh_matches(&mut self, cx: &mut Context<Self>) {
        let Some(query) = self.query(cx) else {
            return;
        };
        let Some(find) = &mut self.find else {
            return;
        };
        let revision = match &find.target {
            Target::Source(_) => self.document.read(cx).revision(),
            Target::Live(_, live) => live.revision(cx),
        };
        if find.searched.as_ref() == Some(&(revision, query.clone())) {
            return;
        }
        // The Live view finds its marks in the text it draws: it needs the
        // matcher again only for a new query, not after each edit.
        let new_query = find
            .searched
            .as_ref()
            .is_none_or(|(_, searched)| *searched != query);
        find.searched = Some((revision, query.clone()));
        let (matches, error, matcher) = match query.compile() {
            None => (Vec::new(), None, None),
            Some(Err(error)) => (Vec::new(), Some(error), None),
            Some(Ok(matcher)) => (
                find::matches(&matcher, &find.target.text(cx)),
                None,
                Some(matcher),
            ),
        };
        match &find.target {
            Target::Source(editor) => {
                let palette = theme::palette(cx);
                // Monaco's amber match wash, so the current match, which also
                // carries the selection's accent wash, reads apart from the
                // others.
                let wash = Hsla::from(palette.warn).opacity(0.3);
                let decorations: Vec<RangeDecoration> = matches
                    .iter()
                    .take(find::SEARCH_COUNT_LIMIT)
                    .map(|found| {
                        RangeDecoration::new(found.clone())
                            .with_style(RangeDecorationStyle::Fill)
                            .with_color(wash)
                    })
                    .collect();
                match &find.highlights {
                    Some(highlights) => highlights.set(decorations, cx),
                    None => {
                        find.highlights = Some(editor.update(cx, |editor, cx| {
                            editor.create_range_decorations_collection(decorations, cx)
                        }));
                    }
                }
            }
            // The view marks what it draws itself, as CodeMirror marks only
            // what is on screen, past the count's limit too.
            Target::Live(_, live) if new_query => live.highlight(matcher, cx),
            Target::Live(..) => {}
        }
        find.matches = matches;
        find.error = error;
    }

    /// Marks the match the source editor's selection is on, when that
    /// changed. The Live view marks its own.
    fn mark_current(&mut self, cx: &mut Context<Self>) {
        let Some(find) = &mut self.find else {
            return;
        };
        let Target::Source(editor) = &find.target else {
            return;
        };
        let selection = editor.read(cx).selected_range();
        let on = find
            .matches
            .iter()
            .take(find::SEARCH_COUNT_LIMIT)
            .find(|found| **found == selection)
            .cloned();
        if find
            .current
            .as_ref()
            .is_some_and(|(marked, _)| *marked == on)
        {
            return;
        }
        let palette = theme::palette(cx);
        let decorations: Vec<RangeDecoration> = on
            .iter()
            .map(|found| {
                RangeDecoration::new(found.clone())
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(Hsla::from(palette.accent).opacity(0.45))
            })
            .collect();
        match &find.current {
            Some((_, current)) => current.set(decorations, cx),
            None => {
                let current = editor.update(cx, |editor, cx| {
                    editor.create_range_decorations_collection(decorations, cx)
                });
                find.current = Some((None, current));
            }
        }
        if let Some((marked, _)) = &mut find.current {
            *marked = on;
        }
    }

    /// Typing a query moves to its first match from where the selection
    /// starts, as Monaco does.
    fn typed(&mut self, cx: &mut Context<Self>) {
        self.refresh_matches(cx);
        let Some(find) = &self.find else {
            return;
        };
        let from = find.target.selection(cx).start;
        self.select_match(from, true, cx);
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(find) = &self.find else {
            return;
        };
        let selection = find.target.selection(cx);
        let at = if forward {
            selection.end
        } else {
            selection.start
        };
        self.select_match(at, forward, cx);
    }

    fn select_match(&mut self, at: usize, forward: bool, cx: &mut Context<Self>) {
        self.refresh_matches(cx);
        let (Some(find), Some(query)) = (&self.find, self.query(cx)) else {
            return;
        };
        let Some(Ok(matcher)) = query.compile() else {
            return;
        };
        let text = find.target.text(cx);
        let found = if forward {
            find::next(&matcher, &text, at)
        } else {
            find::previous(&matcher, &text, at)
        };
        if let Some(found) = found {
            find.target.clone().select(found, cx);
        }
        cx.notify();
    }

    fn toggle(&mut self, change: impl FnOnce(&mut Query), cx: &mut Context<Self>) {
        if let Some(find) = &mut self.find {
            change(&mut find.toggles);
            self.typed(cx);
        }
    }

    fn replace_one(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(find), Some(query)) = (&self.find, self.query(cx)) else {
            return;
        };
        if !self.document.read(cx).can_write(cx) {
            return;
        }
        let with = find.replacement.read(cx).value().to_string();
        let Some(Ok(matcher)) = query.compile() else {
            return;
        };
        let target = find.target.clone();
        let (text, selection) = (target.text(cx), target.selection(cx));
        // The match under the selection is replaced; otherwise the first
        // press only finds the next one.
        // Asked of the selection itself: the counted matches stop at a limit.
        let on_match = !selection.is_empty()
            && matcher
                .find_at(&text, selection.start)
                .is_some_and(|found| found.range() == selection);
        if on_match {
            let replaced = find::replacement(&matcher, &query, &text, &selection, &with);
            let end = selection.start + replaced.len();
            target.replace(selection, replaced, window, cx);
            self.select_match(end, true, cx);
        } else {
            self.select_match(selection.start, true, cx);
        }
    }

    fn replace_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(find), Some(query)) = (&self.find, self.query(cx)) else {
            return;
        };
        if !self.document.read(cx).can_write(cx) {
            return;
        }
        let with = find.replacement.read(cx).value().to_string();
        let Some(Ok(matcher)) = query.compile() else {
            return;
        };
        let target = find.target.clone();
        let text = target.text(cx);
        let (replaced, count) = find::replace_all(&matcher, &query, &text, &with);
        if count == 0 {
            return;
        }
        target.replace_all(replaced, text.len(), window, cx);
        cx.notify();
    }

    // Go to line.

    fn open_go_to(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editor(cx).is_none() || self.source_hidden(cx) {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let typed = input.read(cx).value().to_string();
                    this.go_to_line(&typed, window, cx);
                }
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        );
        input.update(cx, |input, cx| input.focus(window, cx));
        self.go_to = Some(GoTo {
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn go_to_line(&mut self, typed: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editor(cx) else {
            return;
        };
        let lines = editor.read(cx).text().lines_len();
        let Some((line, column)) = find::parse_line(typed).filter(|(line, _)| *line <= lines)
        else {
            return;
        };
        self.go_to = None;
        editor.update(cx, |editor, cx| {
            let position = Position::new(line as u32 - 1, column.unwrap_or(1) as u32 - 1);
            editor.set_cursor_position(position, window, cx);
            // In the middle of the view, as Monaco reveals it, not at the edge
            // the cursor move scrolls it to. The editor clamps the offset.
            if let Some(height) = editor.line_height() {
                let row = editor.display_map().buffer_line_to_display_row(line - 1);
                let viewport = editor.input_bounds().size.height;
                let top = height * row as f32 - (viewport - height) / 2.;
                let x = editor.scroll_offset().x;
                editor.set_scroll_offset(point(x, -top.max(px(0.))), cx);
            }
        });
        cx.notify();
    }

    /// Escape anywhere in the tab closes the box or widget that is open, as in
    /// Monaco, and otherwise goes on to the app.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.go_to.is_some() {
            self.close_go_to(window, cx);
        } else if self.find.is_some() {
            self.close_find(window, cx);
        } else {
            cx.propagate();
        }
    }

    fn close_go_to(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.go_to = None;
        if let Some(editor) = self.editor(cx) {
            editor.update(cx, |editor, cx| editor.focus(window, cx));
        }
        cx.notify();
    }
}

impl Focusable for DocumentPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// A terminal capability for one file is watched as that file; any other root
/// watches the file's directory.
fn watched_path(root: &Root, resource: &str) -> String {
    if root.token.starts_with("file-v2:") {
        resource.to_owned()
    } else {
        parent_path(resource)
    }
}

fn parent_path(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(index) => path[..index].into(),
    }
}

/// The modes a Markdown tab offers: source always, and the rendered ones once
/// their surfaces exist.
/// In Tauri's order: source, live, preview, split.
fn markdown_modes(cx: &App) -> Vec<AppTabViewMode> {
    let mut modes = vec![AppTabViewMode::Source];
    if super::live_markdown(cx).is_some() {
        modes.push(AppTabViewMode::Live);
    }
    if super::preview(cx).is_some() {
        modes.extend([AppTabViewMode::Preview, AppTabViewMode::Split]);
    }
    modes
}

/// A byte limit as Tauri's tooltip writes it: "1 MiB", "48 KiB".
fn size_label(bytes: u64) -> String {
    if bytes >= 1 << 20 && bytes.is_multiple_of(1 << 20) {
        format!("{} MiB", bytes >> 20)
    } else {
        format!("{} KiB", bytes >> 10)
    }
}

fn mode_label(mode: AppTabViewMode) -> &'static str {
    match mode {
        AppTabViewMode::Source => "source",
        AppTabViewMode::Live => "live",
        AppTabViewMode::Preview => "preview",
        AppTabViewMode::Split => "split",
    }
}

impl Render for DocumentPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        let record = self.record.clone();
        let content = self.document.read(cx).content();
        if content != Content::Loading {
            self.loading_shown = false;
            self.loading_wake = None;
        }
        if content != Content::Text {
            self.live = None;
        }
        let download = super::download(cx).is_some();
        let can_write = self.document.read(cx).can_write(cx);
        let connected = self.document.read(cx).connected(cx);
        let failed = |detail: String, this: &mut Self, cx: &mut Context<Self>| {
            empty_tab(
                &record,
                Some(&detail),
                download.then(|| this.download_button(cx)),
                cx,
            )
            .into_any_element()
        };
        let body: AnyElement = match content {
            Content::Loading if !connected => empty_tab(
                &record,
                Some("Reconnect and select a terminal pane to reopen this file."),
                None,
                cx,
            )
            .into_any_element(),
            Content::Loading => {
                let shown = self.loading_line(cx);
                v_flex()
                    .size_full()
                    .child(self.toolbar(None, None, cx))
                    .when(shown, |this| {
                        this.child(
                            div()
                                .id("file-loading")
                                .test_support()
                                .role(Role::Status)
                                .aria_label("Loading…")
                                .px(ui(12.))
                                .py(ui(10.))
                                .text_color(palette.chrome_faint)
                                .text_size(ui(TYPE_SECONDARY))
                                .child("Loading…"),
                        )
                    })
                    .into_any_element()
            }
            Content::Failed(detail) => failed(detail, self, cx),
            Content::Changed(detail) => failed(
                format!("The file changed or became unavailable: {detail}"),
                self,
                cx,
            ),
            Content::TooLarge => failed(
                "This text file is larger than the 10 MiB editor limit.".into(),
                self,
                cx,
            ),
            Content::Binary(file) => self.binary(&file, download, cx).into_any_element(),
            Content::Text => self.text_surface(can_write, window, cx).into_any_element(),
        };
        div()
            .id(SharedString::from(format!("file-tab-{}", record.id)))
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .role(Role::TabPanel)
            .aria_label(SharedString::from(record.title.clone()))
            .size_full()
            .bg(palette.chrome_bg)
            .on_action(
                cx.listener(|this, _: &Search, window, cx| this.open_find(false, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &Replace, window, cx| this.open_find(true, window, cx)),
            )
            .on_action(cx.listener(|this, _: &GoToLine, window, cx| this.open_go_to(window, cx)))
            .on_action(cx.listener(|this, _: &FindNext, _, cx| this.step(true, cx)))
            .on_action(cx.listener(|this, _: &FindPrevious, _, cx| this.step(false, cx)))
            .on_action(cx.listener(|this, _: &Escape, window, cx| this.dismiss(window, cx)))
            .child(body)
    }
}

impl DocumentPane {
    fn download_button(&self, cx: &mut Context<Self>) -> Button {
        let palette = theme::palette(cx);
        toolbar_button(
            Button::new("file-download").label("Download…"),
            Tone::Plain,
            false,
            &palette,
        )
        .on_click(cx.listener(|this, _, window, cx| this.download(window, cx)))
    }

    /// The bar across the top, drawn the same whether the file has arrived or
    /// not. Without a state the chip holds its place and claims nothing.
    fn toolbar(
        &mut self,
        visible: Option<Visible>,
        mode: Option<AppTabViewMode>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let palette = theme::palette(cx);
        let record = &self.record;
        let modes = (record.kind == AppTabKind::Markdown)
            .then(|| markdown_modes(cx))
            .filter(|modes| modes.len() > 1);
        let chip = div()
            .id("save-state")
            .role(Role::Status)
            .flex_none()
            .min_w(ui(72.))
            .px(ui(6.))
            .py(ui(2.))
            .rounded(ui(9.))
            .text_size(ui(TYPE_LABEL))
            .text_right()
            .map(|chip| match visible {
                Some(Visible::Error) => chip.text_color(palette.danger_ink).bg(palette.danger_wash),
                Some(_) => chip
                    .text_color(palette.state_working)
                    .bg(palette.chrome_raised),
                None => chip,
            })
            .aria_label(visible.map_or("", Visible::label))
            .child(visible.map_or("", Visible::label));
        h_flex()
            .id("editor-toolbar")
            .flex_none()
            .items_center()
            .gap(ui(8.))
            .min_w_0()
            .h(ui(BAR_HEIGHT))
            .px(ui(8.))
            .border_b_1()
            .border_color(palette.chrome_hairline)
            .bg(palette.chrome_bg)
            .child(
                div()
                    .id("editor-path")
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .font_family(fonts::MONO)
                    .text_size(ui(TYPE_META))
                    .text_color(palette.chrome_dim)
                    .tooltip({
                        let resource = record.resource.clone();
                        move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(resource.clone())
                                .build(window, cx)
                        }
                    })
                    .child(record.resource.clone()),
            )
            .child(chip)
            .when_some(modes, |bar, modes| {
                let shown = mode.unwrap_or(AppTabViewMode::Source);
                let live_unavailable = self.live_unavailable(cx);
                let live_limit = super::live_markdown(cx).map_or(0, |live| live.limit_bytes);
                bar.child(
                    h_flex()
                        .id("markdown-modes")
                        .role(Role::Group)
                        .aria_label("Markdown view")
                        .children(modes.into_iter().map(|item| {
                            let disabled = item == AppTabViewMode::Live && live_unavailable;
                            toolbar_button(
                                Button::new(SharedString::from(format!(
                                    "markdown-mode-{}",
                                    mode_label(item)
                                )))
                                .label(mode_label(item))
                                .selected(item == shown)
                                .toggled(item == shown)
                                .disabled(disabled)
                                .when(disabled, |button| {
                                    button.tooltip(format!(
                                        "Live view is limited to files under {}",
                                        size_label(live_limit)
                                    ))
                                }),
                                if item == shown {
                                    Tone::Pressed
                                } else {
                                    Tone::Plain
                                },
                                disabled,
                                &palette,
                            )
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.set_view_mode(item, cx)),
                            )
                        })),
                )
            })
            .when(super::download(cx).is_some(), |bar| {
                bar.child(self.download_button(cx))
            })
    }

    fn text_surface(
        &mut self,
        can_write: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let mode = self.shown_mode(cx);
        let visible = self.visible_save_state(cx);
        let error = self.document.read(cx).save_error();
        // A preview-only or Live tab never makes an editor.
        let shows_source = matches!(mode, AppTabViewMode::Source | AppTabViewMode::Split);
        let editor = shows_source.then(|| {
            self.document
                .update(cx, |document, cx| document.editor(window, cx))
        });
        if editor.is_some() {
            let due = self
                .document
                .update(cx, |document, _| document.editor_paint_due());
            crate::perf::after_next_frame(due, window);
        } else {
            // A load shown only as a preview drew no editor; a later switch
            // to the editor measures that switch, not the time in preview.
            self.document.update(cx, |document, _| {
                let _ = document.editor_paint_due();
            });
        }
        let live = (mode == AppTabViewMode::Live)
            .then(|| super::live_markdown(cx))
            .flatten()
            .map(|live| (live.surface)(&self.record, &self.document, window, cx));
        let was_live = std::mem::replace(
            &mut self.live,
            (live.as_ref()).map(|shown| (shown.focus.clone(), shown.find.clone())),
        )
        .map(|(focus, _)| focus);
        let typing = editor
            .as_ref()
            .map(|editor| editor.read(cx).focus_handle(cx))
            .or_else(|| self.live.as_ref().map(|(focus, _)| focus.clone()));
        // Focus given to the tab before its editor existed goes on to it.
        if let Some(target) = typing.clone()
            && self.focus.is_focused(window)
        {
            window.defer(cx, move |window, cx| window.focus(&target, cx));
        }
        // And comes back from a surface the new view hides, which would keep
        // it unseen and stop the app's shortcuts as a field does.
        let hidden =
            |handle: &FocusHandle| Some(handle) != typing.as_ref() && handle.is_focused(window);
        let hidden_editor = self
            .editor(cx)
            .is_some_and(|editor| hidden(&editor.read(cx).focus_handle(cx)));
        if hidden_editor || was_live.as_ref().is_some_and(hidden) {
            let target = typing.clone().unwrap_or_else(|| self.focus.clone());
            window.defer(cx, move |window, cx| window.focus(&target, cx));
        }
        // A bar opened on a view the tab now shows another in place of closes;
        // under the preview it waits, as the source editor's always has.
        let target = self.target(cx);
        if let (Some(find), Some(target)) = (&self.find, &target)
            && find.target != *target
        {
            self.drop_find(cx);
        }
        if self.find.is_some() {
            self.refresh_matches(cx);
        }
        let mut find_bar = self.find_bar(can_write, cx);
        // As drawn last frame: the Live view reveals a match below the bar.
        if let Some(find) = &self.find
            && let Target::Live(_, live) = &find.target
        {
            live.set_top_inset(find.height.get(), cx);
        }
        let source = editor.map(|editor| {
            let find_bar = find_bar.take();
            let go_to = self.go_to_box(&editor, cx);
            div()
                .id("editor-host")
                // Monaco ran with ligatures off: `->` stays two characters.
                .font_features(crate::terminal::cell_metrics::terminal_font_features())
                .relative()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .child(
                    Editor::new(&editor)
                        .bordered(false)
                        .size_full()
                        .readonly(!can_write)
                        .aria_label(SharedString::from(self.record.title.clone())),
                )
                .children(find_bar)
                .children(go_to)
        });
        let live = live.map(|shown| {
            div()
                .id("live-markdown-host")
                .relative()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .child(shown.element)
                .children(find_bar.take())
        });
        let preview = matches!(mode, AppTabViewMode::Preview | AppTabViewMode::Split)
            .then(|| super::preview(cx))
            .flatten()
            .map(|preview| {
                div()
                    .id("markdown-preview-host")
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .child(preview(&self.record, &self.document, window, cx))
            });
        v_flex()
            .size_full()
            .child(self.toolbar(visible, Some(mode), cx))
            .when_some(error, |this, error| {
                this.child(SurfaceError::new("editor-error", error))
            })
            .child(
                // Not `h_flex`, which centers its children: the editor and the
                // preview fill the height. A narrow window stacks them, as
                // `styles.css` does, so neither is squeezed to a sliver.
                div()
                    .flex()
                    .map(|this| {
                        if narrower_than(NARROW_TERMINAL, window) {
                            this.flex_col()
                        } else {
                            this.flex_row()
                        }
                    })
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .children(source)
                    .children(live)
                    .children(preview),
            )
    }

    fn find_bar(&mut self, can_write: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let palette = theme::palette(cx);
        self.mark_current(cx);
        let find = self.find.as_ref()?;
        let height = find.height.clone();
        let selection = find.target.selection(cx);
        let has_query = !find.query.read(cx).value().is_empty();
        let label = match &find.error {
            Some(_) => "Invalid pattern".to_owned(),
            None if has_query => find::count_label(&find.matches, &selection),
            None => String::new(),
        };
        let none = has_query && find.matches.is_empty();
        let toggles = find.toggles.clone();
        let replacing = find.replacing && can_write;
        let toggle = |id: &'static str, text: &'static str, title: &'static str, on: bool| {
            Button::new(id)
                .ghost()
                .xsmall()
                .compact()
                .label(text)
                .selected(on)
                .toggled(on)
                .tooltip(title)
                .accessibility_label(title)
        };
        let icon = |id: &'static str, icon: IconName, title: &'static str| {
            Button::new(id)
                .ghost()
                .xsmall()
                .compact()
                .icon(icon)
                .tooltip(title)
                .accessibility_label(title)
        };
        let find_row = h_flex()
            .min_w_0()
            .gap(ui(4.))
            .items_center()
            .child(
                div().w(ui(220.)).min_w(ui(40.)).flex_shrink(1.).child(
                    Input::new(&find.query)
                        .xsmall()
                        .id("find-query")
                        .aria_label("Find"),
                ),
            )
            .child(
                toggle("find-case", "Aa", "Match Case (Alt+C)", toggles.match_case).on_click(
                    cx.listener(|this, _, _, cx| this.toggle(|q| q.match_case = !q.match_case, cx)),
                ),
            )
            .child(
                toggle(
                    "find-word",
                    "ab",
                    "Match Whole Word (Alt+W)",
                    toggles.whole_word,
                )
                .on_click(
                    cx.listener(|this, _, _, cx| this.toggle(|q| q.whole_word = !q.whole_word, cx)),
                ),
            )
            .child(
                toggle(
                    "find-regex",
                    ".*",
                    "Use Regular Expression (Alt+R)",
                    toggles.regex,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle(|q| q.regex = !q.regex, cx))),
            )
            .child(
                div()
                    .id("find-count")
                    .role(Role::Status)
                    .aria_label(SharedString::from(label.clone()))
                    // Wide enough for "Invalid pattern", so the widget holds still.
                    .min_w(ui(90.))
                    .px(ui(4.))
                    .text_size(ui(TYPE_META))
                    .text_color(if none || find.error.is_some() {
                        palette.danger_ink
                    } else {
                        palette.chrome_dim
                    })
                    .child(label),
            )
            .child(
                icon(
                    "find-previous",
                    IconName::ArrowUp,
                    "Previous Match (Shift+Enter)",
                )
                .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
            )
            .child(
                icon("find-next", IconName::ArrowDown, "Next Match (Enter)")
                    .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
            )
            .child(
                icon("find-close", IconName::Close, "Close (Escape)")
                    .on_click(cx.listener(|this, _, window, cx| this.close_find(window, cx))),
            );
        let replace_row = replacing.then(|| {
            h_flex()
                .min_w_0()
                .gap(ui(4.))
                .items_center()
                .child(
                    div().w(ui(220.)).min_w(ui(40.)).flex_shrink(1.).child(
                        Input::new(&find.replacement)
                            .xsmall()
                            .id("find-replacement")
                            .aria_label("Replace"),
                    ),
                )
                .child(
                    icon("find-replace", IconName::Replace, "Replace (Enter)")
                        .on_click(cx.listener(|this, _, window, cx| this.replace_one(window, cx))),
                )
                .child(
                    Button::new("find-replace-all")
                        .ghost()
                        .xsmall()
                        .compact()
                        .label("All")
                        .tooltip("Replace All")
                        .accessibility_label("Replace All")
                        .on_click(cx.listener(|this, _, window, cx| this.replace_all(window, cx))),
                )
        });
        let expand = icon(
            "find-toggle-replace",
            if replacing {
                IconName::ChevronDown
            } else {
                IconName::ChevronRight
            },
            "Toggle Replace",
        )
        .disabled(!can_write)
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(find) = &mut this.find {
                find.replacing = !find.replacing;
                cx.notify();
            }
        }));
        let widget = h_flex()
            .id("find-widget")
            // Over the Live view, whose own mouse-down would take the click.
            .occlude()
            .key_context(FIELD_CONTEXT)
            .role(Role::Search)
            .aria_label("Find")
            .max_w_full()
            .items_start()
            .gap(ui(2.))
            .p(ui(4.))
            .rounded_b(ui(RADIUS_CONTROL))
            .border_1()
            .border_t_0()
            .border_color(palette.chrome_border)
            .bg(palette.chrome_raised)
            .shadow_md()
            .on_action(cx.listener(|this, _: &ToggleCase, _, cx| {
                this.toggle(|q| q.match_case = !q.match_case, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleWord, _, cx| {
                this.toggle(|q| q.whole_word = !q.whole_word, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleRegex, _, cx| this.toggle(|q| q.regex = !q.regex, cx)),
            )
            .child(expand)
            .child(
                v_flex()
                    .min_w_0()
                    .gap(ui(4.))
                    .child(find_row)
                    .children(replace_row),
            );
        // At the top right, and never wider than the editor: its fields give.
        Some(
            div()
                .absolute()
                .top_0()
                .left(ui(14.))
                .right(ui(14.))
                .flex()
                .justify_end()
                .child(widget)
                .child(
                    canvas(
                        move |bounds, _, _| height.set(bounds.size.height),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .size_full(),
                )
                .into_any_element(),
        )
    }

    fn go_to_box(
        &mut self,
        editor: &Entity<EditorState>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let palette = theme::palette(cx);
        let go_to = self.go_to.as_ref()?;
        let (current, lines) = {
            let editor = editor.read(cx);
            let position = editor.cursor_position();
            (
                (position.line as usize + 1, position.character as usize + 1),
                editor.text().lines_len(),
            )
        };
        let hint = find::line_hint(&go_to.input.read(cx).value(), current, lines);
        let dialog = v_flex()
            .id("go-to-line")
            .key_context(FIELD_CONTEXT)
            .role(Role::Dialog)
            .aria_label("Go to Line")
            .w(ui(420.))
            .max_w_full()
            .gap(ui(4.))
            .p(ui(6.))
            .rounded_b(ui(RADIUS_CONTROL))
            .border_1()
            .border_t_0()
            .border_color(palette.chrome_border)
            .bg(palette.chrome_raised)
            .shadow_md()
            .child(
                Input::new(&go_to.input)
                    .xsmall()
                    .id("go-to-line-input")
                    .aria_label("Go to Line"),
            )
            .child(
                div()
                    .text_size(ui(TYPE_META))
                    .text_color(palette.chrome_dim)
                    .child(hint),
            );
        // Centered at the top, and never wider than the editor.
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(dialog)
                .into_any_element(),
        )
    }

    fn binary(
        &mut self,
        file: &BinaryFile,
        download: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let palette = theme::palette(cx);
        let name = file
            .path
            .rsplit('/')
            .next()
            .unwrap_or(&file.path)
            .to_owned();
        let over_limit = file.size_bytes > IMAGE_PREVIEW_LIMIT_BYTES;
        let image = match (&file.preview, file.image && !over_limit) {
            (Some(bytes), true) => {
                let cached = self
                    .image
                    .as_ref()
                    .filter(|(generation, _)| *generation == file.generation)
                    .map(|(_, image)| image.clone());
                cached.or_else(|| {
                    let format = ImageFormat::from_mime_type(&file.mime)?;
                    let image = Arc::new(Image::from_bytes(format, bytes.to_vec()));
                    self.image = Some((file.generation.clone(), image.clone()));
                    Some(image)
                })
            }
            _ => None,
        };
        // Tauri's `auto auto` grid: a column of terms beside one of values.
        let facts = [
            ("Type", file.mime.clone()),
            ("Size", format_bytes(file.size_bytes)),
            ("Generation", file.generation.clone()),
        ];
        let column = |cells: Vec<SharedString>| v_flex().gap(ui(4.)).children(cells);
        v_flex()
            .id("binary-surface")
            .size_full()
            .items_center()
            .justify_center()
            .gap(ui(10.))
            .p(ui(28.))
            .overflow_y_scroll()
            .bg(palette.chrome_bg)
            .text_color(palette.chrome_dim)
            .map(|surface| match image {
                Some(image) => surface.child(
                    img(image)
                        .max_w(ui(900.))
                        .max_h(ui(650.))
                        .border_1()
                        .border_color(palette.chrome_border)
                        .object_fit(gpui_kit::ObjectFit::Contain)
                        // Bytes that claim an image format but do not decode.
                        .with_fallback(move || binary_icon(palette).border_0().into_any_element()),
                ),
                None => surface.child(binary_icon(palette)),
            })
            .child(
                div()
                    .id("binary-name")
                    .role(Role::Heading)
                    .text_size(ui(16.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(palette.chrome_ink)
                    .child(name),
            )
            .child(
                h_flex()
                    .items_start()
                    .gap(ui(14.))
                    .text_size(ui(TYPE_SECONDARY))
                    .child(
                        column(facts.iter().map(|(term, _)| (*term).into()).collect())
                            .text_color(palette.chrome_faint),
                    )
                    .child(column(
                        facts
                            .iter()
                            .map(|(_, value)| value.clone().into())
                            .collect(),
                    )),
            )
            .when(file.image && over_limit, |surface| {
                surface.child(
                    div()
                        .text_size(ui(TYPE_SECONDARY))
                        .child("Image preview is limited to 25 MiB."),
                )
            })
            .when(download, |surface| {
                surface.child(
                    Button::new("binary-download")
                        .primary()
                        .small()
                        .label("Download…")
                        .on_click(cx.listener(|this, _, window, cx| this.download(window, cx))),
                )
            })
    }
}

/// A tab with nothing to edit: why, and the file it names.
fn binary_icon(palette: Palette) -> Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .size(ui(56.))
        .border_1()
        .border_color(palette.chrome_border)
        .rounded(ui(12.))
        .bg(palette.chrome_raised)
        .font_family(fonts::MONO)
        .text_size(ui(16.))
        .child("01")
}

/// The message a tab shows in place of its content. Not a tab panel itself:
/// it is drawn inside one.
pub fn empty_tab(
    record: &AppTabRecord,
    detail: Option<&str>,
    download: Option<Button>,
    cx: &App,
) -> impl IntoElement + use<> {
    let palette: Palette = theme::palette(cx);
    let letter = match record.kind {
        AppTabKind::GitDiff => "±",
        AppTabKind::Markdown => "M",
        AppTabKind::File => "F",
    };
    v_flex()
        .id(SharedString::from(format!("app-tab-surface-{}", record.id)))
        .size_full()
        .items_center()
        .justify_center()
        .gap(ui(8.))
        .p(ui(32.))
        .bg(palette.chrome_bg)
        .text_color(palette.chrome_dim)
        .child(
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(ui(40.))
                .border_1()
                .border_color(palette.chrome_border)
                .rounded(ui(10.))
                .bg(palette.chrome_raised)
                .font_family(fonts::MONO)
                .text_size(ui(16.))
                .child(letter),
        )
        .child(
            div()
                .id("app-tab-title")
                .role(Role::Heading)
                .text_size(ui(16.))
                .font_weight(FontWeight::BOLD)
                .text_color(palette.chrome_ink)
                .child(record.title.clone()),
        )
        .child(
            div()
                .max_w(ui(680.))
                .px(ui(8.))
                .py(ui(5.))
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .border_1()
                .border_color(palette.chrome_border)
                .rounded(ui(4.))
                .bg(palette.chrome_raised)
                .font_family(fonts::MONO)
                .text_size(ui(TYPE_META))
                .child(record.resource.clone()),
        )
        .when_some(detail, |this, detail| {
            this.child(div().text_size(ui(TYPE_SECONDARY)).child(detail.to_owned()))
        })
        .children(download)
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.;
    let value = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if value < KIB * KIB {
        format!("{:.1} KiB", value / KIB)
    } else if value < KIB * KIB * KIB {
        format!("{:.1} MiB", value / (KIB * KIB))
    } else {
        format!("{:.1} GiB", value / (KIB * KIB * KIB))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_the_tauri_surface_wrote_them() {
        assert_eq!(format_bytes(12), "12 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(3 * 1024 * 1024), "3.0 MiB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
    }

    #[test]
    fn watches_the_files_directory() {
        assert_eq!(parent_path("/repo/src/main.rs"), "/repo/src");
        assert_eq!(parent_path("/main.rs"), "/");
    }

    #[test]
    fn a_single_file_capability_watches_the_file_itself() {
        let root = |token: &str| Root {
            path: "/repo".into(),
            token: token.into(),
        };
        assert_eq!(
            watched_path(&root("file-v2:abc"), "/elsewhere/notes.md"),
            "/elsewhere/notes.md"
        );
        assert_eq!(
            watched_path(&root("root"), "/repo/src/main.rs"),
            "/repo/src"
        );
    }
}

#[cfg(test)]
#[path = "pane_tests.rs"]
mod window_tests;
