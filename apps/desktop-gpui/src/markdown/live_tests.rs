//! The Live view over a real document and a scripted host: edits reach the
//! buffer and autosave, changes on disk reach the view, and an IME preedit is
//! never saved. Ports the `LiveMarkdownEditor.test.tsx` cases about the
//! buffer; the editing itself is the crate's own test.

use std::{
    cell::{Cell, RefCell},
    path::Path,
    rc::Rc,
};

use gpui_kit::{EntityInputHandler, TestAppContext, VisualTestContext};

use super::*;
use crate::{
    app::{self, Core},
    editor::{
        FileNotice, Root,
        autosave::{LineEnding, SAVE_DELAY, SaveRequest},
        document::Live,
        io::{FileIo, FileScope, Opened, Reply, TextFile},
    },
};

#[derive(Default)]
struct Disk {
    content: RefCell<String>,
    generation: Cell<u64>,
    writes: RefCell<Vec<String>>,
}

struct Io(Rc<Disk>);

impl FileIo for Io {
    fn read(&self, _: &FileScope, _: &Root, _: &str) -> Reply<Opened> {
        let content = self.0.content.borrow().clone();
        Reply::ready(Ok(Opened::Text(TextFile {
            size_bytes: content.len() as u64,
            line_ending: LineEnding::detect(&content),
            content,
            generation: self.0.generation.get().to_string(),
        })))
    }

    fn write(&self, _: &FileScope, _: &Root, _: &str, save: &SaveRequest) -> Reply<String> {
        let written = String::from_utf8(
            save.line_ending
                .apply(&save.content.to_string())
                .into_bytes(),
        )
        .expect("text");
        self.0.writes.borrow_mut().push(written);
        self.0.generation.set(self.0.generation.get() + 1);
        Reply::ready(Ok(self.0.generation.get().to_string()))
    }
}

struct Fixture {
    disk: Rc<Disk>,
    writable: Rc<Cell<bool>>,
    document: Entity<Document>,
    surface: Entity<Surface>,
}

fn open<'a>(source: &str, cx: &'a mut TestAppContext) -> (Fixture, &'a mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        app::init(Core::at(Path::new("/nonexistent/app-state.json")), cx);
        live_markdown::view::init(cx);
    });
    let disk = Rc::new(Disk::default());
    *disk.content.borrow_mut() = source.to_owned();
    disk.generation.set(1);
    let writable = Rc::new(Cell::new(true));
    let live = Rc::new(Live {
        scope: Box::new(|_| {
            Some(FileScope {
                client_id: "client".into(),
                profile_id: "local".into(),
                server_identity: "server-a".into(),
                terminal_epoch: 1,
            })
        }),
        can_write: Box::new({
            let writable = writable.clone();
            move |_| writable.get()
        }),
    });
    let io: Rc<dyn FileIo> = Rc::new(Io(disk.clone()));
    let document = cx.new(|cx| {
        Document::new(
            "a".into(),
            "local".into(),
            "server-a".into(),
            "/repo/docs/note.md".into(),
            Root {
                path: "/repo".into(),
                token: "root".into(),
            },
            io,
            live,
            cx,
        )
    });
    cx.run_until_parked();
    let (surface, cx) = cx.add_window_view({
        let document = document.clone();
        move |window, cx| Surface::new(&document, window, cx)
    });
    cx.run_until_parked();
    (
        Fixture {
            disk,
            writable,
            document,
            surface,
        },
        cx,
    )
}

impl Fixture {
    fn view(&self, cx: &mut VisualTestContext) -> Entity<LiveView> {
        cx.update(|_, cx| self.surface.read(cx).view.clone())
    }

    fn shown(&self, cx: &mut VisualTestContext) -> String {
        let view = self.view(cx);
        cx.update(|_, cx| view.read(cx).text().to_owned())
    }

    fn buffer(&self, cx: &mut VisualTestContext) -> String {
        cx.update(|_, cx| self.document.read(cx).text().unwrap().to_string())
    }

    /// Text typed at UTF-16 offset `at` through the platform input handler.
    fn type_at(&self, at: usize, text: &str, cx: &mut VisualTestContext) {
        let view = self.view(cx);
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.replace_text_in_range(Some(at..at), text, window, cx)
            })
        });
        cx.run_until_parked();
    }

    fn saved_after_debounce(&self, cx: &mut VisualTestContext) -> Vec<String> {
        cx.executor().advance_clock(SAVE_DELAY);
        cx.run_until_parked();
        self.disk.writes.borrow().clone()
    }

    /// The file changed on disk, as an agent rewriting it would.
    fn rewrite(&self, source: &str, cx: &mut VisualTestContext) {
        *self.disk.content.borrow_mut() = source.to_owned();
        self.disk.generation.set(self.disk.generation.get() + 1);
        let generation = self.disk.generation.get().to_string();
        cx.update(|_, cx| {
            self.document.update(cx, |document, cx| {
                document.notice(
                    FileNotice::Changed {
                        generation,
                        operation_id: None,
                    },
                    cx,
                )
            })
        });
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn an_edit_in_live_reaches_the_buffer_and_autosaves(cx: &mut TestAppContext) {
    let (fixture, cx) = open("# Title\n\nbody", cx);
    assert_eq!(fixture.shown(cx), "# Title\n\nbody");
    fixture.type_at(13, "!", cx);
    assert_eq!(fixture.buffer(cx), "# Title\n\nbody!");
    assert!(
        fixture.disk.writes.borrow().is_empty(),
        "saved before the debounce"
    );
    assert_eq!(fixture.saved_after_debounce(cx), ["# Title\n\nbody!"]);
}

#[gpui_kit::test]
fn an_outside_change_updates_the_view_and_keeps_the_caret(cx: &mut TestAppContext) {
    let (fixture, cx) = open("one\ntwo\n", cx);
    let view = fixture.view(cx);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.replace_text_in_range(Some(6..6), "", window, cx)
        })
    });
    fixture.rewrite("zero\none\ntwo\n", cx);
    assert_eq!(fixture.shown(cx), "zero\none\ntwo\n");
    assert_eq!(cx.update(|_, cx| view.read(cx).selection().head), 11);
    assert!(
        fixture.saved_after_debounce(cx).is_empty(),
        "the change went back to disk"
    );
}

#[gpui_kit::test]
fn an_ime_preedit_is_never_saved_and_its_commit_is(cx: &mut TestAppContext) {
    let (fixture, cx) = open("ab", cx);
    let view = fixture.view(cx);
    let compose = |text: &str, cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.replace_and_mark_text_in_range(Some(1..1), text, None, window, cx)
            })
        });
        cx.run_until_parked();
    };
    compose("に", cx);
    assert_eq!(fixture.shown(cx), "aにb");
    assert_eq!(fixture.buffer(cx), "ab", "a preedit reached the buffer");
    let state = cx.update(|_, cx| fixture.document.read(cx).save_state());
    assert_eq!(state, Some(crate::editor::autosave::SaveState::Saved));
    assert!(fixture.saved_after_debounce(cx).is_empty());
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.replace_text_in_range(None, "日本", window, cx)
        })
    });
    cx.run_until_parked();
    assert_eq!(fixture.buffer(cx), "a日本b");
    assert_eq!(fixture.saved_after_debounce(cx), ["a日本b"]);
}

// Mirrors LiveMarkdownEditor.test.tsx "edits a CRLF file without inventing lines".
#[gpui_kit::test]
fn edits_a_crlf_file_without_inventing_lines(cx: &mut TestAppContext) {
    let (fixture, cx) = open("# a\r\nb\r\nc\r\n", cx);
    assert_eq!(fixture.shown(cx), "# a\nb\nc\n");
    fixture.type_at(3, "!", cx);
    assert_eq!(fixture.saved_after_debounce(cx), ["# a!\r\nb\r\nc\r\n"]);
    fixture.rewrite("# a!\r\nb\r\nc\r\nd\r\n", cx);
    assert_eq!(fixture.shown(cx), "# a!\nb\nc\nd\n");
}

#[gpui_kit::test]
fn the_view_outlasts_a_tab_switch_until_the_document_goes(cx: &mut TestAppContext) {
    let (fixture, cx) = open("one two", cx);
    let document = fixture.document.clone();
    let first = cx.update(|window, cx| kept(&document, window, cx));
    // Not drawn for a while (another tab), then shown again: the same view,
    // with its caret, scroll and undo history.
    let again = cx.update(|window, cx| kept(&document, window, cx));
    assert_eq!(again.entity_id(), first.entity_id());
    drop((fixture, document, first, again));
    // Release listeners run when the next update flushes its effects.
    cx.update(|_, _| {});
    let left = cx.update(|_, cx| cx.try_global::<Surfaces>().map_or(0, |s| s.0.len()));
    assert_eq!(left, 0, "a surface outlived its document");
}

#[gpui_kit::test]
fn live_edits_reach_the_source_editor_made_after_them(cx: &mut TestAppContext) {
    let (fixture, cx) = open("hello", cx);
    fixture.type_at(5, " world", cx);
    let editor = cx.update(|window, cx| {
        fixture
            .document
            .update(cx, |document, cx| document.editor(window, cx))
    });
    cx.run_until_parked();
    let shown = cx.update(|_, cx| editor.read(cx).text().to_string());
    assert_eq!(shown, "hello world");
}

#[gpui_kit::test]
fn a_composition_committed_by_unmarking_reaches_the_buffer(cx: &mut TestAppContext) {
    let (fixture, cx) = open("ab", cx);
    let view = fixture.view(cx);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(Some(1..1), "に", None, window, cx);
            view.unmark_text(window, cx);
        })
    });
    cx.run_until_parked();
    assert_eq!(fixture.buffer(cx), "aにb");
}

#[gpui_kit::test]
fn a_cancelled_composition_is_no_edit(cx: &mut TestAppContext) {
    let (fixture, cx) = open("ab", cx);
    let view = fixture.view(cx);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(Some(1..1), "に", None, window, cx);
            view.replace_and_mark_text_in_range(None, "", None, window, cx);
            view.unmark_text(window, cx);
        })
    });
    cx.run_until_parked();
    assert_eq!(fixture.buffer(cx), "ab");
    let state = cx.update(|_, cx| fixture.document.read(cx).save_state());
    assert_eq!(state, Some(crate::editor::autosave::SaveState::Saved));
    assert!(fixture.saved_after_debounce(cx).is_empty());
}

#[gpui_kit::test]
fn a_cancelled_composition_in_a_crlf_file_is_no_edit(cx: &mut TestAppContext) {
    let (fixture, cx) = open("a\r\nb", cx);
    let view = fixture.view(cx);
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.replace_and_mark_text_in_range(Some(1..1), "に", None, window, cx);
            view.replace_and_mark_text_in_range(None, "", None, window, cx);
            view.unmark_text(window, cx);
        })
    });
    cx.run_until_parked();
    assert_eq!(fixture.buffer(cx), "a\r\nb");
    assert!(fixture.saved_after_debounce(cx).is_empty());
}

#[gpui_kit::test]
fn an_edit_the_buffer_refuses_does_not_stay_on_screen(cx: &mut TestAppContext) {
    let (fixture, cx) = open("ab", cx);
    // Read-only before the view has drawn again.
    fixture.writable.set(false);
    fixture.type_at(1, "x", cx);
    assert_eq!(fixture.buffer(cx), "ab");
    assert_eq!(fixture.shown(cx), "ab");
}

#[gpui_kit::test]
fn edits_anywhere_reach_the_buffer_as_shown(cx: &mut TestAppContext) {
    let (fixture, cx) = open("one\n\ntwo\n\nthree\n", cx);
    for (at, text) in [(0, "a"), (16, "b"), (5, "\n\nmid"), (2, "é")] {
        fixture.type_at(at, text, cx);
        assert_eq!(
            fixture.buffer(cx),
            fixture.shown(cx),
            "typed {text:?} at {at}"
        );
    }
    assert_eq!(fixture.buffer(cx), "aoéne\n\n\nmid\ntwo\n\nthreeb\n");
}

#[gpui_kit::test]
fn a_file_grown_past_the_limit_is_not_taken_until_it_shrinks(cx: &mut TestAppContext) {
    let (fixture, cx) = open("small\n", cx);
    let big = "x".repeat(live_markdown::LIMIT_BYTES as usize + 1);
    fixture.rewrite(&big, cx);
    assert_eq!(fixture.shown(cx), "small\n");
    fixture.rewrite("small again\n", cx);
    assert_eq!(fixture.shown(cx), "small again\n");
}

#[gpui_kit::test]
fn a_crlf_file_stays_crlf_through_edits_and_rewrites(cx: &mut TestAppContext) {
    let (fixture, cx) = open("a\r\nb\r\n", cx);
    fixture.type_at(1, "1", cx);
    fixture.type_at(4, "2", cx);
    assert_eq!(fixture.buffer(cx), fixture.shown(cx));
    assert_eq!(
        fixture.saved_after_debounce(cx).last().unwrap(),
        "a1\r\nb2\r\n"
    );
    fixture.rewrite("x\r\na1\r\nb2\r\n", cx);
    fixture.type_at(1, "3", cx);
    assert_eq!(fixture.buffer(cx), "x3\na1\nb2\n");
    assert_eq!(
        fixture.saved_after_debounce(cx).last().unwrap(),
        "x3\r\na1\r\nb2\r\n"
    );
}

#[gpui_kit::test]
fn a_view_squeezed_to_nothing_still_draws(cx: &mut TestAppContext) {
    let (fixture, cx) = open("para one\n\nmore text\n", cx);
    for height in [0., 10., 30.] {
        cx.simulate_resize(gpui_kit::size(gpui_kit::px(800.), gpui_kit::px(height)));
        cx.run_until_parked();
    }
    assert_eq!(fixture.shown(cx), "para one\n\nmore text\n");
}
