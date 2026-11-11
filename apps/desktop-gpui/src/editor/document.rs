//! One file tab's read, reconciliation and autosave. Port of
//! `features/files/useOpenFileTab.ts`.
//!
//! Everything about *when* the file is read and written lives here, and
//! nothing about how it is drawn: the one read in flight and its cancellation,
//! the content it produced, the autosave machine, and how a change on disk is
//! admitted over what is on screen. The text itself lives in the editor's
//! buffer, which the surface creates when it first draws the document.

use std::{rc::Rc, time::Duration};

use desktop_core::app_state::AppTabKind;

use gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, Subscription, Task, Window,
    component::{
        Rope,
        input::{EditorState, InputEvent},
    },
};

use crate::perf::{PaintReporter, PaintTicket};

use super::{
    autosave::{Autosave, SAVE_DELAY, SaveRequest, SaveState, Snapshot},
    io::{BinaryFile, FileIo, FileScope, Opened, Reply, Root, TEXT_FILE_LIMIT_BYTES},
    language,
};

/// How often a pending read or write is checked for its answer.
const POLL: Duration = Duration::from_millis(20);

/// What the tab has to show, decided once.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    Loading,
    /// The read failed and there is nothing on screen to keep.
    Failed(String),
    /// Content was on screen, and the file behind it changed or went away.
    Changed(String),
    Binary(BinaryFile),
    /// A text file over the editor's limit.
    TooLarge,
    Text,
}

/// What the document tells the app about itself.
#[derive(Clone, Debug, PartialEq)]
pub enum DocumentEvent {
    /// Clean to dirty: a preview tab stops being disposable.
    Dirty,
}

/// The live connection a document reads and writes through, if any now.
pub type ScopeFn = Box<dyn Fn(&App) -> Option<FileScope>>;

/// Where a document finds its live connection and whether it may write.
pub struct Live {
    pub scope: ScopeFn,
    pub can_write: Box<dyn Fn(&App) -> bool>,
}

/// The watch-side opinions about the file a document holds, delivered by the
/// file service (see [`super::deliver`]).
#[derive(Clone, Debug, PartialEq)]
pub enum FileNotice {
    /// A precise change event.
    Changed {
        generation: String,
        operation_id: Option<String>,
    },
    Deleted,
    /// An authoritative listing of the parent: the file's generation in it, or
    /// `None` when it does not mention the file or describes a symlink (whose
    /// identity does not move when its target is rewritten). `bootstrap` is
    /// the listing a watch armed for this document came with, `fresh` whether
    /// it describes the directory as of that arming.
    Listed {
        generation: Option<String>,
        bootstrap: bool,
        fresh: bool,
    },
}

/// Where the read and the watch's first listing have got to relative to each
/// other: either can land first, so the first has to wait for the second.
#[derive(Clone, Debug, PartialEq)]
enum Reconciliation {
    Pending,
    /// A listing arrived while nothing was on screen yet, parked for the read
    /// in flight then, named by its serial.
    Parked {
        generation: Option<String>,
        serial: u64,
    },
    Done,
}

pub struct Document {
    pub tab_id: String,
    pub host: String,
    pub server: String,
    pub resource: String,
    pub root: Root,
    io: Rc<dyn FileIo>,
    live: Rc<Live>,
    content: Content,
    autosave: Option<Autosave>,
    /// A read's error over content that is still on screen.
    error: Option<String>,
    binary_generation: Option<String>,
    editor: Option<Entity<EditorState>>,
    /// Text for the editor the next time it draws: a read or a change on disk.
    pending_text: Option<Rope>,
    load_serial: u64,
    /// The read in flight.
    load: Option<Task<()>>,
    /// The connection the last read was asked on. A failed read is asked
    /// again only on a new one.
    attempted: Option<FileScope>,
    /// The connection last seen live, and whether it could write.
    live_scope: Option<FileScope>,
    live_writable: bool,
    loaded: bool,
    reconciliation: Reconciliation,
    debounce: Option<Task<()>>,
    save: Option<Task<()>>,
    /// Advances whenever the buffer's text changes, edit or reload.
    revision: u64,
    /// A change from disk is going into the editor.
    applying: bool,
    /// `workflow.file.editorPaint`: from asking for the file, or for the
    /// editor, to the editor drawing it. Empty unless the perf probe is on.
    paint: PaintReporter,
    _subscriptions: Vec<Subscription>,
}

const EDITOR_PAINT: &[&str] = &["workflow.file.editorPaint"];

impl EventEmitter<DocumentEvent> for Document {}

impl Document {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        tab_id: String,
        host: String,
        server: String,
        resource: String,
        root: Root,
        io: Rc<dyn FileIo>,
        live: Rc<Live>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut document = Self {
            tab_id,
            host,
            server,
            resource,
            root,
            io,
            live,
            content: Content::Loading,
            autosave: None,
            error: None,
            binary_generation: None,
            editor: None,
            pending_text: None,
            load_serial: 0,
            load: None,
            attempted: None,
            live_scope: None,
            live_writable: false,
            loaded: false,
            reconciliation: Reconciliation::Pending,
            debounce: None,
            save: None,
            revision: 0,
            applying: false,
            paint: PaintReporter::default(),
            _subscriptions: Vec::new(),
        };
        document.follow_connection(cx);
        document
    }

    /// What to show. An error over shown content is a change, before it
    /// anything else.
    pub fn content(&self) -> Content {
        match (&self.error, &self.content) {
            // A read in flight wins over what an earlier one or an event said.
            (Some(_), Content::Loading) if self.load.is_some() => Content::Loading,
            (Some(error), Content::Loading) => Content::Failed(error.clone()),
            (Some(error), _) => Content::Changed(error.clone()),
            (None, content) => content.clone(),
        }
    }

    pub fn save_state(&self) -> Option<SaveState> {
        self.autosave
            .as_ref()
            .map(|autosave| autosave.current().state)
    }

    pub fn save_error(&self) -> Option<String> {
        self.autosave.as_ref()?.current().error.clone()
    }

    pub fn can_write(&self, cx: &App) -> bool {
        (self.live.can_write)(cx)
    }

    /// Whether a live connection serves this file now.
    pub fn connected(&self, cx: &App) -> bool {
        (self.live.scope)(cx).is_some()
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The buffer as it stands, for the Markdown preview.
    pub fn text(&self) -> Option<Rope> {
        Some(self.autosave.as_ref()?.current().content.clone())
    }

    /// The generation on screen: the autosave machine's for text, which every
    /// save advances, and the read's for anything else.
    fn shown_generation(&self) -> Option<String> {
        self.autosave
            .as_ref()
            .map(|autosave| autosave.current().generation.clone())
            .or_else(|| self.binary_generation.clone())
    }

    /// The editor for this document's text, made when it is first drawn.
    pub fn editor(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<EditorState> {
        let (editor, fresh) = match self.editor.clone() {
            Some(editor) => (editor, false),
            None => {
                let language = language::language_for_path(&self.resource);
                // Wrapped exactly when the tab is a Markdown tab, as in Tauri.
                let markdown = super::tabs::kind_for_path(&self.resource) == AppTabKind::Markdown;
                let editor = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(language)
                        .soft_wrap(markdown)
                        .searchable(false)
                        // Monaco draws fold arrows only while the pointer is over the
                        // gutter; Kit's stay on the caret's line, a stray arrow beside
                        // line 1.
                        .folding(false)
                });
                let subscription = cx.subscribe(&editor, |this, editor, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Change) {
                        let text = editor.read(cx).text().clone();
                        // The change from disk applied below is not an edit.
                        if std::mem::take(&mut this.applying)
                            && this.text().is_some_and(|known| known == text)
                        {
                            return;
                        }
                        this.edit(text, cx);
                    }
                });
                self._subscriptions.push(subscription);
                self.editor = Some(editor.clone());
                if self.pending_text.is_none() {
                    self.pending_text = self.text();
                }
                (editor, true)
            }
        };
        let Some(text) = self.pending_text.take() else {
            return editor;
        };
        self.revision += 1;
        let text = text.to_string();
        if fresh {
            editor.update(cx, |state, cx| state.set_value(text, window, cx));
            return editor;
        }
        // A change on disk replaces only the span that differs, as one undoable
        // step, so the view and the caret stay where the reader left them.
        editor.update(cx, |state, cx| {
            let old = state.text().to_string();
            let (start, old_end, new_end) = differing_span(&old, &text);
            if start == old_end && start == new_end {
                return;
            }
            let selection = state.selected_range();
            let shift = |at: usize| {
                if at <= start {
                    at
                } else if at >= old_end {
                    at - old_end + new_end
                } else {
                    new_end
                }
            };
            self.applying = true;
            state.set_selected_range(start..old_end, cx);
            state.replace(text[start..new_end].to_owned(), window, cx);
            state.set_selected_range(shift(selection.start)..shift(selection.end), cx);
        });
        editor
    }

    /// Asking for the editor (leaving the preview) measures its paint too,
    /// unless a read's measurement is already waiting.
    pub fn hold_editor_paint(&mut self) {
        if !self.paint.is_pending() {
            self.paint
                .hold(self.load_serial, PaintTicket::start(EDITOR_PAINT));
        }
    }

    /// The editor is drawing what the last read loaded: the measurement that
    /// read or request started ends with this frame.
    pub fn editor_paint_due(&mut self) -> Option<PaintTicket> {
        if self.load.is_some() || !self.loaded {
            return None;
        }
        self.paint.paintable(self.load_serial)
    }

    pub fn existing_editor(&self) -> Option<&Entity<EditorState>> {
        self.editor.as_ref()
    }

    /// Reads when the connection allows and nothing is on screen yet, and
    /// stops a read whose connection went away so it restarts once, when a
    /// live one returns. Content already read stays through a reconnect, and
    /// the new connection's watch revalidates it: changes made while away
    /// raised no event here.
    pub fn follow_connection(&mut self, cx: &mut Context<Self>) {
        let scope = (self.live.scope)(cx);
        let writable = (self.live.can_write)(cx);
        // The controller changes often; the tab redraws only for what it shows.
        if scope == self.live_scope && writable == self.live_writable {
            return;
        }
        if scope.is_some() && scope != self.live_scope && self.loaded {
            self.reconciliation = Reconciliation::Pending;
        }
        self.live_scope = scope.clone();
        self.live_writable = writable;
        match scope {
            None => {
                self.load = None;
                self.attempted = None;
            }
            Some(scope) if self.attempted.as_ref() != Some(&scope) => {
                if !self.loaded {
                    self.load(None, cx);
                    self.paint
                        .hold(self.load_serial, PaintTicket::start(EDITOR_PAINT));
                } else if self.load.is_some() {
                    // A reload asked on an earlier connection answers for
                    // that one only. It is dropped, not asked again: the new
                    // connection's watch revalidates what is on screen.
                    self.load = None;
                    self.load_serial += 1;
                    self.attempted = Some(scope);
                }
            }
            Some(_) => {}
        }
        cx.notify();
    }

    fn load(&mut self, external_operation: Option<String>, cx: &mut Context<Self>) {
        let Some(scope) = (self.live.scope)(cx) else {
            return;
        };
        self.load_serial += 1;
        let serial = self.load_serial;
        self.attempted = Some(scope.clone());
        let mut reply = self.io.read(&scope, &self.root, &self.resource);
        let task = cx.spawn(async move |this, cx| {
            let result = loop {
                if let Some(result) = reply.take() {
                    break result;
                }
                cx.background_executor().timer(POLL).await;
            };
            let _ = this.update(cx, |this, cx| {
                if serial != this.load_serial {
                    return;
                }
                this.load = None;
                this.loaded(result, serial, external_operation, cx);
                cx.notify();
            });
        });
        self.load = Some(task);
    }

    fn loaded(
        &mut self,
        result: Result<Opened, String>,
        serial: u64,
        external_operation: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let opened = match result {
            Ok(opened) => opened,
            Err(error) => {
                self.paint.discard(serial);
                self.error = Some(error);
                return;
            }
        };
        self.loaded = true;
        self.error = None;
        let generation = match &opened {
            Opened::Text(file) => file.generation.clone(),
            Opened::Binary(file) => file.generation.clone(),
        };
        // The watch's listing can land while the first read is in flight. It
        // is the authoritative listing, so a difference is a real change, and
        // the file is read once more.
        if let Reconciliation::Parked {
            generation: listed,
            serial: parked,
        } = &self.reconciliation
            && *parked == serial
        {
            let stale = listed.as_ref().is_some_and(|listed| *listed != generation);
            self.reconciliation = Reconciliation::Done;
            if stale {
                // Queued rather than called: re-entering `load` from its own
                // success path would invalidate the serial being applied.
                cx.spawn(async move |this, cx| {
                    let _ = this.update(cx, |this, cx| this.load(None, cx));
                })
                .detach();
            }
        }
        match opened {
            Opened::Binary(file) => {
                // No editor draws a binary or oversized file, so it never
                // publishes the editor's paint.
                self.paint.abandon();
                // A file that stopped being text has no editor and no autosave.
                self.autosave = None;
                self.debounce = None;
                self.binary_generation = Some(file.generation.clone());
                self.content = Content::Binary(file);
            }
            Opened::Text(file) => {
                self.binary_generation = None;
                if file.size_bytes > TEXT_FILE_LIMIT_BYTES {
                    self.paint.abandon();
                    self.autosave = None;
                    self.content = Content::TooLarge;
                    return;
                }
                let snapshot = Snapshot {
                    content: Rope::from(file.content.as_str()),
                    generation: file.generation,
                    line_ending: file.line_ending,
                };
                self.content = Content::Text;
                self.paint.note_committed(serial);
                match &mut self.autosave {
                    Some(autosave) => {
                        // A rescan also sees our own atomic rename: keep a
                        // newer local edit while disk has the generation we
                        // already know.
                        if external_operation.is_none()
                            && autosave.current().generation == snapshot.generation
                        {
                            return;
                        }
                        let text = snapshot.content.clone();
                        if autosave.external(snapshot, external_operation.as_deref()) {
                            self.debounce = None;
                            self.pending_text = Some(text);
                        }
                    }
                    None => {
                        self.pending_text = Some(snapshot.content.clone());
                        self.autosave = Some(Autosave::new(snapshot));
                    }
                }
            }
        }
    }

    /// A local edit from the editor.
    fn edit(&mut self, text: Rope, cx: &mut Context<Self>) {
        if !self.can_write(cx) {
            return;
        }
        let Some(autosave) = &mut self.autosave else {
            return;
        };
        let was = autosave.current().state;
        let revision = autosave.edit(text);
        self.revision += 1;
        if was != SaveState::Dirty {
            cx.emit(DocumentEvent::Dirty);
        }
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                this.debounce = None;
                if let Some(request) = this.autosave.as_mut().and_then(|a| a.due(revision)) {
                    this.start_save(request, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn start_save(&mut self, request: SaveRequest, cx: &mut Context<Self>) {
        let mut reply = match (self.live.scope)(cx) {
            Some(scope) => self.io.write(&scope, &self.root, &self.resource, &request),
            None => Reply::ready(Err("The file host is disconnected.".to_owned())),
        };
        self.save = Some(cx.spawn(async move |this, cx| {
            let result = loop {
                if let Some(result) = reply.take() {
                    break result;
                }
                cx.background_executor().timer(POLL).await;
            };
            let _ = this.update(cx, |this, cx| {
                this.save = None;
                let next = this
                    .autosave
                    .as_mut()
                    .and_then(|autosave| autosave.finished(&request, result));
                if let Some(next) = next {
                    this.start_save(next, cx);
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Saves whatever is owed and answers once it is written, or why not. A
    /// failed save is tried once more first.
    pub fn flush(document: &Entity<Self>, cx: &mut App) -> Task<Result<(), String>> {
        let document = document.clone();
        // The flush writes what the debounce would have; an edit typed while
        // it waits arms a debounce of its own, which stays.
        document.update(cx, |this, _| this.debounce = None);
        cx.spawn(async move |cx| {
            let mut tried = false;
            loop {
                let step = document.update(cx, |this, cx| {
                    let autosave = this.autosave.as_mut()?;
                    if autosave.saving() {
                        return None;
                    }
                    match autosave.current().state {
                        SaveState::Dirty | SaveState::Error if !tried => {
                            tried = true;
                            if let Some(request) = autosave.flush() {
                                this.start_save(request, cx);
                            }
                            None
                        }
                        SaveState::Error => {
                            Some(Err(autosave.current().error.clone().unwrap_or_else(|| {
                                "The file could not be saved.".to_owned()
                            })))
                        }
                        _ => Some(Ok(())),
                    }
                });
                match step {
                    Some(result) => return result,
                    None if document.read_with(cx, |this, _| this.autosave.is_none()) => {
                        return Ok(());
                    }
                    None => cx.background_executor().timer(POLL).await,
                }
            }
        })
    }

    /// What the file service says about this file. Port of the subscription,
    /// bootstrap and reload rules in `useOpenFileTab.ts`.
    pub fn notice(&mut self, notice: FileNotice, cx: &mut Context<Self>) {
        let shown = self.shown_generation();
        match notice {
            FileNotice::Deleted => {
                self.error = Some("The file was deleted externally. The tab remains open.".into());
            }
            FileNotice::Changed {
                generation,
                operation_id,
            } => {
                if !generation.is_empty() && Some(&generation) == shown.as_ref() {
                    return;
                }
                if shown.is_none() {
                    // Nothing on screen means the read that will put it there
                    // is in flight, and restarting it on every event would
                    // never let a slow read finish. Parked, not dropped.
                    self.park((!generation.is_empty()).then_some(generation));
                    return;
                }
                self.load(operation_id, cx);
            }
            FileNotice::Listed {
                generation,
                bootstrap,
                fresh,
            } => {
                if bootstrap {
                    if self.reconciliation == Reconciliation::Done {
                        return;
                    }
                    if !fresh {
                        // A watch someone else armed earlier: its listing has
                        // no opinion about a file read just now.
                        self.reconciliation = Reconciliation::Done;
                        return;
                    }
                }
                let Some(generation) = generation else {
                    // Absence proves nothing; only a delete event retires a tab.
                    if bootstrap && shown.is_some() {
                        self.reconciliation = Reconciliation::Done;
                    }
                    return;
                };
                if shown.is_none() {
                    self.park(Some(generation));
                    return;
                }
                if bootstrap {
                    self.reconciliation = Reconciliation::Done;
                }
                if Some(&generation) != shown.as_ref() {
                    self.reload_from_disk(cx);
                }
            }
        }
        cx.notify();
    }

    fn park(&mut self, generation: Option<String>) {
        self.reconciliation = Reconciliation::Parked {
            generation,
            serial: self.load_serial,
        };
    }

    /// Re-reads because an authoritative listing says the file moved, unless
    /// the buffer holds work that disk would overwrite.
    fn reload_from_disk(&mut self, cx: &mut Context<Self>) {
        if matches!(
            self.save_state(),
            Some(SaveState::Dirty | SaveState::Saving | SaveState::Error)
        ) {
            return;
        }
        self.load(None, cx);
    }
}

/// Where `old` and `new` differ: the common prefix's end, and the ends of the
/// differing spans in each, all on character boundaries.
fn differing_span(old: &str, new: &str) -> (usize, usize, usize) {
    let start = old
        .char_indices()
        .zip(new.chars())
        .find(|((_, a), b)| a != b)
        .map_or(old.len().min(new.len()), |((at, _), _)| at);
    let suffix = old[start..]
        .chars()
        .rev()
        .zip(new[start..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();
    // The editor never puts a caret inside a `\r\n`, so an edge that would
    // split one takes the whole pair; the pair is the same on both sides.
    let bytes = old.as_bytes();
    let start = if start > 0 && bytes[start - 1] == b'\r' {
        start - 1
    } else {
        start
    };
    let end = old.len() - suffix;
    let suffix = if suffix > 0 && end > 0 && bytes[end - 1] == b'\r' && bytes[end] == b'\n' {
        suffix - 1
    } else {
        suffix
    };
    (start, old.len() - suffix, new.len() - suffix)
}

#[cfg(test)]
#[path = "document_tests.rs"]
mod tests;
