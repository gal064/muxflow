//! Quick Open: files, workspaces and tabs. Port of
//! `apps/desktop/src/features/workspaces/{quickOpen.ts,useQuickOpenSearch.ts,WorkspaceSwitcher.tsx}`.
//!
//! The rows come from a [`QuickOpenSource`] the workspace and file features
//! install; until then Quick Open opens with nothing to list.

use std::{cmp::Reverse, collections::HashSet, ops::Range, time::Duration};

use gpui_kit::{
    App, AppContext, Context, Entity, Focusable, HighlightStyle, InteractiveElement, IntoElement,
    ParentElement, Render, Role, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    StyledText, Subscription, Task, TestSupportExt, Window,
    component::{
        Icon, Sizable, WindowExt, h_flex,
        input::{Input, InputEvent, InputState, MoveDown, MoveUp},
        v_flex,
    },
    div,
    prelude::FluentBuilder,
    svg, transparent_black,
};

use crate::ui::icons::Glyph;
use crate::{
    agents::{model::DisplayState, view::state_indicator},
    files::{
        icons::{Shape, file_icon},
        wire::FileKind,
    },
};

use super::{
    Commands,
    dialogs::{escape_ends_composition, is_composing, open_overlay},
    fuzzy::fuzzy_match,
};
use crate::theme::{
    self,
    metrics::{
        NARROW_QUICK_OPEN, RADIUS_CONTROL, RADIUS_PANEL, TYPE_LABEL, TYPE_MENU, TYPE_META,
        TYPE_SECONDARY, narrower_than, ui,
    },
};

const WIDTH: f32 = 680.;
const ROW_HEIGHT: f32 = 32.;
const LIST_MAX_HEIGHT: f32 = 448.;
const VIEWPORT_LIMIT: f32 = 0.55;
const MAX_RESULTS: usize = 75;
const MAX_QUERY_LENGTH: usize = 256;
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(100);
/// A name match always outranks a path-only match.
const NAME_MATCH_BONUS: i64 = 1000;

#[derive(Clone, Debug, PartialEq)]
pub struct QuickOpenWorkspace {
    pub key: String,
    pub name: String,
    /// Its metadata line and host label, joined.
    pub detail: String,
    pub active: bool,
    pub letter: Option<String>,
    /// The loudest agent state in it, while one wants showing.
    pub attention: Option<DisplayState>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuickOpenTab {
    pub key: String,
    pub title: String,
    /// `None` for a terminal; the open document's path otherwise.
    pub file_path: Option<String>,
    /// A document with a path to match, not some other app tab.
    pub is_file: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileMatch {
    pub path: String,
    pub relative_path: String,
    pub score: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FileSearchResults {
    pub matches: Vec<FileMatch>,
    pub complete: bool,
}

impl FileSearchResults {
    fn none() -> Self {
        Self {
            matches: Vec::new(),
            complete: true,
        }
    }
}

/// What Quick Open lists right now, read on every render.
#[derive(Clone, Debug, Default)]
pub struct QuickOpenSnapshot {
    pub workspaces: Vec<QuickOpenWorkspace>,
    pub tabs: Vec<QuickOpenTab>,
    pub active_tab_key: Option<String>,
    /// Identifies the searched root; `None` while there is nothing to search.
    pub search_key: Option<String>,
    pub cwd: Option<String>,
    pub host_label: String,
}

pub trait QuickOpenSource {
    fn snapshot(&self, cx: &App) -> QuickOpenSnapshot;
    fn search_files(&self, query: String, cx: &mut App) -> Task<Result<FileSearchResults, String>>;
    fn choose(&self, item: &QuickOpenItem, window: &mut Window, cx: &mut App);
}

#[derive(Clone, Debug, PartialEq)]
pub enum QuickOpenKind {
    Workspace(QuickOpenWorkspace),
    Tab(QuickOpenTab),
    File(FileMatch),
}

#[derive(Clone, Debug, PartialEq)]
pub struct QuickOpenItem {
    pub key: String,
    pub title: String,
    pub detail: String,
    pub current: bool,
    pub kind: QuickOpenKind,
}

/// Fixed section order: terminals, then workspaces, then files and other tabs.
fn group(item: &QuickOpenItem) -> usize {
    match &item.kind {
        QuickOpenKind::Tab(tab) if tab.file_path.is_none() => 0,
        QuickOpenKind::Workspace(_) => 1,
        _ => 2,
    }
}

pub fn quick_open_score(title: &str, detail: &str, query: &str) -> Option<i64> {
    match fuzzy_match(title, query) {
        Some(name) => Some(name.score + NAME_MATCH_BONUS),
        None => fuzzy_match(detail, query).map(|path| path.score),
    }
}

/// The rows to show, capped, always keeping the selected row among them.
pub fn quick_open_items(
    snapshot: &QuickOpenSnapshot,
    files: &[FileMatch],
    query: &str,
    selected_key: Option<&str>,
) -> Vec<QuickOpenItem> {
    let mut items: Vec<QuickOpenItem> = snapshot
        .tabs
        .iter()
        .map(|tab| QuickOpenItem {
            key: tab.key.clone(),
            title: tab.title.clone(),
            detail: tab
                .file_path
                .clone()
                .unwrap_or_else(|| "Terminal".to_owned()),
            current: snapshot.active_tab_key.as_deref() == Some(&tab.key),
            kind: QuickOpenKind::Tab(tab.clone()),
        })
        .collect();
    items.extend(snapshot.workspaces.iter().map(|workspace| QuickOpenItem {
        key: format!("workspace:{}", workspace.key),
        title: workspace.name.clone(),
        detail: workspace.detail.clone(),
        current: workspace.active,
        kind: QuickOpenKind::Workspace(workspace.clone()),
    }));
    if query.is_empty() {
        items.sort_by_key(group);
        items.truncate(MAX_RESULTS);
        return items;
    }
    let open_paths: HashSet<&str> = snapshot
        .tabs
        .iter()
        .filter(|tab| tab.is_file)
        .filter_map(|tab| tab.file_path.as_deref())
        .collect();
    items.extend(
        files
            .iter()
            .filter(|file| !open_paths.contains(file.path.as_str()))
            .map(|file| QuickOpenItem {
                key: format!("file:{}", file.path),
                title: file
                    .relative_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&file.relative_path)
                    .to_owned(),
                detail: file.relative_path.clone(),
                current: false,
                kind: QuickOpenKind::File(file.clone()),
            }),
    );
    let mut ranked: Vec<(usize, i64, QuickOpenItem)> = items
        .into_iter()
        .filter_map(|item| {
            let score = match &item.kind {
                QuickOpenKind::File(file) => Some(file.score),
                _ => quick_open_score(&item.title, &item.detail, query),
            }?;
            Some((group(&item), score, item))
        })
        .collect();
    ranked.sort_by_key(|(group, score, _)| (*group, Reverse(*score)));
    let selected = selected_key
        .and_then(|key| ranked.iter().find(|(_, _, item)| item.key == key))
        .map(|(_, _, item)| item.clone());
    let mut visible: Vec<QuickOpenItem> = ranked
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, _, item)| item)
        .collect();
    if let Some(selected) = selected
        && !visible.iter().any(|item| item.key == selected.key)
        && let Some(last) = visible.last_mut()
    {
        *last = selected;
    }
    visible
}

/// Byte ranges of the matched characters, for highlighting.
fn highlight_ranges(text: &str, query: &str) -> Vec<Range<usize>> {
    let Some(matched) = fuzzy_match(text, query) else {
        return Vec::new();
    };
    let indices: HashSet<usize> = matched.indices.into_iter().collect();
    text.char_indices()
        .enumerate()
        .filter(|(index, _)| indices.contains(index))
        .map(|(_, (start, character))| start..start + character.len_utf8())
        .collect()
}

fn highlighted(text: &str, query: &str, accent: gpui_kit::Rgba) -> StyledText {
    let style = HighlightStyle {
        color: Some(accent.into()),
        font_weight: Some(gpui_kit::FontWeight(650.)),
        ..Default::default()
    };
    let ranges = highlight_ranges(text, query)
        .into_iter()
        .map(|range| (range, style));
    StyledText::new(text.to_owned()).with_highlights(ranges)
}

pub struct QuickOpen {
    input: Entity<InputState>,
    scroll: ScrollHandle,
    items: Vec<QuickOpenItem>,
    /// The query and row last selected. The selection sticks to its row while
    /// results rerank, and starts over when the query changes.
    selection: (String, Option<String>),
    searched: (Option<String>, String),
    results: FileSearchResults,
    loading: bool,
    error: bool,
    _search: Option<Task<()>>,
    _subscriptions: [Subscription; 2],
}

impl QuickOpen {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search files, workspaces, and tabs…")
                .validate(|value, _| value.chars().count() <= MAX_QUERY_LENGTH)
        });
        let changes = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.scroll.scroll_to_item(0);
                this.sync_search(cx);
                cx.notify();
            }
        });
        let source = cx.observe_global::<Commands>(|this, cx| {
            this.sync_search(cx);
            cx.notify();
        });
        Self {
            input,
            scroll: ScrollHandle::new(),
            items: Vec::new(),
            selection: (String::new(), None),
            searched: (None, String::new()),
            results: FileSearchResults::none(),
            loading: false,
            error: false,
            _search: None,
            _subscriptions: [changes, source],
        }
    }

    fn normalized_query(&self, cx: &App) -> String {
        self.input.read(cx).value().trim().to_owned()
    }

    fn snapshot(cx: &App) -> QuickOpenSnapshot {
        cx.global::<Commands>()
            .quick_open
            .as_ref()
            .map(|source| source.snapshot(cx))
            .unwrap_or_default()
    }

    fn set_search_results(&mut self, results: FileSearchResults, error: bool) {
        self.results = results;
        self.loading = false;
        self.error = error;
    }

    /// Restarts the debounced, cancellable search when the query or the
    /// searched root changes.
    fn sync_search(&mut self, cx: &mut Context<Self>) {
        let query = self.normalized_query(cx);
        let snapshot = Self::snapshot(cx);
        let wanted = (snapshot.search_key.clone(), query.clone());
        if wanted == self.searched {
            return;
        }
        self.searched = wanted;
        self._search = None;
        self.set_search_results(FileSearchResults::none(), false);
        let Some(source) = cx.global::<Commands>().quick_open.clone() else {
            return;
        };
        if query.is_empty() || snapshot.search_key.is_none() {
            return;
        }
        self.loading = true;
        self._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let result = cx.update(|cx| source.search_files(query, cx)).await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(results) => this.set_search_results(results, false),
                    Err(_) => this.set_search_results(FileSearchResults::none(), true),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn refresh_items(&mut self, snapshot: &QuickOpenSnapshot, cx: &App) {
        let query = self.normalized_query(cx);
        let selected = (self.selection.0 == query)
            .then(|| self.selection.1.clone())
            .flatten();
        self.items = quick_open_items(snapshot, &self.results.matches, &query, selected.as_deref());
        let active = selected
            .filter(|key| self.items.iter().any(|item| &item.key == key))
            .or_else(|| self.items.first().map(|item| item.key.clone()));
        self.selection = (query, active);
    }

    fn active_index(&self) -> Option<usize> {
        let key = self.selection.1.as_ref()?;
        self.items.iter().position(|item| &item.key == key)
    }

    fn step(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if is_composing(&self.input, window, cx) {
            return;
        }
        cx.stop_propagation();
        let Some(index) = self.active_index() else {
            return;
        };
        let next = index
            .saturating_add_signed(delta)
            .min(self.items.len().saturating_sub(1));
        self.selection.1 = Some(self.items[next].key.clone());
        self.scroll.scroll_to_item(next);
        cx.notify();
    }

    fn choose(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(index).cloned() else {
            return;
        };
        window.close_dialog(cx);
        if let Some(source) = cx.global::<Commands>().quick_open.clone() {
            source.choose(&item, window, cx);
        }
    }

    fn choose_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if is_composing(&self.input, window, cx) {
            return;
        }
        if let Some(index) = self.active_index() {
            self.choose(index, window, cx);
        }
    }

    fn render_row(
        &self,
        index: usize,
        item: &QuickOpenItem,
        query: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let colors = theme::palette(cx);
        let selected = Some(index) == self.active_index();
        // A file wears its type's shape and color, as in the Explorer.
        let file_icon = |name: &str| {
            let (shape, tint) = file_icon(name, FileKind::File, None, false);
            svg()
                .data(shape.svg())
                .flex_none()
                .size(ui(16.))
                .text_color(tint.color(&colors))
                .into_any_element()
        };
        let glyph = |shape: Shape, color| {
            svg()
                .data(shape.svg())
                .flex_none()
                .size(ui(16.))
                .text_color(color)
                .into_any_element()
        };
        let (icon, letter, kind_label, attention) = match &item.kind {
            QuickOpenKind::File(_) => (file_icon(&item.title), None, "File", None),
            QuickOpenKind::Workspace(workspace) => (
                Icon::new(Glyph::SidebarLeft)
                    .xsmall()
                    .text_color(colors.term_6)
                    .into_any_element(),
                workspace.letter.clone(),
                "Workspace",
                workspace.attention,
            ),
            QuickOpenKind::Tab(tab) if tab.is_file => {
                (file_icon(&item.title), None, "Open tab", None)
            }
            QuickOpenKind::Tab(_) => (glyph(Shape::Shell, colors.chrome_dim), None, "Tab", None),
        };
        let glyphs = crate::settings::get(cx).agent_state_glyphs;
        let kind = if item.current {
            format!("{kind_label} · current")
        } else {
            kind_label.to_owned()
        };
        let key = item.key.clone();
        h_flex()
            .id(SharedString::from(format!("quick-open-{}", item.key)))
            .role(Role::ListBoxOption)
            .aria_label(item.title.clone())
            .aria_selected(selected)
            .h(ui(ROW_HEIGHT))
            .flex_none()
            .px(ui(9.))
            .gap(ui(9.))
            .rounded(ui(RADIUS_CONTROL))
            .border_1()
            .border_color(transparent_black())
            .when(selected, |row| {
                row.bg(colors.accent_wash)
                    .text_color(colors.chrome_ink_strong)
                    .border_color(colors.accent)
            })
            .when(!selected, |row| {
                row.hover(|row| row.bg(colors.chrome_hover))
            })
            .on_mouse_move(cx.listener(move |this, _, _, cx| {
                if this.selection.1.as_ref() != Some(&key) {
                    this.selection.1 = Some(key.clone());
                    cx.notify();
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| this.choose(index, window, cx)))
            .child(icon)
            .when_some(letter, |row, letter| {
                row.child(div().text_color(colors.accent).child(letter))
            })
            .child(div().flex_none().max_w_1_2().truncate().child(highlighted(
                &item.title,
                query,
                colors.accent,
            )))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(colors.chrome_dim)
                    .text_size(ui(TYPE_SECONDARY))
                    .child(highlighted(&item.detail, query, colors.accent)),
            )
            .when_some(attention, |row, state| {
                row.child(
                    div()
                        .id(SharedString::from(format!(
                            "quick-open-attention-{}",
                            item.key
                        )))
                        .role(Role::Image)
                        .aria_label(format!("Agent {}", state.word()))
                        // Tauri's 8px dot, or its plain 9px spinner.
                        .child(state_indicator(
                            state,
                            glyphs,
                            if state == DisplayState::Working {
                                9.
                            } else {
                                8.
                            },
                            colors.chrome_ink,
                            cx,
                        ))
                        .test_support(),
                )
            })
            .child(
                div()
                    .flex_none()
                    .text_size(ui(TYPE_LABEL))
                    .text_color(if selected {
                        colors.accent
                    } else {
                        colors.chrome_dim
                    })
                    .child(kind),
            )
            .test_support()
    }
}

pub fn open(window: &mut Window, cx: &mut App) {
    let quick_open = cx.new(|cx| QuickOpen::new(window, cx));
    let focus = quick_open.read(cx).input.focus_handle(cx);
    let view = quick_open.clone();
    open_overlay(
        quick_open,
        WIDTH,
        focus,
        true,
        move |window, cx| view.update(cx, |this, cx| this.choose_active(window, cx)),
        window,
        cx,
    );
}

impl Render for QuickOpen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = theme::palette(cx);
        // `styles.css` drops the scope's and footer's secondary labels this narrow.
        let narrow = narrower_than(NARROW_QUICK_OPEN, window);
        let snapshot = Self::snapshot(cx);
        // The focused pane's directory or connection changed under an open
        // Quick Open: search the new root, never show the old one's files.
        if snapshot.search_key != self.searched.0 {
            self.sync_search(cx);
        }
        self.refresh_items(&snapshot, cx);
        let query = self.selection.0.clone();
        let max_height = ui(LIST_MAX_HEIGHT)
            .to_pixels(window.rem_size())
            .min(window.viewport_size().height * VIEWPORT_LIMIT);
        let rows: Vec<_> = self
            .items
            .clone()
            .iter()
            .enumerate()
            .map(|(index, item)| self.render_row(index, item, &query, cx))
            .collect();
        let empty_text = if self.loading {
            "Searching filenames…"
        } else {
            "No matching names or paths."
        };
        let status = if self.error {
            Some("File search unavailable. Workspaces and tabs are still searchable.")
        } else if !self.results.complete {
            Some("Search limited—narrow your query.")
        } else {
            None
        };
        let hint = |text: &'static str| div().child(text);

        v_flex()
            .id("quick-open")
            .role(Role::Group)
            .aria_label("Quick Open")
            .capture_action(escape_ends_composition)
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| this.step(1, window, cx)))
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| this.step(-1, window, cx)))
            .child(
                div().p(ui(9.)).child(
                    h_flex()
                        .h(ui(36.))
                        .px(ui(11.))
                        .gap(ui(9.))
                        .border_1()
                        .border_color(colors.accent)
                        .rounded(ui(RADIUS_CONTROL))
                        .bg(colors.chrome_bg)
                        .child(
                            Icon::new(Glyph::Search)
                                .xsmall()
                                .text_color(colors.chrome_dim),
                        )
                        .child(
                            div().flex_1().min_w_0().child(
                                Input::new(&self.input)
                                    .id("quick-open-input")
                                    .role(Role::EditableComboBox)
                                    .aria_label("Search files, workspaces, and tabs")
                                    .appearance(false)
                                    .xsmall()
                                    .p_0()
                                    .text_size(ui(TYPE_MENU)),
                            ),
                        ),
                ),
            )
            .child(
                h_flex()
                    .justify_between()
                    .gap(ui(12.))
                    .px(ui(14.))
                    .pb(ui(7.))
                    .text_size(ui(TYPE_META))
                    .text_color(colors.chrome_dim)
                    .child({
                        let heading = if query.is_empty() {
                            "Open tabs & workspaces"
                        } else {
                            "Matches"
                        };
                        div()
                            .id("quick-open-heading")
                            .aria_label(heading)
                            .test_support()
                            .child(heading)
                    })
                    .child({
                        let count = if self.loading {
                            "Searching files…".to_owned()
                        } else {
                            format!("{} results", self.items.len())
                        };
                        div()
                            .id("quick-open-count")
                            .aria_label(SharedString::from(count.clone()))
                            .test_support()
                            .child(count)
                    }),
            )
            .child(if rows.is_empty() {
                div()
                    .id("quick-open-empty")
                    .aria_label(empty_text)
                    .test_support()
                    .px(ui(14.))
                    .py(ui(12.))
                    .text_color(colors.chrome_dim)
                    .child(empty_text)
                    .into_any_element()
            } else {
                v_flex()
                    .id("quick-open-results")
                    .role(Role::ListBox)
                    .aria_label("Results")
                    .max_h(max_height)
                    .px(ui(5.))
                    .pb(ui(5.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows)
                    .test_support()
                    .into_any_element()
            })
            .when_some(status, |this, status| {
                this.child(
                    div()
                        .id("quick-open-status")
                        .role(Role::Status)
                        .aria_label(status)
                        .test_support()
                        .px(ui(14.))
                        .py(ui(7.))
                        .border_t_1()
                        .border_color(colors.chrome_hairline)
                        .text_size(ui(TYPE_SECONDARY))
                        .text_color(colors.chrome_dim)
                        .child(status),
                )
            })
            .child(
                h_flex()
                    .gap(ui(7.))
                    .px(ui(14.))
                    .py(ui(8.))
                    .border_t_1()
                    .border_color(colors.chrome_hairline)
                    .text_size(ui(TYPE_META))
                    .text_color(colors.chrome_dim)
                    .child(Icon::new(Glyph::Folder).xsmall())
                    .child(
                        div().flex_1().min_w_0().truncate().child(
                            snapshot
                                .cwd
                                .clone()
                                .unwrap_or_else(|| "No focused terminal directory".to_owned()),
                        ),
                    )
                    .child({
                        let scope = if narrow {
                            snapshot.host_label.clone()
                        } else {
                            format!("{} · focused pane", snapshot.host_label)
                        };
                        div()
                            .id("quick-open-scope-host")
                            .test_support()
                            .aria_label(SharedString::from(scope.clone()))
                            .child(scope)
                    }),
            )
            .child(
                h_flex()
                    .gap(ui(14.))
                    .px(ui(14.))
                    .py(ui(8.))
                    .border_t_1()
                    .border_color(colors.chrome_hairline)
                    .bg(colors.chrome_bg)
                    .rounded_b(ui(RADIUS_PANEL))
                    .text_size(ui(TYPE_LABEL))
                    .text_color(colors.chrome_dim)
                    .when(narrow, |this| this.gap(ui(10.)))
                    .child(hint("↑ ↓ navigate"))
                    .child(hint("↵ open / switch"))
                    .child(hint("esc close"))
                    .when(!narrow, |this| {
                        this.child(div().ml_auto().child("Names & paths"))
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(key: &str, name: &str, active: bool) -> QuickOpenWorkspace {
        QuickOpenWorkspace {
            key: key.into(),
            name: name.into(),
            detail: format!("branch-{name}"),
            active,
            letter: None,
            attention: None,
        }
    }

    fn terminal(key: &str, title: &str) -> QuickOpenTab {
        QuickOpenTab {
            key: key.into(),
            title: title.into(),
            file_path: None,
            is_file: false,
        }
    }

    fn file_tab(key: &str, title: &str, path: &str) -> QuickOpenTab {
        QuickOpenTab {
            key: key.into(),
            title: title.into(),
            file_path: Some(path.into()),
            is_file: true,
        }
    }

    fn file(relative: &str, score: i64) -> FileMatch {
        FileMatch {
            path: format!("/root/{relative}"),
            relative_path: relative.into(),
            score,
        }
    }

    fn snapshot() -> QuickOpenSnapshot {
        QuickOpenSnapshot {
            workspaces: vec![workspace("w1", "api", false), workspace("w2", "web", true)],
            tabs: vec![
                file_tab("doc", "README.md", "/root/README.md"),
                terminal("t1", "shell"),
            ],
            active_tab_key: Some("t1".into()),
            ..Default::default()
        }
    }

    fn keys(items: &[QuickOpenItem]) -> Vec<&str> {
        items.iter().map(|item| item.key.as_str()).collect()
    }

    #[test]
    fn an_empty_query_lists_tabs_and_workspaces_in_section_order() {
        let items = quick_open_items(&snapshot(), &[file("x.rs", 5)], "", None);
        assert_eq!(keys(&items), ["t1", "workspace:w1", "workspace:w2", "doc"]);
        assert!(items[0].current);
        assert!(items[2].current);
    }

    #[test]
    fn an_open_file_appears_as_its_tab_only() {
        let files = [file("README.md", 9), file("src/readme.rs", 3)];
        let items = quick_open_items(&snapshot(), &files, "read", None);
        assert_eq!(keys(&items), ["doc", "file:/root/src/readme.rs"]);
        assert_eq!(items[1].title, "readme.rs");
        assert_eq!(items[1].detail, "src/readme.rs");
    }

    #[test]
    fn a_name_match_outranks_a_path_only_match_within_a_section() {
        let mut snap = snapshot();
        snap.workspaces = vec![workspace("a", "other", false), workspace("b", "web", false)];
        snap.workspaces[0].detail = "web-thing".into();
        let items = quick_open_items(&snap, &[], "web", None);
        assert_eq!(keys(&items), ["workspace:b", "workspace:a"]);
    }

    #[test]
    fn files_use_the_search_score_not_the_fuzzy_score() {
        let snap = QuickOpenSnapshot::default();
        let files = [file("a.rs", 1), file("b.rs", 50)];
        let items = quick_open_items(&snap, &files, "rs", None);
        assert_eq!(keys(&items), ["file:/root/b.rs", "file:/root/a.rs"]);
    }

    #[test]
    fn caps_at_75_but_keeps_the_selected_row() {
        let snap = QuickOpenSnapshot::default();
        let files: Vec<_> = (0..100)
            .map(|i| file(&format!("f{i}.rs"), 100 - i))
            .collect();
        let items = quick_open_items(&snap, &files, "f", Some("file:/root/f90.rs"));
        assert_eq!(items.len(), 75);
        assert_eq!(items[74].key, "file:/root/f90.rs");
    }

    #[test]
    fn highlight_ranges_are_byte_ranges_of_matched_characters() {
        let accented = highlight_ranges("añb", "b");
        assert_eq!(accented.len(), 1);
        assert_eq!(accented[0], 3..4);
        assert_eq!(highlight_ranges("abc", "ac"), [0..1, 2..3]);
        assert_eq!(highlight_ranges("abc", "x"), Vec::<Range<usize>>::new());
    }
}
