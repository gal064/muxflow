//! A document against a scripted host: when it reads, what it keeps through a
//! reconnect, how it autosaves, and how it answers what the host says about
//! its file. Ports the rules `useOpenFileTab.ts` held and the
//! `editorChunkBoundary` cases about state surviving a reconnect and an
//! unmount.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use gpui_kit::{Entity, TestAppContext};

use super::*;
use crate::editor::{
    autosave::LineEnding,
    io::{BinaryFile, Slot, TextFile},
};

/// Whether the document told the host to stop a transfer.
type Cancelled = Rc<Cell<bool>>;

struct Write {
    content: String,
    base: String,
    operation: String,
    slot: Slot<String>,
    cancelled: Cancelled,
}

#[derive(Default)]
struct Host {
    reads: RefCell<Vec<(FileScope, Slot<Opened>, Cancelled)>>,
    writes: RefCell<Vec<Write>>,
}

struct Io(Rc<Host>);

impl FileIo for Io {
    fn read(&self, scope: &FileScope, _: &Root, _: &str) -> Reply<Opened> {
        let (reply, slot, cancelled) = Reply::pending();
        self.0
            .reads
            .borrow_mut()
            .push((scope.clone(), slot, cancelled));
        reply
    }

    fn write(&self, _: &FileScope, _: &Root, _: &str, save: &SaveRequest) -> Reply<String> {
        let (reply, slot, cancelled) = Reply::pending();
        self.0.writes.borrow_mut().push(Write {
            content: save.content.to_string(),
            base: save.generation.clone(),
            operation: save.operation_id.clone(),
            slot,
            cancelled,
        });
        reply
    }
}

struct Fixture {
    host: Rc<Host>,
    scope: Rc<RefCell<Option<FileScope>>>,
    writable: Rc<Cell<bool>>,
    document: Entity<Document>,
    events: Rc<RefCell<Vec<DocumentEvent>>>,
}

fn scope(epoch: u64) -> FileScope {
    FileScope {
        client_id: format!("client-{epoch}"),
        profile_id: "local".into(),
        server_identity: "server-a".into(),
        terminal_epoch: epoch,
    }
}

fn open(cx: &mut TestAppContext, connected: bool) -> Fixture {
    let host = Rc::new(Host::default());
    let scope_cell = Rc::new(RefCell::new(connected.then(|| scope(1))));
    let writable = Rc::new(Cell::new(true));
    let live = {
        let (scope_cell, writable) = (scope_cell.clone(), writable.clone());
        Rc::new(Live {
            scope: Box::new(move |_| scope_cell.borrow().clone()),
            can_write: Box::new(move |_| writable.get()),
        })
    };
    let io: Rc<dyn FileIo> = Rc::new(Io(host.clone()));
    let document = cx.new(|cx| {
        Document::new(
            "tab".into(),
            "local".into(),
            "server-a".into(),
            "/repo/a.rs".into(),
            Root {
                path: "/repo".into(),
                token: "root".into(),
            },
            io,
            live,
            cx,
        )
    });
    let events = Rc::new(RefCell::new(Vec::new()));
    {
        let events = events.clone();
        cx.update(|cx| {
            cx.subscribe(&document, move |_, event: &DocumentEvent, _| {
                events.borrow_mut().push(event.clone());
            })
            .detach();
        });
    }
    Fixture {
        host,
        scope: scope_cell,
        writable,
        document,
        events,
    }
}

fn text(content: &str, generation: &str) -> Result<Opened, String> {
    Ok(Opened::Text(TextFile {
        content: content.into(),
        generation: generation.into(),
        size_bytes: content.len() as u64,
        line_ending: LineEnding::detect(content),
    }))
}

fn settle(cx: &mut TestAppContext) {
    cx.executor().advance_clock(POLL);
    cx.run_until_parked();
}

fn answer<T>(slot: &Slot<T>, result: Result<T, String>, cx: &mut TestAppContext) {
    *slot.lock().unwrap() = Some(result);
    settle(cx);
}

impl Fixture {
    fn read(&self, index: usize, result: Result<Opened, String>, cx: &mut TestAppContext) {
        let slot = self.host.reads.borrow()[index].1.clone();
        answer(&slot, result, cx);
    }

    fn write(&self, index: usize, result: Result<String, String>, cx: &mut TestAppContext) {
        let slot = self.host.writes.borrow()[index].slot.clone();
        answer(&slot, result, cx);
    }

    fn read_cancelled(&self, index: usize) -> bool {
        self.host.reads.borrow()[index].2.get()
    }

    fn reads(&self) -> usize {
        self.host.reads.borrow().len()
    }

    fn writes(&self) -> usize {
        self.host.writes.borrow().len()
    }

    fn content(&self, cx: &mut TestAppContext) -> Content {
        self.document
            .read_with(cx, |document, _| document.content())
    }

    fn text(&self, cx: &mut TestAppContext) -> String {
        self.document
            .read_with(cx, |document, _| document.text().unwrap().to_string())
    }

    fn state(&self, cx: &mut TestAppContext) -> Option<SaveState> {
        self.document
            .read_with(cx, |document, _| document.save_state())
    }

    fn edit(&self, text: &str, cx: &mut TestAppContext) {
        self.document
            .update(cx, |document, cx| document.edit(Rope::from(text), cx));
    }

    fn reconnect(&self, next: Option<FileScope>, cx: &mut TestAppContext) {
        *self.scope.borrow_mut() = next;
        self.document
            .update(cx, |document, cx| document.follow_connection(cx));
        cx.run_until_parked();
    }

    fn notice(&self, notice: FileNotice, cx: &mut TestAppContext) {
        self.document
            .update(cx, |document, cx| document.notice(notice, cx));
        cx.run_until_parked();
    }

    fn flush(&self, cx: &mut TestAppContext) -> Rc<RefCell<Option<Result<(), String>>>> {
        let out = Rc::new(RefCell::new(None));
        let document = self.document.clone();
        let sink = out.clone();
        cx.update(|cx| {
            let flush = Document::flush(&document, cx);
            cx.spawn(async move |_| *sink.borrow_mut() = Some(flush.await))
                .detach();
        });
        cx.run_until_parked();
        out
    }
}

fn saved_after_debounce(cx: &mut TestAppContext) {
    cx.executor().advance_clock(SAVE_DELAY);
    cx.run_until_parked();
}

#[gpui_kit::test]
fn reads_once_on_a_live_connection_and_shows_the_text(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    assert_eq!(fixture.reads(), 1);
    assert_eq!(fixture.content(cx), Content::Loading);
    fixture.read(0, text("hello\n", "1"), cx);
    assert_eq!(fixture.content(cx), Content::Text);
    assert_eq!(fixture.text(cx), "hello\n");
    assert_eq!(fixture.state(cx), Some(SaveState::Saved));
}

#[gpui_kit::test]
fn waits_for_a_live_connection_and_asks_again_only_on_a_new_one(cx: &mut TestAppContext) {
    let fixture = open(cx, false);
    assert_eq!(fixture.reads(), 0);
    fixture.reconnect(Some(scope(1)), cx);
    assert_eq!(fixture.reads(), 1);
    fixture.read(0, Err("File unavailable.".into()), cx);
    assert_eq!(
        fixture.content(cx),
        Content::Failed("File unavailable.".into())
    );
    // The same connection does not hammer a file that cannot be read.
    fixture.reconnect(Some(scope(1)), cx);
    assert_eq!(fixture.reads(), 1);
    fixture.reconnect(Some(scope(2)), cx);
    assert_eq!(fixture.reads(), 2);
    fixture.read(1, text("back", "4"), cx);
    assert_eq!(fixture.content(cx), Content::Text);
}

#[gpui_kit::test]
fn a_read_cut_off_by_a_disconnect_restarts_once_when_the_connection_returns(
    cx: &mut TestAppContext,
) {
    let fixture = open(cx, true);
    fixture.reconnect(None, cx);
    assert!(
        fixture.read_cancelled(0),
        "the host stops the abandoned read"
    );
    // An answer to the abandoned read changes nothing.
    fixture.read(0, text("stale", "1"), cx);
    assert_eq!(fixture.content(cx), Content::Loading);
    fixture.reconnect(Some(scope(2)), cx);
    assert_eq!(fixture.reads(), 2);
    fixture.read(1, text("fresh", "2"), cx);
    assert_eq!(fixture.text(cx), "fresh");
}

#[gpui_kit::test]
fn a_dirty_draft_survives_a_reconnect_without_an_eager_reread(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    fixture.edit("draft", cx);
    fixture.reconnect(None, cx);
    fixture.reconnect(Some(scope(2)), cx);
    assert_eq!(fixture.reads(), 1, "content already read is not read again");
    assert_eq!(fixture.text(cx), "draft");
    assert_eq!(fixture.state(cx), Some(SaveState::Dirty));
    saved_after_debounce(cx);
    assert_eq!(fixture.writes(), 1);
    assert_eq!(fixture.host.writes.borrow()[0].content, "draft");
}

#[gpui_kit::test]
fn a_reconnect_lets_the_new_watch_revalidate_what_is_on_screen(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    let bootstrap = |generation: &str| FileNotice::Listed {
        generation: Some(generation.into()),
        bootstrap: true,
        fresh: true,
    };
    fixture.notice(bootstrap("1"), cx);
    fixture.reconnect(None, cx);
    fixture.reconnect(Some(scope(2)), cx);
    assert_eq!(fixture.reads(), 1);
    // Changed while the connection was away, which raised no event.
    fixture.notice(bootstrap("2"), cx);
    assert_eq!(fixture.reads(), 2);
    fixture.read(1, text("b", "2"), cx);
    assert_eq!(fixture.text(cx), "b");
}

#[gpui_kit::test]
fn a_reload_asked_on_an_earlier_connection_is_dropped_for_the_new_watch(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    fixture.notice(
        FileNotice::Changed {
            generation: "2".into(),
            operation_id: None,
        },
        cx,
    );
    assert_eq!(fixture.reads(), 2);
    fixture.reconnect(Some(scope(2)), cx);
    assert_eq!(fixture.reads(), 2, "not asked again on the new connection");
    assert!(fixture.read_cancelled(1), "the obsolete reload is stopped");
    assert!(
        !fixture.read_cancelled(0),
        "an answered read has nothing to stop"
    );
    fixture.read(1, Err("terminal_epoch_rejected: stale".into()), cx);
    assert_eq!(
        fixture.content(cx),
        Content::Text,
        "the stale answer is dropped"
    );
    assert_eq!(fixture.text(cx), "a");
    fixture.notice(
        FileNotice::Listed {
            generation: Some("2".into()),
            bootstrap: true,
            fresh: true,
        },
        cx,
    );
    assert_eq!(fixture.reads(), 3, "the new watch's listing revalidates");
    fixture.read(2, text("b", "2"), cx);
    assert_eq!(fixture.text(cx), "b");
}

#[test]
fn a_change_from_disk_replaces_only_whole_line_breaks() {
    assert_eq!(
        super::differing_span("one\ntwo", "zero\none\ntwo"),
        (0, 0, 5)
    );
    assert_eq!(super::differing_span("a\r\nb\r\n", "a\nb\n"), (1, 6, 4));
    assert_eq!(super::differing_span("a\nb", "a\r\nb"), (1, 1, 2));
    assert_eq!(super::differing_span("same", "same"), (4, 4, 4));
}

#[gpui_kit::test]
fn autosaves_after_the_debounce_on_the_generation_it_read(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    fixture.edit("ab", cx);
    fixture.edit("abc", cx);
    assert_eq!(
        fixture.writes(),
        0,
        "the debounce waits for the last keystroke"
    );
    saved_after_debounce(cx);
    assert_eq!(fixture.writes(), 1);
    {
        let writes = fixture.host.writes.borrow();
        assert_eq!(
            (writes[0].content.as_str(), writes[0].base.as_str()),
            ("abc", "1")
        );
    }
    fixture.write(0, Ok("2".into()), cx);
    assert_eq!(fixture.state(cx), Some(SaveState::Saved));
    fixture.edit("abcd", cx);
    saved_after_debounce(cx);
    assert_eq!(fixture.host.writes.borrow()[1].base, "2");
}

#[gpui_kit::test]
fn a_failed_save_says_why_and_a_flush_tries_once_more(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    fixture.edit("ab", cx);
    saved_after_debounce(cx);
    let conflict = "stale_file_generation: file changed since it was opened";
    fixture.write(0, Err(conflict.into()), cx);
    assert_eq!(fixture.state(cx), Some(SaveState::Error));
    assert_eq!(
        fixture.document.read_with(cx, |d, _| d.save_error()),
        Some(conflict.into())
    );
    let flushed = fixture.flush(cx);
    assert_eq!(fixture.writes(), 2, "the flush retries the failed save");
    fixture.write(1, Err(conflict.into()), cx);
    settle(cx);
    assert_eq!(*flushed.borrow(), Some(Err(conflict.into())));
}

#[gpui_kit::test]
fn a_pending_save_is_written_when_flushed_before_its_debounce(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    fixture.edit("ab", cx);
    let flushed = fixture.flush(cx);
    assert_eq!(
        fixture.writes(),
        1,
        "a flush does not wait out the debounce"
    );
    assert_eq!(*flushed.borrow(), None);
    fixture.write(0, Ok("2".into()), cx);
    // The flush looks again on its own timer.
    settle(cx);
    assert_eq!(*flushed.borrow(), Some(Ok(())));
    assert_eq!(fixture.state(cx), Some(SaveState::Saved));
}

#[gpui_kit::test]
fn an_external_change_wins_and_this_clients_own_echo_does_not_reload(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    fixture.notice(
        FileNotice::Changed {
            generation: "2".into(),
            operation_id: None,
        },
        cx,
    );
    assert_eq!(fixture.reads(), 2);
    fixture.read(1, text("from disk", "2"), cx);
    assert_eq!(fixture.text(cx), "from disk");

    fixture.edit("mine", cx);
    saved_after_debounce(cx);
    let operation = fixture.host.writes.borrow()[0].operation.clone();
    fixture.write(0, Ok("3".into()), cx);
    fixture.notice(
        FileNotice::Changed {
            generation: "3".into(),
            operation_id: Some(operation),
        },
        cx,
    );
    assert_eq!(
        fixture.reads(),
        2,
        "the echo of our own save is not a change"
    );
    assert_eq!(fixture.text(cx), "mine");
}

#[gpui_kit::test]
fn a_deleted_file_keeps_its_tab_and_says_so(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    fixture.notice(FileNotice::Deleted, cx);
    assert_eq!(
        fixture.content(cx),
        Content::Changed("The file was deleted externally. The tab remains open.".into())
    );
}

#[gpui_kit::test]
fn a_listing_that_lands_before_the_first_read_is_reconciled_once(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.notice(
        FileNotice::Listed {
            generation: Some("2".into()),
            bootstrap: true,
            fresh: true,
        },
        cx,
    );
    assert_eq!(fixture.reads(), 1, "a read in flight is not restarted");
    fixture.read(0, text("old", "1"), cx);
    assert_eq!(fixture.reads(), 2, "the listing said the file moved");
    fixture.read(1, text("new", "2"), cx);
    assert_eq!(fixture.text(cx), "new");
    fixture.notice(
        FileNotice::Listed {
            generation: Some("2".into()),
            bootstrap: true,
            fresh: true,
        },
        cx,
    );
    assert_eq!(fixture.reads(), 2);
}

#[gpui_kit::test]
fn a_read_in_flight_is_never_restarted_however_often_the_file_changes(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    for generation in ["2", "3", "4"] {
        fixture.notice(
            FileNotice::Changed {
                generation: generation.into(),
                operation_id: None,
            },
            cx,
        );
    }
    assert_eq!(fixture.reads(), 1);
    fixture.read(0, text("old", "1"), cx);
    assert_eq!(fixture.reads(), 2, "the file moved under the read, once");
    fixture.read(1, text("new", "4"), cx);
    assert_eq!(fixture.text(cx), "new");
    assert_eq!(fixture.reads(), 2);
}

#[gpui_kit::test]
fn a_rescan_reloads_only_when_it_carries_a_newer_generation_for_this_file(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    let listed = |generation: Option<&str>| FileNotice::Listed {
        generation: generation.map(Into::into),
        bootstrap: false,
        fresh: true,
    };
    fixture.notice(listed(None), cx);
    fixture.notice(listed(Some("1")), cx);
    assert_eq!(
        fixture.reads(),
        1,
        "silence and the same generation are no news"
    );
    assert_eq!(fixture.content(cx), Content::Text, "absence keeps the tab");
    fixture.notice(listed(Some("2")), cx);
    assert_eq!(fixture.reads(), 2);
    fixture.read(1, text("b", "2"), cx);
    assert_eq!(fixture.text(cx), "b");
}

#[gpui_kit::test]
fn a_listing_never_reloads_over_unsaved_work(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    fixture.edit("unsaved", cx);
    fixture.notice(
        FileNotice::Listed {
            generation: Some("9".into()),
            bootstrap: false,
            fresh: false,
        },
        cx,
    );
    assert_eq!(fixture.reads(), 1);
    assert_eq!(fixture.text(cx), "unsaved");
}

#[gpui_kit::test]
fn binary_and_oversized_files_get_no_editor_and_no_autosave(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    let binary = BinaryFile {
        path: "/repo/a.rs".into(),
        generation: "1".into(),
        size_bytes: 3,
        mime: "application/octet-stream".into(),
        image: false,
        preview: None,
    };
    fixture.read(0, Ok(Opened::Binary(binary.clone())), cx);
    assert_eq!(fixture.content(cx), Content::Binary(binary));
    assert_eq!(fixture.state(cx), None);

    let large = open(cx, true);
    large.read(
        0,
        Ok(Opened::Text(TextFile {
            content: String::new(),
            generation: "1".into(),
            size_bytes: TEXT_FILE_LIMIT_BYTES + 1,
            line_ending: LineEnding::None,
        })),
        cx,
    );
    assert_eq!(large.content(cx), Content::TooLarge);
    assert_eq!(large.state(cx), None);
}

#[gpui_kit::test]
fn tells_the_tab_once_when_it_turns_dirty_and_ignores_edits_it_cannot_write(
    cx: &mut TestAppContext,
) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    fixture.edit("ab", cx);
    fixture.edit("abc", cx);
    assert_eq!(*fixture.events.borrow(), [DocumentEvent::Dirty]);

    let read_only = open(cx, true);
    read_only.read(0, text("a", "1"), cx);
    read_only.writable.set(false);
    read_only.edit("ab", cx);
    assert!(read_only.events.borrow().is_empty());
    assert_eq!(read_only.state(cx), Some(SaveState::Saved));
}

#[gpui_kit::test]
fn a_save_without_a_connection_fails_and_says_so(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    fixture.edit("ab", cx);
    *fixture.scope.borrow_mut() = None;
    saved_after_debounce(cx);
    assert_eq!(fixture.writes(), 0);
    assert_eq!(fixture.state(cx), Some(SaveState::Error));
    assert_eq!(
        fixture.document.read_with(cx, |d, _| d.save_error()),
        Some("The file host is disconnected.".into())
    );
}

/// Port of `editorFlushRegistry.test.ts`: the close barrier waits for every
/// editor's save, including one whose tab already left, and fails when one
/// does not save.
#[gpui_kit::test]
fn flushing_every_editor_waits_for_each_save_and_fails_if_one_does(cx: &mut TestAppContext) {
    let first = open(cx, true);
    first.read(0, text("a", "1"), cx);
    first.edit("ab", cx);
    let second = open(cx, true);
    second.read(0, text("b", "1"), cx);
    second.edit("bc", cx);
    cx.update(|cx| {
        cx.set_global(crate::editor::Editors::default());
        cx.global_mut::<crate::editor::Editors>().retiring =
            vec![first.document.clone(), second.document.clone()];
    });
    assert!(cx.update(|cx| crate::editor::owes_save(cx)));
    let out = Rc::new(RefCell::new(None));
    let sink = out.clone();
    cx.update(|cx| {
        let flush = crate::editor::flush_all(cx);
        cx.spawn(async move |_| *sink.borrow_mut() = Some(flush.await))
            .detach();
    });
    cx.run_until_parked();
    assert_eq!((first.writes(), second.writes()), (1, 1));
    first.write(0, Ok("2".into()), cx);
    settle(cx);
    assert_eq!(*out.borrow(), None, "one save is still owed");
    second.write(0, Err("disk full".into()), cx);
    settle(cx);
    assert_eq!(
        *out.borrow(),
        Some(Err("One or more editor files could not be saved.".into()))
    );
}

#[gpui_kit::test]
fn a_read_nobody_waits_for_is_stopped_but_a_save_on_the_wire_never_is(cx: &mut TestAppContext) {
    let fixture = open(cx, true);
    let Fixture { host, document, .. } = fixture;
    drop(document);
    cx.update(|_| {});
    cx.run_until_parked();
    assert!(
        host.reads.borrow()[0].2.get(),
        "a read nobody waits for is stopped"
    );

    let fixture = open(cx, true);
    fixture.read(0, text("a", "1"), cx);
    fixture.edit("b", cx);
    saved_after_debounce(cx);
    assert_eq!(fixture.writes(), 1);
    // A disconnect and the flush a closing tab runs both leave the save on the
    // wire: a write the host may already have applied is answered, never
    // abandoned.
    fixture.reconnect(None, cx);
    let flushed = fixture.flush(cx);
    assert!(!fixture.host.writes.borrow()[0].cancelled.get());
    assert!(flushed.borrow().is_none(), "the flush waits for the save");
    fixture.write(0, Ok("2".into()), cx);
    settle(cx);
    assert_eq!(*flushed.borrow(), Some(Ok(())));
    assert!(!fixture.host.writes.borrow()[0].cancelled.get());
}
