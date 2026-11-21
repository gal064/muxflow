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

/// Where `range` of the view's text is drawn, after the frames a reveal takes.
fn drawn_at(
    view: &Entity<LiveView>,
    range: std::ops::Range<usize>,
    cx: &mut VisualTestContext,
) -> Option<gpui_kit::Bounds<gpui_kit::Pixels>> {
    for _ in 0..3 {
        // As the frame a reveal asks for: the test platform draws no frames.
        cx.update(|window, cx| {
            view.update(cx, |_, cx| cx.notify());
            window.draw(cx).clear(cx)
        });
        cx.run_until_parked();
    }
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.bounds_for_range(range, gpui_kit::Bounds::default(), window, cx)
        })
    })
}

#[gpui_kit::test]
fn finding_in_a_wide_table_scrolls_it_to_the_match_and_replaces_there(cx: &mut TestAppContext) {
    let cells = |f: &dyn Fn(usize) -> String| (0..30).map(f).collect::<Vec<_>>().join(" | ");
    let source = format!(
        "# wide\n\n| {} |\n| {} |\n| {} |\n",
        cells(&|i| format!("col{i}")),
        cells(&|_| "---".into()),
        cells(&|i| if i == 28 {
            "needle".into()
        } else {
            format!("c{i}")
        }),
    );
    let (fixture, cx) = open(&source, cx);
    let width = gpui_kit::px(800.);
    cx.simulate_resize(gpui_kit::size(width, gpui_kit::px(600.)));
    let view = fixture.view(cx);
    let find = cx.update(|_, cx| fixture.surface.read(cx).find.clone());
    let at = source.find("needle").unwrap();
    let before = drawn_at(&view, at..at + 6, cx);
    assert!(
        before.is_none_or(|b| b.left() > width),
        "the match starts off the side: {before:?}"
    );
    cx.update(|_, cx| find.select(at..at + 6, cx));
    let shown = drawn_at(&view, at..at + 6, cx).expect("the match is drawn");
    assert!(
        shown.left() >= gpui_kit::px(0.) && shown.right() <= width,
        "the table scrolled to the match: {shown:?}"
    );
    // Replace there: the cell stays in view and the file is saved.
    cx.update(|_, cx| find.replace(at..at + 6, "pin", cx));
    let replaced = drawn_at(&view, at..at + 3, cx).expect("the cell is drawn");
    assert!(replaced.left() >= gpui_kit::px(0.) && replaced.right() <= width);
    let saved = fixture.saved_after_debounce(cx);
    assert!(saved.last().unwrap().contains("| pin |"));
}

/// The view focused and drawn, with the selection at `range`.
fn focus_at(view: &Entity<LiveView>, range: std::ops::Range<usize>, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        window.focus(&view.focus_handle(cx), cx);
        view.update(cx, |view, cx| view.select_match(range, cx));
    });
    drawn_at(view, 0..0, cx);
}

/// Keys typed into the view, each after a drawn frame, as a person types.
fn press(view: &Entity<LiveView>, keys: &str, cx: &mut VisualTestContext) -> usize {
    for key in keys.split(' ') {
        cx.simulate_keystrokes(key);
        drawn_at(view, 0..0, cx);
    }
    cx.update(|_, cx| view.read(cx).selection().head)
}

#[gpui_kit::test]
fn table_breaks_draw_navigate_and_save_inside_the_cell(cx: &mut TestAppContext) {
    let source = "| a | b |\n|---|---|\n| one | other |\n| next | row |";
    let (fixture, cx) = open(source, cx);
    let view = fixture.view(cx);
    let at = source.find("one").unwrap() + 3;
    focus_at(&view, at..at, cx);
    let before = drawn_at(&view, at..at, cx).unwrap();
    assert_eq!(press(&view, "enter", cx), at + 4);
    let after = drawn_at(&view, at + 4..at + 4, cx).unwrap();
    assert!(
        after.top() > before.top(),
        "Enter must draw a new line: {before:?} {after:?}"
    );
    fixture.type_at(at + 4, "two", cx);
    drawn_at(&view, 0..0, cx);
    assert_eq!(press(&view, "home", cx), at + 4);
    assert_eq!(press(&view, "end", cx), at + 7);
    assert_eq!(press(&view, "up", cx), at);
    assert_eq!(press(&view, "down", cx), at + 7);
    let saved = fixture.saved_after_debounce(cx);
    assert_eq!(saved.last().unwrap(), &source.replace("one", "one<br>two"));
    assert_eq!(press(&view, "home backspace", cx), at);
    assert_eq!(fixture.shown(cx), source.replace("one", "onetwo"));
}

#[gpui_kit::test]
fn deleting_the_last_word_character_keeps_typing_in_its_cell(cx: &mut TestAppContext) {
    let source = "| a | b |\n|---|---|\n| hello x | other |";
    let (fixture, cx) = open(source, cx);
    let view = fixture.view(cx);
    let at = source.find("hello x").unwrap() + 7;
    focus_at(&view, at..at, cx);
    assert_eq!(press(&view, "backspace z", cx), at);
    assert_eq!(fixture.shown(cx), source.replace("hello x", "hello z"));
}

#[gpui_kit::test]
fn down_and_up_walk_a_lists_items_from_left_of_their_text(cx: &mut TestAppContext) {
    // From a paragraph's start, the goal is left of the items' text, over
    // their bullets.
    let source = "intro\n\n- a\n- b\n- c\n\nend\n";
    let (fixture, cx) = open(source, cx);
    let view = fixture.view(cx);
    focus_at(&view, 0..0, cx);
    let heads: Vec<usize> = (0..5).map(|_| press(&view, "down", cx)).collect();
    assert_eq!(heads, [6, 9, 13, 17, 19]);
    assert_eq!(press(&view, "up", cx), 17);
    assert_eq!(press(&view, "up", cx), 13);
    // Into a nested item and on past it.
    let source = "# T\n\n- a\n  - b\n  - c\n- d\n\nend\n";
    let (fixture, cx) = open(source, cx);
    let view = fixture.view(cx);
    focus_at(&view, 7..7, cx);
    let heads: Vec<usize> = (0..3).map(|_| press(&view, "down", cx)).collect();
    assert_eq!(heads, [13, 19, 23]);
}

#[gpui_kit::test]
fn down_enters_an_item_that_starts_with_a_code_fence(cx: &mut TestAppContext) {
    let source = "9. nine\n10. ten\n11. ```\n    code\n    ```\n\nend\n";
    let (fixture, cx) = open(source, cx);
    let view = fixture.view(cx);
    let ten = source.find("ten").unwrap();
    focus_at(&view, ten..ten, cx);
    let head = press(&view, "down", cx);
    let fence = source.find("```").unwrap();
    let code_end = source.find("\n\nend").unwrap();
    assert!(
        (fence..=code_end).contains(&head),
        "Down from {ten} went to {head}"
    );
}

#[gpui_kit::test]
fn ctrl_home_and_end_reach_the_ends_of_a_long_document(cx: &mut TestAppContext) {
    let source: String = (0..400).map(|i| format!("paragraph {i}\n\n")).collect();
    let (fixture, cx) = open(&source, cx);
    let view = fixture.view(cx);
    focus_at(&view, 20..20, cx);
    #[cfg(target_os = "macos")]
    let (start, end, select_start) = ("cmd-up", "cmd-down", "cmd-shift-up");
    #[cfg(not(target_os = "macos"))]
    let (start, end, select_start) = ("ctrl-home", "ctrl-end", "ctrl-shift-home");
    assert_eq!(press(&view, end, cx), source.len());
    assert!(drawn_at(&view, source.len()..source.len(), cx).is_some());
    assert_eq!(press(&view, select_start, cx), 0);
    let sel = cx.update(|_, cx| view.read(cx).selection());
    assert_eq!(sel.anchor, source.len());
    assert_eq!(press(&view, start, cx), 0);
}

#[gpui_kit::test]
fn the_input_method_is_placed_at_the_caret_when_the_selection_starts_off_screen(
    cx: &mut TestAppContext,
) {
    let source: String = (0..400).map(|i| format!("paragraph {i}\n\n")).collect();
    let (fixture, cx) = open(&source, cx);
    let view = fixture.view(cx);
    focus_at(&view, 0..source.len() - 2, cx);
    let start = drawn_at(&view, 0..0, cx);
    let caret = drawn_at(&view, source.len() - 2..source.len() - 2, cx);
    assert!(caret.is_some());
    assert_eq!(start, caret, "the start is off screen: the caret's place");
}

#[gpui_kit::test]
fn a_click_from_a_frame_drawn_before_the_text_shrank_does_not_panic(cx: &mut TestAppContext) {
    let (fixture, cx) = open("abc def ghi\n", cx);
    let view = fixture.view(cx);
    focus_at(&view, 11..11, cx);
    let end = drawn_at(&view, 11..11, cx).expect("drawn");
    // Cut everything, then click where the text was before a frame is drawn.
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.replace_text_in_range(Some(0..12), "", window, cx)
        })
    });
    cx.simulate_click(end.center(), gpui_kit::Modifiers::default());
    cx.simulate_mouse_move(end.center(), None, gpui_kit::Modifiers::default());
    assert_eq!(fixture.shown(cx), "");
    assert_eq!(cx.update(|_, cx| view.read(cx).selection().head), 0);
}

#[gpui_kit::test]
fn the_scrollbar_takes_a_press_before_any_hover_has_shown_it(cx: &mut TestAppContext) {
    let source: String = (0..400).map(|i| format!("paragraph {i}\n\n")).collect();
    let (fixture, cx) = open(&source, cx);
    cx.simulate_resize(gpui_kit::size(gpui_kit::px(800.), gpui_kit::px(600.)));
    let view = fixture.view(cx);
    focus_at(&view, 0..0, cx);
    let modifiers = gpui_kit::Modifiers::default();
    let left = gpui_kit::MouseButton::Left;
    // No move first: the pointer was already there.
    let bar = |y: f32| gpui_kit::point(gpui_kit::px(796.), gpui_kit::px(y));
    cx.simulate_mouse_down(bar(300.), left, modifiers);
    // The caret's block is laid out wherever it is: above the view now.
    let top = drawn_at(&view, 0..0, cx).expect("the caret's block");
    assert!(
        top.bottom() < gpui_kit::px(0.),
        "the view scrolled: {top:?}"
    );
    cx.simulate_mouse_move(bar(590.), left, modifiers);
    cx.simulate_mouse_up(bar(590.), left, modifiers);
    let end = source.len() - 2;
    assert!(drawn_at(&view, end..end, cx).is_some_and(|b| b.bottom() <= gpui_kit::px(600.)));
    let sel = cx.update(|_, cx| view.read(cx).selection());
    assert_eq!((sel.anchor, sel.head), (0, 0), "the press selected text");
}

#[gpui_kit::test]
fn a_scrolled_tables_columns_keep_their_longest_word_on_one_line(cx: &mut TestAppContext) {
    let word = "supercalifragilisticexpialidocious";
    let cells = |f: &dyn Fn(usize) -> String| (0..30).map(f).collect::<Vec<_>>().join(" | ");
    let source = format!(
        "| {} |\n| {} |\n| {} |\n",
        cells(&|i| format!("c{i}")),
        cells(&|_| "---".into()),
        cells(&|i| if i == 1 {
            format!("a {word} b")
        } else {
            "x".into()
        }),
    );
    let (fixture, cx) = open(&source, cx);
    cx.simulate_resize(gpui_kit::size(gpui_kit::px(800.), gpui_kit::px(600.)));
    let view = fixture.view(cx);
    let at = source.find(word).unwrap();
    // Past its first letter: a caret where a line wraps is drawn on the
    // line above.
    let first = drawn_at(&view, at + 1..at + 1, cx).expect("drawn");
    let end = at + word.len();
    let last = drawn_at(&view, end - 1..end - 1, cx).expect("drawn");
    // The space after it stays at the end of its line: the next starts at
    // the cell's edge, as the word's line does.
    let after = drawn_at(&view, end + 2..end + 2, cx).expect("drawn");
    assert!(after.top() > last.top(), "{after:?} {last:?}");
    let indent = (after.left() - first.left()).abs();
    assert!(
        indent < gpui_kit::px(0.5),
        "a space starts the line: {indent:?}"
    );
    assert_eq!(
        first.top(),
        last.top(),
        "the word broke: {first:?} {last:?}"
    );
}

#[gpui_kit::test]
fn a_wide_tables_bar_scrolls_it_when_pressed_and_dragged(cx: &mut TestAppContext) {
    let cells = |f: &dyn Fn(usize) -> String| (0..30).map(f).collect::<Vec<_>>().join(" | ");
    let source = format!(
        "| {} |\n| {} |\n| {} |\n\nafter\n",
        cells(&|i| format!("h{i}")),
        cells(&|_| "---".into()),
        cells(&|i| format!("c{i}")),
    );
    let (fixture, cx) = open(&source, cx);
    let width = gpui_kit::px(800.);
    cx.simulate_resize(gpui_kit::size(width, gpui_kit::px(600.)));
    let view = fixture.view(cx);
    let first = source.find("c0").unwrap();
    let last = source.find("c29").unwrap();
    // The blank line under the table.
    let after = source.find("\n\nafter").unwrap() + 1;
    assert!(drawn_at(&view, first..first, cx).is_some());
    assert!(drawn_at(&view, last..last, cx).is_none_or(|b| b.left() > width));
    // The bar's strip ends the table's block, just above the blank line.
    let y = drawn_at(&view, after..after, cx).expect("drawn").top() - gpui_kit::px(5.);
    let at = |x: f32| gpui_kit::point(gpui_kit::px(x), y);
    let (left, modifiers) = (gpui_kit::MouseButton::Left, gpui_kit::Modifiers::default());
    // A click on the thumb, near its right end, holds it where it was
    // pressed: the table stays at its left edge (the column starts at 40).
    cx.simulate_mouse_down(at(370.), left, modifiers);
    cx.simulate_mouse_up(at(370.), left, modifiers);
    let kept = drawn_at(&view, first..first, cx).expect("drawn");
    assert!(kept.left() > gpui_kit::px(40.), "{kept:?}");
    cx.simulate_mouse_down(at(400.), left, modifiers);
    cx.simulate_mouse_move(at(790.), left, modifiers);
    cx.simulate_mouse_up(at(790.), left, modifiers);
    let shown = drawn_at(&view, last..last, cx).expect("drawn");
    assert!(
        shown.left() >= gpui_kit::px(0.) && shown.right() <= width,
        "{shown:?}"
    );
    let sel = cx.update(|_, cx| view.read(cx).selection());
    assert_eq!((sel.anchor, sel.head), (0, 0), "the press moved the caret");
}
