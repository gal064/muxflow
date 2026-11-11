//! The Explorer's view: its header, the tree as a virtual list, its menus and
//! keys, and the Downloads section. Port of `ExplorerTree.tsx`,
//! `ExplorerRow.tsx`, `explorerWindow.ts` (GPUI's `uniform_list` lays out only
//! the rows in view) and `DownloadTransfers.tsx`.

use std::{collections::HashSet, rc::Rc};

use desktop_core::app_state::DockSide;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, FocusHandle, FontWeight, InteractiveElement,
    IntoElement, KeyDownEvent, ParentElement, Render, Role, ScrollStrategy, SharedString,
    StatefulInteractiveElement, Styled, UniformListScrollHandle, WeakEntity, Window,
    base::{Selectable, TestSupportExt},
    component::{
        Sizable,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{ContextMenuExt, PopupMenu},
        scroll::ScrollableElement,
        tooltip::Tooltip,
        v_flex,
    },
    div,
    prelude::FluentBuilder,
    svg, uniform_list,
};

use crate::{
    commands::{self, Commands, RowCommandSource, RowSurface, registry::CommandId},
    context_menu::{Entry, Item, fill},
    fonts,
    terminal::integration::transfers::InternalPathDrag,
    theme::{
        self, Palette,
        metrics::{RADIUS_CONTROL, TYPE_LABEL, TYPE_META, TYPE_ROW, TYPE_SECONDARY, ui},
    },
    transfers::TransferState,
    ui::error_summary::{self, Summary},
};

use super::{
    downloads::{DownloadIntent, DownloadKind, DownloadStatus, reveal_label},
    explorer::{DownloadOrigin, Explorer},
    icons::{CHEVRON_DOWN, CHEVRON_RIGHT, EYE, EYE_OFF, file_icon},
    wire::{DirectoryListing, FileEntry, FileKind},
};

/// Every row is exactly this tall, which is what lets the list place rows by
/// index.
const ROW_HEIGHT: f32 = 24.;

/// A file action the user started, bound to the root and connection it was
/// started under: the dialog outlives a keystroke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationAction {
    NewFile,
    NewDirectory,
    Rename,
    Move,
    Duplicate,
    Delete,
}

impl MutationAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::NewFile => "Create file",
            Self::NewDirectory => "Create folder",
            Self::Rename => "Rename",
            Self::Move => "Move",
            Self::Duplicate => "Duplicate",
            Self::Delete => "Delete",
        }
    }
}

#[derive(Clone)]
pub enum RowKind {
    Entry {
        listing: Rc<DirectoryListing>,
        index: usize,
    },
    More {
        directory: String,
    },
}

/// One row of the flattened tree. Positions are per level, as the tree role
/// asks, because only the rows in view exist.
#[derive(Clone)]
pub struct Row {
    pub kind: RowKind,
    pub depth: usize,
    pub position: usize,
    pub size: usize,
}

impl Row {
    pub fn entry(&self) -> Option<&FileEntry> {
        match &self.kind {
            RowKind::Entry { listing, index } => listing.entries.get(*index),
            RowKind::More { .. } => None,
        }
    }

    /// The row's identity, which is what the keyboard holds: a precise change
    /// inserts or removes rows without moving it to another file.
    /// Whether this row is the one `key` names, without building its key.
    fn is(&self, key: &str) -> bool {
        match &self.kind {
            RowKind::Entry { .. } => self.entry().is_some_and(|entry| entry.path == key),
            RowKind::More { directory } => key
                .strip_suffix("\0more")
                .is_some_and(|held| held == directory),
        }
    }

    pub fn key(&self) -> String {
        match &self.kind {
            RowKind::Entry { .. } => self
                .entry()
                .map(|entry| entry.path.clone())
                .unwrap_or_default(),
            RowKind::More { directory } => format!("{directory}\0more"),
        }
    }
}

/// The tree in display order. `hidden` paths are left out with everything
/// under them: the walk never descends into a directory it dropped.
pub fn flatten(
    root: &str,
    listings: &std::collections::HashMap<String, Rc<DirectoryListing>>,
    expanded: &HashSet<String>,
    hidden: Option<&HashSet<String>>,
) -> Vec<Row> {
    fn visit(
        directory: &str,
        depth: usize,
        listings: &std::collections::HashMap<String, Rc<DirectoryListing>>,
        expanded: &HashSet<String>,
        hidden: Option<&HashSet<String>>,
        rows: &mut Vec<Row>,
    ) {
        let Some(listing) = listings.get(directory) else {
            return;
        };
        let shown: Vec<usize> = (0..listing.entries.len())
            .filter(|index| {
                !hidden.is_some_and(|hidden| hidden.contains(&listing.entries[*index].path))
            })
            .collect();
        let more = !listing.complete && listing.next_page_token.is_some();
        let size = shown.len() + usize::from(more);
        for (offset, index) in shown.into_iter().enumerate() {
            let entry = &listing.entries[index];
            rows.push(Row {
                kind: RowKind::Entry {
                    listing: listing.clone(),
                    index,
                },
                depth,
                position: offset + 1,
                size,
            });
            if entry.expandable && expanded.contains(&entry.path) {
                visit(&entry.path, depth + 1, listings, expanded, hidden, rows);
            }
        }
        if more {
            rows.push(Row {
                kind: RowKind::More {
                    directory: directory.to_owned(),
                },
                depth,
                position: size,
                size,
            });
        }
    }
    let mut rows = Vec::new();
    visit(root, 0, listings, expanded, hidden, &mut rows);
    rows
}

/// The tooltip a row carries: its path, kind and size, age and link target.
pub fn entry_tooltip(entry: &FileEntry) -> String {
    let kind = match entry.kind {
        FileKind::File => "file",
        FileKind::Directory => "directory",
        FileKind::Symlink => "symlink",
    };
    let mut lines = vec![
        entry.path.clone(),
        format!("{kind} · {} bytes", entry.size_bytes),
    ];
    if entry.modified_millis > 0
        && let Some(modified) = chrono::DateTime::from_timestamp_millis(entry.modified_millis)
    {
        let local = modified.with_timezone(&chrono::Local);
        lines.push(format!(
            "Modified {}",
            local.format("%-m/%-d/%Y, %-I:%M:%S %p")
        ));
    }
    if let Some(target) = &entry.symlink_target {
        lines.push(format!("Symlink → {target}"));
    }
    lines.join("\n")
}

pub struct ViewState {
    pub focus: FocusHandle,
    pub scroll: UniformListScrollHandle,
    /// Which row has the keyboard, by identity.
    pub focus_key: Option<String>,
    pub last_focus_index: usize,
    /// A renamed or moved row: the keyboard goes to it once it is listed.
    pub follow: Option<String>,
    /// VS Code's escape hatch, reversible from the tree itself.
    pub show_ignored: bool,
    /// Whether the keyboard moved last: the ring shows only then, as
    /// `:focus-visible` does.
    pub keyboard: bool,
    /// A Downloads row's refusal to open, by transfer id.
    pub open_error: Option<(String, String)>,
    pub rows: Rc<Vec<Row>>,
    published: Option<Vec<CommandId>>,
    /// Whether the Files segment is drawn.
    pub(super) shown: bool,
}

impl ViewState {
    pub fn new(cx: &mut App) -> Self {
        Self {
            focus: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            focus_key: None,
            last_focus_index: 0,
            follow: None,
            show_ignored: false,
            keyboard: false,
            open_error: None,
            rows: Rc::default(),
            published: None,
            shown: false,
        }
    }

    /// A new root is a new repository; showing its ignored files is a choice
    /// made again.
    pub fn reset_for_root(&mut self) {
        self.show_ignored = false;
    }

    /// The focused row's index. A row that went away leaves the keyboard where
    /// it was rather than at the top.
    pub fn focus_index(&self) -> usize {
        self.focus_key
            .as_ref()
            .and_then(|key| self.rows.iter().position(|row| row.is(key)))
            .unwrap_or_else(|| self.last_focus_index.min(self.rows.len().saturating_sub(1)))
    }
}

impl Explorer {
    /// Recomputes the rows and keeps the keyboard on a row that still exists.
    pub(super) fn refresh_rows(&mut self, cx: &mut Context<Self>) {
        let ignored = super::ignored(cx);
        let hidden = (!self.view.show_ignored).then_some(ignored).flatten();
        self.view.rows = Rc::new(match &self.tree.root {
            Some(root) => flatten(
                &root.path,
                &self.tree.listings,
                &self.tree.expanded,
                hidden.as_deref(),
            ),
            None => Vec::new(),
        });
        let followed = self
            .view
            .follow
            .as_ref()
            .and_then(|key| self.view.rows.iter().position(|row| row.is(key)));
        if let Some(index) = followed {
            self.view.focus_key = self.view.follow.take();
            self.view
                .scroll
                .scroll_to_item(index, ScrollStrategy::Nearest);
        }
        let index = self.view.focus_index();
        self.view.last_focus_index = index;
        self.view.focus_key = self
            .view
            .rows
            .get(index)
            .map(Row::key)
            .or(self.view.focus_key.take());
        self.publish_row_commands(cx);
    }

    pub(super) fn focused_entry(&self) -> Option<FileEntry> {
        self.view
            .rows
            .get(self.view.focus_index())
            .and_then(Row::entry)
            .cloned()
    }

    fn focus_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.view.rows.get(index) else {
            return;
        };
        self.view.focus_key = Some(row.key());
        self.view.last_focus_index = index;
        self.view.follow = None;
        self.view
            .scroll
            .scroll_to_item(index, ScrollStrategy::Nearest);
        self.view.focus.focus(window, cx);
        self.publish_row_commands(cx);
        cx.notify();
    }

    pub(super) fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        self.view.shown = shown;
        if !shown {
            self.due_paints.clear();
        }
        self.publish_row_commands(cx);
    }

    /// The Files commands the palette offers for the keyboard's row: none
    /// while the segment is hidden, and nothing that changes files while the
    /// host is read-only.
    pub(super) fn row_command_ids(&self) -> Vec<CommandId> {
        let mut ids: Vec<CommandId> = Vec::new();
        if self.view.shown && self.tree.root.is_some() {
            let disabled = !self.inputs.can_mutate;
            if let Some(entry) = self.focused_entry() {
                if entry.kind != FileKind::Directory {
                    ids.push("files.open");
                }
                ids.push("files.download");
                if !disabled {
                    ids.extend([
                        "files.rename",
                        "files.move",
                        "files.duplicate",
                        "files.delete",
                    ]);
                }
            }
            if !disabled {
                ids.extend(["files.newFile", "files.newFolder"]);
            }
            ids.push("files.refresh");
        }
        ids
    }

    /// The palette's Files commands, for the row the keyboard is on, while
    /// the segment is shown. Only the id list is published again when it
    /// changes.
    fn publish_row_commands(&mut self, cx: &mut Context<Self>) {
        if !cx.has_global::<Commands>() {
            return;
        }
        let ids = self.row_command_ids();
        if self.view.published.as_ref() == Some(&ids) {
            return;
        }
        self.view.published = Some(ids.clone());
        let weak = cx.entity().downgrade();
        let source = (!ids.is_empty()).then(|| RowCommandSource {
            available: ids,
            run: Rc::new(move |id, window, cx| {
                weak.update(cx, |this, cx| this.run_row_command(id, window, cx))
                    .ok();
            }),
        });
        commands::publish_row_commands(RowSurface::Files, source, cx);
    }

    pub(super) fn run_row_command(
        &mut self,
        id: CommandId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entry = self.focused_entry();
        match id {
            "files.open" => {
                if let Some(entry) = entry {
                    self.open_entry(&entry, false, false, window, cx);
                }
            }
            "files.rename" => self.begin(MutationAction::Rename, entry, window, cx),
            "files.move" => self.begin(MutationAction::Move, entry, window, cx),
            "files.duplicate" => self.begin(MutationAction::Duplicate, entry, window, cx),
            "files.delete" => self.begin(MutationAction::Delete, entry, window, cx),
            "files.download" => {
                if let Some(entry) = entry {
                    self.download_entry(&entry, window, cx);
                }
            }
            "files.newFile" => self.begin(MutationAction::NewFile, entry, window, cx),
            "files.newFolder" => self.begin(MutationAction::NewDirectory, entry, window, cx),
            "files.refresh" => self.refresh(None, cx),
            _ => {}
        }
    }

    fn begin(
        &mut self,
        action: MutationAction,
        entry: Option<FileEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.tree.root.clone() else {
            return;
        };
        let scope_identity = self.scope_identity();
        drop(super::dialog::open(
            cx.entity(),
            action,
            entry,
            root,
            scope_identity,
            window,
            cx,
        ));
    }

    /// The connection a pending file action belongs to.
    pub(super) fn scope_identity(&self) -> String {
        self.inputs
            .scope
            .as_ref()
            .map_or_else(|| "disconnected".to_owned(), |scope| scope.key())
    }

    /// A single click previews, a deliberate open pins; `dock` opens beside
    /// the terminal.
    fn open_entry(
        &mut self,
        entry: &FileEntry,
        preview: bool,
        dock: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target_is_file = entry.kind == FileKind::File
            || (entry.kind == FileKind::Symlink
                && entry.target_kind == Some(super::wire::TargetKind::File));
        let (Some(selection), Some(root)) = (self.inputs.selection.clone(), self.tree.root.clone())
        else {
            return;
        };
        if !target_is_file {
            return;
        }
        // Beside whatever is docked already, as Tauri reused its side.
        let side = dock
            .then(|| crate::workspaces::current(cx).map(|w| w.read(cx).view(cx).dock_side()))
            .map(|side| side.unwrap_or(DockSide::Right));
        let request = crate::editor::OpenFile {
            session_id: selection.session_id,
            path: entry.path.clone(),
            root: crate::editor::Root {
                path: root.path,
                token: root.token,
            },
            preview,
            refresh_root: false,
            dock: side,
        };
        // Deferred: the editor updates the workspace this row is drawn in.
        cx.defer_in(window, move |_, _, cx| {
            crate::editor::open_file(request, cx)
        });
    }

    fn download_entry(&mut self, entry: &FileEntry, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.tree.root.clone() else {
            return;
        };
        let intent = DownloadIntent {
            path: entry.path.clone(),
            kind: if entry.kind == FileKind::Directory {
                DownloadKind::Folder
            } else {
                DownloadKind::File
            },
        };
        // After this update: the download updates the Explorer itself.
        let explorer = cx.entity();
        window.defer(cx, move |window, cx| {
            super::download(explorer, intent, root, DownloadOrigin::Explorer, window, cx)
        });
    }

    fn toggle_ignored(&mut self, cx: &mut Context<Self>) {
        self.view.show_ignored = !self.view.show_ignored;
        self.refresh_rows(cx);
        cx.notify();
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.view.rows.clone();
        if rows.is_empty() {
            return;
        }
        let index = self.view.focus_index();
        let last = rows.len() - 1;
        let row = &rows[index];
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt {
            return;
        }
        self.view.keyboard = true;
        match event.keystroke.key.as_str() {
            "down" => self.focus_row((index + 1).min(last), window, cx),
            "up" => self.focus_row(index.saturating_sub(1), window, cx),
            // The tree's own keys: without them the end of a large directory is
            // thousands of presses away.
            "home" => self.focus_row(0, window, cx),
            "end" => self.focus_row(last, window, cx),
            "right" => match row.entry() {
                Some(entry) if entry.expandable && !self.tree.expanded.contains(&entry.path) => {
                    let path = entry.path.clone();
                    self.toggle_directory(&path, cx);
                }
                Some(_)
                    if rows
                        .get(index + 1)
                        .is_some_and(|next| next.depth == row.depth + 1) =>
                {
                    self.focus_row(index + 1, window, cx)
                }
                _ => return,
            },
            "left" => match row.entry() {
                Some(entry) if entry.expandable && self.tree.expanded.contains(&entry.path) => {
                    let path = entry.path.clone();
                    self.toggle_directory(&path, cx);
                }
                Some(_) => {
                    if let Some(parent) = (0..index).rev().find(|at| rows[*at].depth < row.depth) {
                        self.focus_row(parent, window, cx);
                    }
                }
                None => return,
            },
            // Enter pins, as a double-click does.
            "enter" | "space" => match (row.entry().cloned(), &row.kind) {
                (Some(entry), _) if entry.expandable => self.toggle_directory(&entry.path, cx),
                (Some(entry), _) => self.open_entry(&entry, false, false, window, cx),
                // The keyboard reaches the next page as a click does.
                (None, RowKind::More { directory }) => {
                    if !self.tree.loading.contains(directory) {
                        let directory = directory.clone();
                        self.load_more(&directory, cx);
                    }
                }
                (None, _) => return,
            },
            _ => return,
        }
        cx.stop_propagation();
    }

    fn entry_menu(&self, entry: &FileEntry, cx: &Context<Self>) -> Vec<Entry> {
        let weak = cx.entity().downgrade();
        let disabled = !self.inputs.can_mutate;
        let act = |run: fn(&mut Explorer, FileEntry, &mut Window, &mut Context<Explorer>)| {
            let (weak, entry) = (weak.clone(), entry.clone());
            move |window: &mut Window, cx: &mut App| {
                weak.update(cx, |this, cx| run(this, entry.clone(), window, cx))
                    .ok();
            }
        };
        let mut entries: Vec<Entry> = Vec::new();
        if entry.kind != FileKind::Directory {
            entries.push(
                Item::new(
                    "Open",
                    act(|this, entry, window, cx| {
                        this.open_entry(&entry, false, false, window, cx)
                    }),
                )
                .into(),
            );
            entries.push(
                Item::new(
                    "Open to the Side",
                    act(|this, entry, window, cx| this.open_entry(&entry, false, true, window, cx)),
                )
                .into(),
            );
        }
        for (label, action) in [
            ("Rename…", MutationAction::Rename),
            ("Move…", MutationAction::Move),
            ("Duplicate…", MutationAction::Duplicate),
        ] {
            let (weak, entry) = (weak.clone(), entry.clone());
            entries.push(
                Item::new(label, move |window, cx| {
                    weak.update(cx, |this, cx| {
                        this.begin(action, Some(entry.clone()), window, cx)
                    })
                    .ok();
                })
                .disabled(disabled)
                .into(),
            );
        }
        let download = if entry.kind == FileKind::Directory {
            "Download folder…"
        } else {
            "Download…"
        };
        entries.push(
            Item::new(
                download,
                act(|this, entry, window, cx| this.download_entry(&entry, window, cx)),
            )
            .into(),
        );
        entries.push(Entry::Separator);
        for (label, action) in [
            ("New file…", MutationAction::NewFile),
            ("New folder…", MutationAction::NewDirectory),
        ] {
            let (weak, entry) = (weak.clone(), entry.clone());
            entries.push(
                Item::new(label, move |window, cx| {
                    weak.update(cx, |this, cx| {
                        this.begin(action, Some(entry.clone()), window, cx)
                    })
                    .ok();
                })
                .disabled(disabled)
                .into(),
            );
        }
        entries.push(Entry::Separator);
        let (weak_delete, delete_entry) = (weak.clone(), entry.clone());
        entries.push(
            Item::new("Delete…", move |window, cx| {
                weak_delete
                    .update(cx, |this, cx| {
                        this.begin(
                            MutationAction::Delete,
                            Some(delete_entry.clone()),
                            window,
                            cx,
                        )
                    })
                    .ok();
            })
            .destructive()
            .disabled(disabled)
            .into(),
        );
        if let Some(toggle) = self.ignored_toggle(&weak, cx) {
            entries.push(Entry::Separator);
            entries.push(toggle);
        }
        entries
    }

    /// The empty area's menu acts on the root.
    fn root_menu(&self, cx: &Context<Self>) -> Vec<Entry> {
        let weak = cx.entity().downgrade();
        let disabled = !self.inputs.can_mutate || self.tree.root.is_none();
        let mut entries: Vec<Entry> = Vec::new();
        for (label, action) in [
            ("New file…", MutationAction::NewFile),
            ("New folder…", MutationAction::NewDirectory),
        ] {
            let weak = weak.clone();
            entries.push(
                Item::new(label, move |window, cx| {
                    weak.update(cx, |this, cx| this.begin(action, None, window, cx))
                        .ok();
                })
                .disabled(disabled)
                .into(),
            );
        }
        entries.push(Entry::Separator);
        if let Some(toggle) = self.ignored_toggle(&weak, cx) {
            entries.push(toggle);
        }
        entries.push(
            Item::new("Refresh", move |_, cx| {
                weak.update(cx, |this, cx| this.refresh(None, cx)).ok();
            })
            .into(),
        );
        entries
    }

    fn ignored_label(&self) -> &'static str {
        if self.view.show_ignored {
            "Hide ignored files"
        } else {
            "Show ignored files"
        }
    }

    /// Offered only when git has said there is something to hide.
    fn ignored_toggle(&self, weak: &WeakEntity<Self>, cx: &App) -> Option<Entry> {
        super::ignored(cx).filter(|ignored| !ignored.is_empty())?;
        let weak = weak.clone();
        Some(
            Item::new(self.ignored_label(), move |_, cx| {
                weak.update(cx, |this, cx| this.toggle_ignored(cx)).ok();
            })
            .into(),
        )
    }

    /// `focused` is the keyboard's row; `ring` whether the tree holds focus,
    /// which shows it as `:focus-visible` would.
    fn render_row(
        &self,
        index: usize,
        row: &Row,
        focused: bool,
        ring: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let palette = theme::palette(cx);
        let indent = 8. + row.depth as f32 * 14.;
        let weak = cx.entity().downgrade();
        let base = div()
            .id(("explorer-row", index))
            .test_support()
            .h(ui(ROW_HEIGHT))
            .w_full()
            .flex()
            .items_center()
            .flex_none()
            .pl(ui(indent))
            .pr(ui(5.))
            .aria_level(row.depth + 1)
            .aria_position_in_set(row.position)
            .aria_size_of_set(row.size)
            .aria_selected(focused)
            .when(focused && ring, |this| {
                this.border_2().border_color(palette.accent).rounded(ui(3.))
            });
        let Some(entry) = row.entry().cloned() else {
            let RowKind::More { directory } = &row.kind else {
                return base.into_any_element();
            };
            // Busy, not removed: it keeps its place and the keyboard.
            let busy = self.tree.loading.contains(directory);
            let directory = directory.clone();
            return base
                .role(Role::TreeItem)
                .aria_label("Load more…")
                .child(
                    div()
                        .h_full()
                        .flex()
                        .items_center()
                        .px(ui(8.))
                        .border_1()
                        .border_color(palette.chrome_border)
                        .rounded(ui(4.))
                        .bg(palette.chrome_raised)
                        .text_color(palette.chrome_dim)
                        .text_size(ui(TYPE_META))
                        .when(busy, |this| this.opacity(0.6))
                        .child("Load more…"),
                )
                .on_click(move |_, window, cx| {
                    weak.update(cx, |this, cx| {
                        this.focus_row(index, window, cx);
                        if !busy {
                            this.load_more(&directory, cx);
                        }
                    })
                    .ok();
                })
                .into_any_element();
        };
        let open = entry.expandable && self.tree.expanded.contains(&entry.path);
        let (shape, tint) = file_icon(&entry.name, entry.kind, entry.target_kind, open);
        let tooltip: SharedString = entry_tooltip(&entry).into();
        let menu = self.entry_menu(&entry, cx);
        let drag = self
            .inputs
            .selection
            .as_ref()
            .filter(|_| self.paints_selection())
            .map(|selection| InternalPathDrag {
                host_profile_id: selection.host_profile_id.clone(),
                server_identity: selection.server_identity.clone(),
                path: entry.path.clone(),
            });
        let click_entry = entry.clone();
        base.role(Role::TreeItem)
            .aria_label(entry.name.clone())
            .when(entry.expandable, |this| this.aria_expanded(open))
            .when(focused, |this| {
                this.bg(palette.chrome_selected).when(!ring, |this| {
                    this.border_1().border_color(palette.chrome_selected_edge)
                })
            })
            // The selected fill wins over hover, as in the Tauri stylesheet.
            .when(!focused, |this| {
                this.hover(|style| style.bg(palette.chrome_hover))
            })
            .gap(ui(4.))
            .text_size(ui(TYPE_ROW))
            .text_color(palette.chrome_ink)
            .child(div().flex_none().w(ui(12.)).when(entry.expandable, |this| {
                this.child(
                    svg()
                        .data(if open { CHEVRON_DOWN } else { CHEVRON_RIGHT })
                        .size(ui(11.))
                        .text_color(palette.chrome_faint),
                )
            }))
            .child(
                svg()
                    .data(shape.svg())
                    .flex_none()
                    .size(ui(14.))
                    .text_color(tint.color(&palette)),
            )
            // On the name only, so it never sits over an open context menu.
            .child(
                div()
                    .id(("explorer-row-name", index))
                    .min_w_0()
                    .truncate()
                    .child(entry.name.clone())
                    .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx)),
            )
            // Any press selects the row first, so a menu or a drag acts on
            // the row it shows.
            .on_any_mouse_down({
                let weak = weak.clone();
                move |_, window, cx| {
                    weak.update(cx, |this, cx| {
                        this.view.keyboard = false;
                        this.focus_row(index, window, cx)
                    })
                    .ok();
                }
            })
            .on_click(move |event, window, cx| {
                let entry = click_entry.clone();
                weak.update(cx, |this, cx| {
                    this.focus_row(index, window, cx);
                    if entry.expandable {
                        // Every click toggles, as the Tauri row's button does.
                        this.toggle_directory(&entry.path, cx);
                    } else {
                        // The first click previews; the second pins the same tab.
                        this.open_entry(&entry, event.click_count() < 2, false, window, cx);
                    }
                })
                .ok();
            })
            .when_some(drag, |this, drag| {
                this.on_drag(drag, |drag, _, _, cx| {
                    let name = drag.path.rsplit('/').next().unwrap_or_default().to_owned();
                    cx.new(|_| DragGhost(name))
                })
            })
            .context_menu(move |popup: PopupMenu, window, cx| fill(popup, &menu, window, cx))
            .into_any_element()
    }
}

/// What follows the pointer while a row is dragged.
struct DragGhost(String);

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = theme::palette(cx);
        div()
            .px(ui(6.))
            .py(ui(2.))
            .rounded(ui(RADIUS_CONTROL))
            .bg(palette.chrome_raised)
            .border_1()
            .border_color(palette.chrome_border)
            .text_size(ui(TYPE_SECONDARY))
            .text_color(palette.chrome_ink)
            .child(self.0.clone())
    }
}

/// A rejection as a sentence, with the host's diagnostic one disclosure
/// away. Port of `ui/SurfaceError.tsx`.
pub(super) fn surface_error(
    id: &'static str,
    raw: &str,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let palette = theme::palette(cx);
    let Summary { summary, detail } = error_summary::summarize(raw);
    // The detail shown open; a new rejection opens closed.
    let opened = window.use_keyed_state(SharedString::from(format!("{id}-details")), cx, |_, _| {
        None::<String>
    });
    let open = detail.is_some() && opened.read(cx).as_deref() == detail.as_deref();
    div()
        .id(id)
        .role(Role::Alert)
        .aria_label(SharedString::from(summary.clone()))
        .flex()
        .flex_col()
        .items_start()
        .gap(ui(4.))
        .px(ui(8.))
        .py(ui(6.))
        .rounded(ui(RADIUS_CONTROL))
        .bg(palette.danger_wash)
        .text_color(palette.danger_ink)
        .text_size(ui(TYPE_SECONDARY))
        .child(summary)
        .when_some(detail, |banner, detail| {
            let shown = detail.clone();
            banner
                .child(
                    Button::new(SharedString::from(format!("{id}-toggle")))
                        .ghost()
                        .xsmall()
                        .label(if open { "▾ Details" } else { "▸ Details" })
                        .on_click(move |_, _, cx| {
                            opened.update(cx, |held, cx| {
                                *held = (!open).then(|| shown.clone());
                                cx.notify();
                            })
                        }),
                )
                .when(open, |banner| {
                    banner.child(
                        div()
                            .id(SharedString::from(format!("{id}-detail")))
                            .role(Role::Paragraph)
                            .aria_label(SharedString::from(detail.clone()))
                            .max_h(ui(132.))
                            .overflow_y_scroll()
                            .text_color(palette.chrome_ink)
                            .font_family(fonts::MONO)
                            .text_size(ui(TYPE_META))
                            .child(detail),
                    )
                })
        })
        .into_any_element()
}

fn quiet(text: impl Into<SharedString>, palette: &Palette) -> AnyElement {
    div()
        .px(ui(12.))
        .py(ui(10.))
        .text_size(ui(TYPE_SECONDARY))
        .text_color(palette.chrome_faint)
        .child(text.into())
        .into_any_element()
}

impl Render for Explorer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for ticket in self.due_paints.drain(..) {
            crate::perf::after_next_frame(Some(ticket), window);
        }
        let palette = theme::palette(cx);
        let root = self.tree.root.clone();
        let root_name = root
            .as_ref()
            .map(|root| {
                root.path
                    .split('/')
                    .rfind(|part| !part.is_empty())
                    .unwrap_or(&root.path)
                    .to_owned()
            })
            .unwrap_or_else(|| "No active root".to_owned());
        let has_ignored = super::ignored(cx).is_some_and(|ignored| !ignored.is_empty());
        let ignored_label = self.ignored_label();
        let root_menu = self.root_menu(cx);
        let header_menu = root_menu.clone();
        let header = h_flex()
            .id("explorer-root")
            .flex_none()
            .items_center()
            .gap(ui(7.))
            .min_h(ui(40.))
            .px(ui(12.))
            .py(ui(5.))
            .border_b_1()
            .border_color(palette.chrome_hairline)
            .child({
                let path: Option<SharedString> = root.as_ref().map(|root| root.path.clone().into());
                div()
                    .id("explorer-root-name")
                    .min_w_0()
                    .truncate()
                    .text_size(ui(TYPE_ROW))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(palette.chrome_ink)
                    .child(root_name)
                    .when_some(path, |this, path| {
                        this.tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
                    })
            })
            .when_some(root.as_ref(), |this, root| {
                this.child(
                    div()
                        .flex_none()
                        .text_size(ui(TYPE_LABEL))
                        .text_color(palette.chrome_faint)
                        .child(if root.git_worktree {
                            "git worktree"
                        } else {
                            "pane cwd"
                        }),
                )
            })
            // The one wait shown for a listing already on screen, outside the
            // scrolling tree so it cannot change its height.
            .when(self.requested_reads() > 0, |this| {
                this.child(
                    div()
                        .id("explorer-refreshing")
                        .role(Role::Status)
                        .ml_auto()
                        .text_size(ui(TYPE_LABEL))
                        .text_color(palette.state_working)
                        .child("Refreshing…"),
                )
            })
            .when(has_ignored, |this| {
                let showing = self.view.show_ignored;
                this.child(
                    Button::new("explorer-ignored-toggle")
                        .ghost()
                        .xsmall()
                        .ml_auto()
                        .w(ui(24.))
                        .h(ui(24.))
                        .accessibility_label(ignored_label)
                        .tooltip(ignored_label)
                        .selected(showing)
                        .child(
                            svg()
                                .data(if showing { EYE } else { EYE_OFF })
                                .size(ui(14.))
                                .text_color(if showing {
                                    palette.accent
                                } else {
                                    palette.chrome_dim
                                }),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_ignored(cx))),
                )
            })
            .context_menu(move |menu: PopupMenu, window, cx| fill(menu, &header_menu, window, cx));

        let error = self.tree.error.clone().map(|text| {
            div()
                .flex_none()
                .mx(ui(12.))
                .my(ui(6.))
                .child(surface_error("explorer-error", &text, window, cx))
        });

        let rows = self.view.rows.clone();
        let empty: Option<AnyElement> = match &root {
            None => Some(quiet("Select a live terminal pane.", &palette)),
            Some(root) if !self.tree.listings.contains_key(&root.path) => self
                .tree
                .loading
                .contains(&root.path)
                .then(|| quiet("Loading…", &palette)),
            Some(root) if rows.is_empty() => {
                let held = self
                    .tree
                    .listings
                    .get(&root.path)
                    .map_or(0, |l| l.entries.len());
                let hiding = !self.view.show_ignored && has_ignored;
                Some(quiet(
                    if hiding && held > 0 {
                        "Everything here is ignored by git. Use the eye button in the Explorer header to show ignored files."
                    } else {
                        "This directory is empty."
                    },
                    &palette,
                ))
            }
            Some(_) => None,
        };
        let tree = div()
            .id("explorer-tree")
            .test_support()
            .role(Role::Tree)
            .aria_label("Files")
            .track_focus(&self.view.focus)
            .on_key_down(cx.listener(Self::key_down))
            .relative()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            // A message sits under the header, where the rows would start.
            .children(empty.is_none().then(|| {
                uniform_list(
                    "explorer-rows",
                    rows.len(),
                    cx.processor(|this, range: std::ops::Range<usize>, window, cx| {
                        let rows = this.view.rows.clone();
                        // Found by key, so once per frame rather than per row.
                        let focus = this.view.focus_index();
                        let ring = this.view.keyboard && this.view.focus.is_focused(window);
                        range
                            .filter_map(|index| {
                                rows.get(index).map(|row| {
                                    this.render_row(index, row, index == focus, ring, cx)
                                })
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.view.scroll)
                .flex_1()
                .min_h_0()
                .pb(ui(12.))
            }))
            .when(empty.is_none(), |this| {
                this.vertical_scrollbar(&self.view.scroll)
            })
            .children(empty)
            .context_menu(move |menu: PopupMenu, window, cx| fill(menu, &root_menu, window, cx));

        v_flex()
            .id("explorer")
            .size_full()
            .min_h_0()
            .overflow_hidden()
            .child(header)
            .children(error)
            .child(tree)
            .children(self.render_downloads(&palette, cx))
    }
}

impl Explorer {
    /// The Explorer's Downloads list. Open and reveal are invoked directly, and
    /// a refusal is shown on the row that caused it.
    fn render_downloads(&self, palette: &Palette, cx: &Context<Self>) -> Option<AnyElement> {
        let items = &self.downloads.items;
        if items.is_empty() {
            return None;
        }
        let finished = items.iter().any(|item| item.state.is_terminal());
        let header = h_flex()
            .justify_between()
            .items_center()
            .mb(ui(5.))
            .child(
                div()
                    .text_size(ui(TYPE_LABEL))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(palette.chrome_dim)
                    .child("DOWNLOADS"),
            )
            .when(finished, |this| {
                this.child(
                    Button::new("downloads-clear")
                        .ghost()
                        .xsmall()
                        .label("Clear all")
                        .accessibility_label("Clear finished downloads")
                        .on_click(cx.listener(|this, _, _, cx| this.clear_finished_downloads(cx))),
                )
            });
        Some(
            v_flex()
                .id("downloads")
                .role(Role::Region)
                .aria_label("Downloads")
                .flex_none()
                .max_h(ui(180.))
                .overflow_y_scroll()
                .p(ui(7.))
                .border_t_1()
                .border_color(palette.chrome_hairline)
                .child(header)
                .children(
                    items
                        .iter()
                        .enumerate()
                        .map(|(index, item)| self.render_download(index, item, palette, cx)),
                )
                .into_any_element(),
        )
    }

    fn render_download(
        &self,
        index: usize,
        item: &DownloadStatus,
        palette: &Palette,
        cx: &Context<Self>,
    ) -> AnyElement {
        let name = item.path.rsplit('/').next().unwrap_or_default().to_owned();
        let alert = |id: &'static str, text: String| {
            div()
                .id((id, index))
                .role(Role::Alert)
                .text_color(palette.danger_ink)
                .child(text)
        };
        let detail = |text: String| div().text_color(palette.chrome_dim).child(text);
        let permille = item.progress_permille();
        let edge = match item.state {
            TransferState::Verifying => Some(palette.state_working),
            TransferState::Failed => Some(palette.danger),
            _ => None,
        };
        let id = item.id.clone();
        let published = item.outcome == Some(crate::transfers::TransferOutcome::Published);
        let open_error = self
            .view
            .open_error
            .as_ref()
            .filter(|(failed, _)| *failed == item.id)
            .map(|(_, message)| message.clone());
        v_flex()
            .id(("download", index))
            .role(Role::ListItem)
            .aria_label(format!("Download {}: {}", item.path, item.state.label()))
            .when(index > 0, |this| this.mt(ui(4.)))
            .gap(ui(3.))
            .p(ui(5.))
            .rounded(ui(4.))
            .bg(palette.chrome_raised)
            .text_size(ui(TYPE_META))
            .when_some(edge, |this, edge| this.border_l_2().border_color(edge))
            .child(
                h_flex()
                    .gap(ui(7.))
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(div().text_color(palette.chrome_dim).child(item.state.label())),
            )
            .child(
                div()
                    .id(("download-progress", index))
                    .role(Role::ProgressIndicator)
                    .aria_label(format!("Download progress for {}", item.path))
                    .when_some(permille, |this, value| this.aria_numeric_value(f64::from(value)))
                    .h(ui(4.))
                    .w_full()
                    .rounded(ui(2.))
                    .bg(palette.chrome_hairline)
                    .child(
                        div()
                            .h_full()
                            .rounded(ui(2.))
                            .bg(palette.accent)
                            .w(gpui_kit::relative(permille.unwrap_or(0) as f32 / 1000.)),
                    ),
            )
            .child(detail(item.progress_text()))
            .when(item.state.can_cancel(), |this| {
                let id = id.clone();
                this.child(
                    h_flex().justify_end().child(
                        Button::new(("download-cancel", index))
                            .ghost()
                            .xsmall()
                            .label("Cancel")
                            .accessibility_label(format!("Cancel download {}", item.path))
                            .on_click(cx.listener(move |this, _, _, cx| this.cancel_download(&id, cx))),
                    ),
                )
            })
            // A published local file is what can be opened, whatever the state
            // says: a scope that went stale after publishing reports `failed`.
            .when_some(item.destination.clone().filter(|_| published), |this, destination| {
                let reveal = reveal_label();
                let act = |reveal_it: bool| {
                    let (id, destination) = (id.clone(), destination.clone());
                    cx.listener(move |this: &mut Explorer, _, _, cx| {
                        let transport = this.transport();
                        let result = if reveal_it {
                            transport.reveal_download(destination.clone())
                        } else {
                            transport.open_download(destination.clone())
                        };
                        // A later success clears an earlier refusal.
                        this.view.open_error = result.err().map(|error| (id.clone(), error));
                        cx.notify();
                    })
                };
                this.child(
                    h_flex()
                        .justify_end()
                        .gap(ui(6.))
                        .child(
                            Button::new(("download-open", index))
                                .ghost()
                                .xsmall()
                                .label("Open")
                                .accessibility_label(format!("Open {destination}"))
                                .on_click(act(false)),
                        )
                        .child(
                            Button::new(("download-reveal", index))
                                .ghost()
                                .xsmall()
                                .label(reveal)
                                .accessibility_label(format!("{reveal}: {destination}"))
                                .on_click(act(true)),
                        ),
                )
            })
            .when_some(open_error, |this, message| this.child(alert("download-open-error", message)))
            .when(item.state == TransferState::Verifying, |this| {
                this.child(
                    div()
                        .id(("download-finalizing", index))
                        .role(Role::Status)
                        .text_color(palette.state_working)
                        .child("The verified bytes are being committed; awaiting the authoritative backend outcome."),
                )
            })
            .when(item.failure_kind == Some(crate::transfers::TransferFailureKind::StaleScope), |this| {
                this.child(alert("download-stale", "Download stopped because the connection scope changed.".into()))
            })
            .when(item.failure_kind == Some(crate::transfers::TransferFailureKind::Timeout), |this| {
                this.child(alert("download-timeout", "Download timed out before an authoritative result arrived.".into()))
            })
            .when(item.outcome == Some(crate::transfers::TransferOutcome::Unknown), |this| {
                this.child(alert(
                    "download-unknown",
                    "The download outcome is unknown. Inspect the destination before retrying.".into(),
                ))
            })
            .when_some(item.error.clone(), |this, error| this.child(alert("download-error", error)))
            .when_some(item.cleanup_error.clone(), |this, error| {
                this.child(alert("download-cleanup-error", format!("Partial cleanup failed: {error}")))
            })
            .when_some(
                item.cleanup_status
                    .filter(|_| matches!(item.state, TransferState::Failed | TransferState::Cancelled)),
                |this, status| this.child(detail(format!("Cleanup: {}", status.word()))),
            )
            .into_any_element()
    }
}
