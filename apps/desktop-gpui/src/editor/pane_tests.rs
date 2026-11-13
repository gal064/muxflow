//! A file tab drawn in a window over a scripted host: find and replace, go to
//! line, and the files that show a message instead of an editor. Ports the
//! visible outcomes of `editorChunkAbsence.test.tsx` and the find widget's
//! behavior Monaco provided.

use std::{
    cell::{Cell, RefCell},
    path::Path,
    rc::Rc,
};

use desktop_core::app_state::{AppTabKind, AppTabRecord, AppTabViewMode};
use gpui_kit::{
    Entity, TestAppContext, TestSupportExt, VisualTestContext, px, size, test::TestQueryExt,
};

use super::*;
use crate::{
    app::{self, Core},
    editor::{
        LiveMarkdown, LiveShown,
        autosave::{LineEnding, SAVE_DELAY, SaveRequest},
        io::{FileScope, Opened, Reply, Slot, TEXT_FILE_LIMIT_BYTES, TextFile},
    },
};

#[derive(Default)]
struct Host {
    file: RefCell<Option<Opened>>,
    reads: RefCell<usize>,
    /// How many times the Markdown preview was asked to draw.
    previews: Cell<usize>,
    /// How many times the Live view was asked to draw.
    lives: Cell<usize>,
    writes: RefCell<Vec<String>>,
    unanswered: RefCell<Vec<Slot<Opened>>>,
}

struct Io(Rc<Host>);

impl FileIo for Io {
    fn read(&self, _: &FileScope, _: &Root, _: &str) -> Reply<Opened> {
        *self.0.reads.borrow_mut() += 1;
        // No file is a read the host has not answered yet.
        let Some(file) = self.0.file.borrow().clone() else {
            let (reply, slot, _) = Reply::pending();
            self.0.unanswered.borrow_mut().push(slot);
            return reply;
        };
        Reply::ready(Ok(file))
    }

    fn write(&self, _: &FileScope, _: &Root, _: &str, save: &SaveRequest) -> Reply<String> {
        let mut writes = self.0.writes.borrow_mut();
        writes.push(save.content.to_string());
        Reply::ready(Ok((writes.len() + 1).to_string()))
    }
}

fn record() -> AppTabRecord {
    let mut record = crate::workspaces::model::fixtures::tab("a", "$1", 0);
    record.kind = AppTabKind::File;
    record
}

fn text(content: &str) -> Opened {
    Opened::Text(TextFile {
        content: content.into(),
        generation: "1".into(),
        size_bytes: content.len() as u64,
        line_ending: LineEnding::detect(content),
    })
}

fn open(
    cx: &mut TestAppContext,
    file: Option<Opened>,
    connected: bool,
) -> (Entity<DocumentPane>, Rc<Host>, &mut VisualTestContext) {
    open_tab(cx, record(), file, connected)
}

fn open_tab(
    cx: &mut TestAppContext,
    record: AppTabRecord,
    file: Option<Opened>,
    connected: bool,
) -> (Entity<DocumentPane>, Rc<Host>, &mut VisualTestContext) {
    open_with(cx, record, file, connected, false)
}

/// A tab drawn with the real Live view, or with a stub.
fn open_with(
    cx: &mut TestAppContext,
    record: AppTabRecord,
    file: Option<Opened>,
    connected: bool,
    real_live: bool,
) -> (Entity<DocumentPane>, Rc<Host>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        app::init(Core::at(Path::new("/nonexistent/app-state.json")), cx);
    });
    let host = Rc::new(Host {
        file: RefCell::new(file),
        ..Host::default()
    });
    // Counts the draws; what the preview shows is the Markdown module's own test.
    cx.update(|cx| {
        let host = host.clone();
        crate::editor::set_markdown_preview(
            Rc::new(move |_, _, _, _| {
                host.previews.set(host.previews.get() + 1);
                div()
                    .id("preview-stub")
                    .test_support()
                    .aria_label("preview stub")
                    .size_full()
                    .into_any_element()
            }),
            cx,
        );
    });
    cx.update(|cx| {
        let host = host.clone();
        let focus = cx.focus_handle();
        let find: Rc<dyn LiveFind> = Rc::new(NoFind);
        crate::editor::set_live_markdown(
            LiveMarkdown {
                surface: Rc::new(move |_, _, _, _| {
                    host.lives.set(host.lives.get() + 1);
                    let element = div()
                        .id("live-stub")
                        .track_focus(&focus)
                        .size_full()
                        .into_any_element();
                    LiveShown {
                        element,
                        focus: focus.clone(),
                        find: find.clone(),
                    }
                }),
                limit_bytes: LIVE_LIMIT,
            },
            cx,
        );
    });
    if real_live {
        cx.update(crate::markdown::init);
    }
    let io: Rc<dyn FileIo> = Rc::new(Io(host.clone()));
    let live = Rc::new(Live {
        scope: Box::new(move |_| {
            connected.then(|| FileScope {
                client_id: "client".into(),
                profile_id: "local".into(),
                server_identity: "server-a".into(),
                terminal_epoch: 1,
            })
        }),
        can_write: Box::new(|_| true),
    });
    let root = Root {
        path: "/repo".into(),
        token: "root".into(),
    };
    let (pane, cx) =
        cx.add_window_view(|_, cx| DocumentPane::new(record, root, io, live, None, cx));
    cx.run_until_parked();
    draw(cx);
    (pane, host, cx)
}

/// The stub Live view has nothing to find.
struct NoFind;

impl LiveFind for NoFind {
    fn text(&self, _: &App) -> String {
        String::new()
    }
    fn revision(&self, _: &App) -> u64 {
        0
    }
    fn selection(&self, _: &App) -> Range<usize> {
        0..0
    }
    fn select(&self, _: Range<usize>, _: &mut App) {}
    fn replace(&self, _: Range<usize>, _: &str, _: &mut App) {}
    fn replace_all(&self, _: &str, _: &mut App) {}
    fn highlight(&self, _: Option<regex::Regex>, _: &mut App) {}
    fn observe(&self, _: Box<dyn Fn(&mut App)>, _: &mut App) -> Subscription {
        Subscription::new(|| {})
    }
}

/// The Live view's limit: a file longer than this shows as source.
const LIVE_LIMIT: u64 = live_markdown::LIMIT_BYTES;

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.run_until_parked();
}

fn editor(pane: &Entity<DocumentPane>, cx: &mut VisualTestContext) -> Entity<EditorState> {
    cx.update(|_, cx| pane.read(cx).editor(cx))
        .expect("a text file has an editor")
}

fn find(pane: &Entity<DocumentPane>, query: &str, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| pane.open_find(false, window, cx));
        let input = pane.read(cx).find.as_ref().unwrap().query.clone();
        input.update(cx, |input, cx| input.set_value(query, window, cx));
        pane.update(cx, |pane, cx| pane.typed(cx));
    });
    draw(cx);
}

fn label(pane: &Entity<DocumentPane>, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| {
        let pane = pane.read(cx);
        let selection = pane.editor(cx).unwrap().read(cx).selected_range();
        find::count_label(&pane.find.as_ref().unwrap().matches, &selection)
    })
}

#[gpui_kit::test]
fn find_counts_steps_and_replaces_all_then_autosaves(cx: &mut TestAppContext) {
    let (pane, host, cx) = open(cx, Some(text("one two one two")), true);
    let editor = editor(&pane, cx);
    find(&pane, "two", cx);
    assert_eq!(label(&pane, cx), "1 of 2");
    assert_eq!(cx.update(|_, cx| editor.read(cx).selected_range()), 4..7);
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.step(true, cx)));
    assert_eq!(label(&pane, cx), "2 of 2");
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.step(true, cx)));
    assert_eq!(label(&pane, cx), "1 of 2", "stepping wraps to the top");

    cx.update(|window, cx| {
        let input = pane.read(cx).find.as_ref().unwrap().replacement.clone();
        input.update(cx, |input, cx| input.set_value("2", window, cx));
        pane.update(cx, |pane, cx| pane.replace_all(window, cx));
    });
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| editor.read(cx).text().to_string()),
        "one 2 one 2"
    );
    assert_eq!(label(&pane, cx), "No results");
    cx.executor().advance_clock(SAVE_DELAY);
    cx.run_until_parked();
    assert_eq!(*host.writes.borrow(), ["one 2 one 2"]);
}

#[gpui_kit::test]
fn replace_takes_the_match_under_the_selection_with_its_captures(cx: &mut TestAppContext) {
    let (pane, _, cx) = open(cx, Some(text("a=1 b=2")), true);
    let editor = editor(&pane, cx);
    find(&pane, r"(\w)=(\d)", cx);
    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            if let Some(find) = &mut pane.find {
                find.toggles.regex = true;
            }
            pane.typed(cx);
        })
    });
    assert_eq!(label(&pane, cx), "1 of 2");
    cx.update(|window, cx| {
        let input = pane.read(cx).find.as_ref().unwrap().replacement.clone();
        input.update(cx, |input, cx| input.set_value("$2=$1", window, cx));
        pane.update(cx, |pane, cx| pane.replace_one(window, cx));
    });
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| editor.read(cx).text().to_string()),
        "1=a b=2"
    );
    assert_eq!(label(&pane, cx), "1 of 1", "the next match is selected");
}

#[gpui_kit::test]
fn go_to_line_moves_the_cursor_to_a_line_and_column(cx: &mut TestAppContext) {
    let (pane, _, cx) = open(cx, Some(text("one\ntwo\nthree\n")), true);
    let editor = editor(&pane, cx);
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.open_go_to(window, cx);
            pane.go_to_line("3:2", window, cx);
        })
    });
    draw(cx);
    let position = cx.update(|_, cx| editor.read(cx).cursor_position());
    assert_eq!((position.line, position.character), (2, 1));
    assert!(cx.update(|_, cx| pane.read(cx).go_to.is_none()));
    // A line past the end leaves the box open and the cursor where it was.
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.open_go_to(window, cx);
            pane.go_to_line("40", window, cx);
        })
    });
    assert!(cx.update(|_, cx| pane.read(cx).go_to.is_some()));
}

#[gpui_kit::test]
fn go_to_line_shows_the_line_in_the_middle_of_the_view(cx: &mut TestAppContext) {
    let content: String = (1..=400).map(|n| format!("line {n}\n")).collect();
    let (pane, _, cx) = open(cx, Some(text(&content)), true);
    let editor = editor(&pane, cx);
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.open_go_to(window, cx);
            pane.go_to_line("200", window, cx);
        })
    });
    draw(cx);
    draw(cx);
    let rows = cx
        .update(|_, cx| editor.read(cx).visible_row_range())
        .expect("laid out");
    let middle = (rows.start + rows.end) / 2;
    assert!(
        rows.len() > 8 && middle.abs_diff(199) <= 2,
        "line 200 is not centered in {rows:?}"
    );
}

#[gpui_kit::test]
fn a_binary_file_shows_its_details_instead_of_an_editor(cx: &mut TestAppContext) {
    let binary = Opened::Binary(crate::editor::io::BinaryFile {
        path: "/repo/a.rs".into(),
        generation: "1".into(),
        size_bytes: 3,
        mime: "application/octet-stream".into(),
        image: false,
        preview: None,
    });
    let (pane, _, cx) = open(cx, Some(binary), true);
    assert!(cx.update(|_, cx| pane.read(cx).editor(cx).is_none()));
}

#[gpui_kit::test]
fn a_file_over_the_limit_shows_its_message_instead_of_an_editor(cx: &mut TestAppContext) {
    let large = Opened::Text(TextFile {
        content: String::new(),
        generation: "1".into(),
        size_bytes: TEXT_FILE_LIMIT_BYTES + 1,
        line_ending: LineEnding::None,
    });
    let (pane, _, cx) = open(cx, Some(large), true);
    assert!(cx.update(|_, cx| pane.read(cx).editor(cx).is_none()));
    assert_eq!(
        cx.update(|_, cx| pane.read(cx).document.read(cx).content()),
        Content::TooLarge
    );
}

#[gpui_kit::test]
fn a_disconnected_tab_reads_nothing_and_shows_no_editor(cx: &mut TestAppContext) {
    let (pane, host, cx) = open(cx, Some(text("a")), false);
    assert_eq!(*host.reads.borrow(), 0);
    assert!(cx.update(|_, cx| pane.read(cx).editor(cx).is_none()));
}

#[gpui_kit::test]
fn a_change_on_disk_keeps_the_caret_and_is_not_saved_back(cx: &mut TestAppContext) {
    let (pane, host, cx) = open(cx, Some(text("one\ntwo\nthree\n")), true);
    let editor = editor(&pane, cx);
    cx.update(|_, cx| editor.update(cx, |editor, cx| editor.set_selected_range(8..9, cx)));
    *host.file.borrow_mut() = Some(Opened::Text(TextFile {
        generation: "2".into(),
        ..match text("zero\none\ntwo\nthree\n") {
            Opened::Text(file) => file,
            Opened::Binary(_) => unreachable!(),
        }
    }));
    cx.update(|_, cx| {
        let document = pane.read(cx).document.clone();
        document.update(cx, |document, cx| {
            document.notice(
                crate::editor::FileNotice::Changed {
                    generation: "2".into(),
                    operation_id: None,
                },
                cx,
            )
        });
    });
    cx.run_until_parked();
    draw(cx);
    assert_eq!(
        cx.update(|_, cx| editor.read(cx).text().to_string()),
        "zero\none\ntwo\nthree\n"
    );
    // Still on the `t` of `three`, five bytes further down.
    assert_eq!(cx.update(|_, cx| editor.read(cx).selected_range()), 13..14);
    cx.executor().advance_clock(SAVE_DELAY * 4);
    cx.run_until_parked();
    assert!(host.writes.borrow().is_empty(), "nothing is written back");
    assert_eq!(
        cx.update(|_, cx| pane.read(cx).document.read(cx).save_state()),
        Some(crate::editor::autosave::SaveState::Saved)
    );
}

#[gpui_kit::test]
fn find_reopens_on_the_query_and_toggles_it_closed_with(cx: &mut TestAppContext) {
    let (pane, _, cx) = open(cx, Some(text("Cat cat")), true);
    find(&pane, "cat", cx);
    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            if let Some(find) = &mut pane.find {
                find.toggles.match_case = true;
            }
            pane.typed(cx);
        })
    });
    assert_eq!(label(&pane, cx), "1 of 1");
    cx.update(|window, cx| {
        pane.update(cx, |pane, cx| {
            pane.close_find(window, cx);
            pane.open_find(false, window, cx);
        })
    });
    draw(cx);
    let (query, case) = cx.update(|_, cx| {
        let find = pane.read(cx).find.as_ref().unwrap();
        (
            find.query.read(cx).value().to_string(),
            find.toggles.match_case,
        )
    });
    assert_eq!((query.as_str(), case), ("cat", true));
}

#[gpui_kit::test]
fn only_a_slow_open_says_it_is_loading(cx: &mut TestAppContext) {
    let (pane, host, cx) = open_tab(cx, record(), None, true);
    let loading = |cx: &mut VisualTestContext| {
        draw(cx);
        cx.update(|window, _| window.try_find_by_label("Loading…").map(|line| line.role()))
    };
    // DelayedLoading's 150 ms: a fast open never flashes the line.
    cx.executor()
        .advance_clock(LOADING_DELAY - Duration::from_millis(10));
    assert_eq!(loading(cx), None);
    cx.executor().advance_clock(Duration::from_millis(10));
    assert_eq!(loading(cx), Some(Some(Role::Status)));
    // The answer replaces it.
    *host.unanswered.borrow()[0].lock().unwrap() = Some(Ok(text("done")));
    cx.executor().advance_clock(LOADING_DELAY);
    assert_eq!(loading(cx), None);
    assert!(cx.update(|_, cx| pane.read(cx).editor(cx).is_some()));
}

fn markdown_tab(mode: AppTabViewMode) -> AppTabRecord {
    let mut record = record();
    record.kind = AppTabKind::Markdown;
    record.view_mode = Some(mode);
    record
}

#[gpui_kit::test]
fn a_preview_only_markdown_tab_reads_the_file_but_never_makes_an_editor(cx: &mut TestAppContext) {
    let (pane, host, cx) = open_tab(
        cx,
        markdown_tab(AppTabViewMode::Preview),
        Some(text("# heading")),
        true,
    );
    assert_eq!(*host.reads.borrow(), 1);
    assert!(cx.update(|_, cx| pane.read(cx).editor(cx).is_none()));
    assert!(host.previews.get() > 0, "the preview was not drawn");
}

#[gpui_kit::test]
fn a_split_markdown_tab_shows_the_editor_and_the_preview(cx: &mut TestAppContext) {
    let (pane, host, cx) = open_tab(
        cx,
        markdown_tab(AppTabViewMode::Split),
        Some(text("# heading")),
        true,
    );
    assert!(cx.update(|_, cx| pane.read(cx).editor(cx).is_some()));
    assert!(host.previews.get() > 0);
}

#[gpui_kit::test]
fn a_narrow_window_stacks_the_split_preview_below_the_editor(cx: &mut TestAppContext) {
    let (_, _, cx) = open_tab(
        cx,
        markdown_tab(AppTabViewMode::Split),
        Some(text("# heading")),
        true,
    );
    let mut preview_at = |width: f32| {
        cx.simulate_resize(size(px(width), px(800.)));
        draw(cx);
        cx.update(|window, _| window.find_by_label("preview stub").bounds().origin)
    };
    let beside = preview_at(1280.);
    assert!(beside.x > px(400.), "{beside:?}");
    let below = preview_at(860.);
    assert!(below.x < px(10.) && below.y > px(300.), "{below:?}");
}

#[gpui_kit::test]
fn a_source_markdown_tab_and_a_plain_file_draw_no_preview(cx: &mut TestAppContext) {
    let (pane, host, cx) = open_tab(
        cx,
        markdown_tab(AppTabViewMode::Source),
        Some(text("# heading")),
        true,
    );
    assert!(cx.update(|_, cx| pane.read(cx).editor(cx).is_some()));
    assert_eq!(host.previews.get(), 0);
}

#[gpui_kit::test]
fn a_plain_file_never_draws_a_preview(cx: &mut TestAppContext) {
    let mut file = record();
    file.view_mode = Some(AppTabViewMode::Preview);
    let (_, host, _) = open_tab(cx, file, Some(text("# heading")), true);
    assert_eq!(host.previews.get(), 0);
}

#[gpui_kit::test]
fn an_edit_in_split_mode_reaches_the_buffer(cx: &mut TestAppContext) {
    let (pane, _, cx) = open_tab(
        cx,
        markdown_tab(AppTabViewMode::Split),
        Some(text("one")),
        true,
    );
    let editor = editor(&pane, cx);
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_range(0..3, cx);
            editor.replace("two", window, cx);
        })
    });
    draw(cx);
    let document = cx.update(|_, cx| pane.read(cx).document.clone());
    assert_eq!(
        cx.update(|_, cx| document.read(cx).text().map(|text| text.to_string())),
        Some("two".into()),
        "the buffer the preview reads"
    );
}

#[gpui_kit::test]
fn switching_to_the_preview_and_back_keeps_the_buffer_and_its_pending_save(
    cx: &mut TestAppContext,
) {
    let (pane, host, cx) = open_tab(
        cx,
        markdown_tab(AppTabViewMode::Split),
        Some(text("one")),
        true,
    );
    let editor = editor(&pane, cx);
    cx.update(|window, cx| {
        editor.update(cx, |editor, cx| {
            editor.set_selected_range(0..3, cx);
            editor.replace("two", window, cx);
        })
    });
    let set_mode = |mode, cx: &mut VisualTestContext| {
        cx.update(|_, cx| pane.update(cx, |pane, _| pane.record.view_mode = Some(mode)));
        draw(cx);
    };
    // The editor leaves the screen with the save still owed.
    set_mode(AppTabViewMode::Preview, cx);
    assert!(host.writes.borrow().is_empty());
    cx.executor().advance_clock(SAVE_DELAY);
    cx.run_until_parked();
    assert_eq!(
        *host.writes.borrow(),
        ["two"],
        "the debounce outlives the view"
    );
    set_mode(AppTabViewMode::Split, cx);
    let back = cx.update(|_, cx| pane.read(cx).editor(cx)).unwrap();
    assert_eq!(back, editor, "the same editor comes back");
    assert_eq!(cx.update(|_, cx| back.read(cx).text().to_string()), "two");
}

#[gpui_kit::test]
fn the_preview_takes_the_keyboard_back_from_the_editor_it_hides(cx: &mut TestAppContext) {
    let (pane, _, cx) = open_tab(
        cx,
        markdown_tab(AppTabViewMode::Split),
        Some(text("one")),
        true,
    );
    let editor = editor(&pane, cx);
    cx.update(|window, cx| window.focus(&editor.read(cx).focus_handle(cx), cx));
    draw(cx);
    // As the Preview button asks the app state to.
    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            pane.record.view_mode = Some(AppTabViewMode::Preview)
        })
    });
    draw(cx);
    // Not the hidden editor, where the app's shortcuts would stop as at a field.
    assert!(cx.update(|window, cx| pane.read(cx).focus.is_focused(window)));
}

#[gpui_kit::test]
fn a_live_markdown_tab_shows_the_live_view_and_never_makes_an_editor(cx: &mut TestAppContext) {
    let (pane, host, cx) = open_tab(
        cx,
        markdown_tab(AppTabViewMode::Live),
        Some(text("# heading")),
        true,
    );
    assert!(host.lives.get() > 0, "the Live view was not drawn");
    assert!(cx.update(|_, cx| pane.read(cx).editor(cx).is_none()));
    assert_eq!(host.previews.get(), 0);
    // Offered in Tauri's order, and pressed.
    let modes: Vec<_> = cx.update(|_, cx| markdown_modes(cx));
    assert_eq!(
        modes,
        [
            AppTabViewMode::Source,
            AppTabViewMode::Live,
            AppTabViewMode::Preview,
            AppTabViewMode::Split
        ]
    );
    assert_eq!(
        cx.update(|_, cx| pane.read(cx).shown_mode(cx)),
        AppTabViewMode::Live
    );
}

#[gpui_kit::test]
fn a_file_at_the_live_limit_shows_live(cx: &mut TestAppContext) {
    let at = format!("# at\n{}", "x".repeat(LIVE_LIMIT as usize - 5));
    assert_eq!(at.len() as u64, LIVE_LIMIT);
    let (pane, host, cx) = open_tab(
        cx,
        markdown_tab(AppTabViewMode::Live),
        Some(text(&at)),
        true,
    );
    assert!(host.lives.get() > 0);
    assert_eq!(
        cx.update(|_, cx| pane.read(cx).shown_mode(cx)),
        AppTabViewMode::Live
    );
}

#[gpui_kit::test]
fn a_file_over_the_live_limit_shows_as_source_and_stays_live_on_record(cx: &mut TestAppContext) {
    let big = format!("# big\n{}", "x".repeat(LIVE_LIMIT as usize - 5));
    assert_eq!(big.len() as u64, LIVE_LIMIT + 1);
    let (pane, host, cx) = open_tab(
        cx,
        markdown_tab(AppTabViewMode::Live),
        Some(text(&big)),
        true,
    );
    assert_eq!(host.lives.get(), 0);
    assert!(cx.update(|_, cx| pane.read(cx).editor(cx).is_some()));
    assert_eq!(
        cx.update(|_, cx| pane.read(cx).shown_mode(cx)),
        AppTabViewMode::Source
    );
    assert_eq!(
        cx.update(|_, cx| pane.read(cx).record.view_mode),
        Some(AppTabViewMode::Live)
    );
    assert!(cx.update(|_, cx| pane.read(cx).live_unavailable(cx)));
}

#[gpui_kit::test]
fn the_live_view_takes_the_keyboard_and_gives_it_to_the_editor_on_leaving(cx: &mut TestAppContext) {
    let (pane, _, cx) = open_tab(
        cx,
        markdown_tab(AppTabViewMode::Live),
        Some(text("one")),
        true,
    );
    let tab = cx.update(|_, cx| pane.read(cx).focus.clone());
    cx.update(|window, cx| window.focus(&tab, cx));
    draw(cx);
    let live = cx
        .update(|_, cx| pane.read(cx).input_focus(cx))
        .expect("the Live view takes the keyboard");
    assert!(cx.update(|window, _| live.is_focused(window)));
    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            pane.record.view_mode = Some(AppTabViewMode::Source)
        })
    });
    draw(cx);
    draw(cx);
    let editor = editor(&pane, cx);
    assert!(cx.update(|window, cx| editor.read(cx).focus_handle(cx).is_focused(window)));
}

#[test]
fn the_live_limit_is_written_as_tauri_writes_it() {
    assert_eq!(size_label(1024 * 1024), "1 MiB");
    assert_eq!(size_label(48 * 1024), "48 KiB");
}

/// Prose, a heading, bold text, a link (whose hidden URL holds the word too),
/// a table and a code block, each with "cat".
const LIVE_CATS: &str = "# cat title\n\nA **cat** and [cat](./cat.md).\n\n\
    | a | cat |\n|---|---|\n| cat | x |\n\n```\ncat\n```\n";

fn open_live<'a>(
    content: &str,
    cx: &'a mut TestAppContext,
) -> (Entity<DocumentPane>, Rc<Host>, &'a mut VisualTestContext) {
    let (pane, host, cx) = open_with(
        cx,
        markdown_tab(AppTabViewMode::Live),
        Some(text(content)),
        true,
        true,
    );
    let live = cx
        .update(|_, cx| pane.read(cx).input_focus(cx))
        .expect("the Live view takes the keyboard");
    cx.update(|window, cx| window.focus(&live, cx));
    draw(cx);
    (pane, host, cx)
}

fn live_focused(pane: &Entity<DocumentPane>, cx: &mut VisualTestContext) -> bool {
    cx.update(|window, cx| {
        let live = pane.read(cx).live.clone().expect("Live is shown");
        live.0.is_focused(window)
    })
}

/// The Live view's selection, and the count the bar shows for it.
fn live_state(pane: &Entity<DocumentPane>, cx: &mut VisualTestContext) -> (Range<usize>, String) {
    cx.update(|_, cx| {
        let pane = pane.read(cx);
        let find = pane.find.as_ref().expect("the bar is open");
        let selection = find.target.selection(cx);
        let label = find::count_label(&find.matches, &selection);
        (selection, label)
    })
}

fn type_query(pane: &Entity<DocumentPane>, query: &str, cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let input = pane.read(cx).find.as_ref().unwrap().query.clone();
        input.update(cx, |input, cx| input.set_value(query, window, cx));
        pane.update(cx, |pane, cx| pane.typed(cx));
    });
    draw(cx);
}

fn written(host: &Host, cx: &mut VisualTestContext) -> Option<String> {
    cx.executor().advance_clock(SAVE_DELAY);
    cx.run_until_parked();
    host.writes.borrow().last().cloned()
}

#[gpui_kit::test]
fn ctrl_f_finds_in_the_live_view_and_steps_through_every_match(cx: &mut TestAppContext) {
    let (pane, _, cx) = open_live(LIVE_CATS, cx);
    cx.simulate_keystrokes("ctrl-f");
    draw(cx);
    assert!(
        cx.update(|_, cx| pane.read(cx).find.is_some()),
        "Ctrl+F opens the bar"
    );
    assert!(
        cx.update(|_, cx| pane.read(cx).editor(cx).is_none()),
        "no source editor"
    );
    type_query(&pane, "cat", cx);
    let starts: Vec<usize> = LIVE_CATS.match_indices("cat").map(|(at, _)| at).collect();
    assert_eq!(starts.len(), 7);
    let (selection, label) = live_state(&pane, cx);
    assert_eq!(
        (selection, label.as_str()),
        (starts[0]..starts[0] + 3, "1 of 7")
    );
    for (n, at) in starts.iter().enumerate().skip(1) {
        cx.update(|_, cx| pane.update(cx, |pane, cx| pane.step(true, cx)));
        draw(cx);
        let (selection, label) = live_state(&pane, cx);
        assert_eq!(selection, *at..at + 3);
        assert_eq!(label, format!("{} of 7", n + 1));
    }
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.step(true, cx)));
    assert_eq!(live_state(&pane, cx).1, "1 of 7", "Enter wraps to the top");
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.step(false, cx)));
    assert_eq!(
        live_state(&pane, cx).1,
        "7 of 7",
        "Shift+Enter wraps to the end"
    );
    // The bar's toggles: case and whole word.
    cx.update(|_, cx| pane.update(cx, |pane, cx| pane.toggle(|q| q.match_case = true, cx)));
    type_query(&pane, "CAT", cx);
    assert_eq!(live_state(&pane, cx).1, "No results");
    cx.update(|_, cx| {
        pane.update(cx, |pane, cx| {
            pane.toggle(|q| (q.match_case, q.regex) = (false, true), cx)
        })
    });
    type_query(&pane, r"c.t\.md", cx);
    assert_eq!(live_state(&pane, cx).1, "1 of 1");
}

#[gpui_kit::test]
fn the_find_bar_is_seeded_from_the_live_selection_and_escape_returns_to_it(
    cx: &mut TestAppContext,
) {
    let (pane, _, cx) = open_live("one cat two cat\n", cx);
    // A double click's word, as the source editor seeds from its selection.
    cx.update(|_, cx| {
        let pane = pane.read(cx);
        let (_, live) = pane.live.clone().unwrap();
        live.select(4..7, cx);
    });
    cx.simulate_keystrokes("ctrl-f");
    draw(cx);
    let query = cx.update(|_, cx| {
        let find = pane.read(cx).find.as_ref().unwrap();
        find.query.read(cx).value().to_string()
    });
    assert_eq!(query, "cat");
    assert_eq!(live_state(&pane, cx), (4..7, "1 of 2".into()));
    assert!(!live_focused(&pane, cx), "the query field has the keyboard");
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.update(|_, cx| pane.read(cx).find.is_none()));
    assert!(
        live_focused(&pane, cx),
        "Escape gives the keyboard back to Live"
    );
    // Escape in the Live view closes it too, and keeps the keyboard there.
    cx.simulate_keystrokes("ctrl-f");
    draw(cx);
    cx.update(|window, cx| {
        let live = pane.read(cx).live.clone().unwrap();
        window.focus(&live.0, cx);
    });
    cx.simulate_keystrokes("escape");
    draw(cx);
    assert!(cx.update(|_, cx| pane.read(cx).find.is_none()));
    assert!(live_focused(&pane, cx));
}

#[gpui_kit::test]
fn replace_and_replace_all_in_live_are_one_undo_step_each_and_autosave(cx: &mut TestAppContext) {
    let (pane, host, cx) = open_live("a cat, a **cat**, a cat\n", cx);
    cx.update(|window, cx| pane.update(cx, |pane, cx| pane.open_find(true, window, cx)));
    type_query(&pane, "cat", cx);
    let set_replacement = |with: &str, cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            let input = pane.read(cx).find.as_ref().unwrap().replacement.clone();
            input.update(cx, |input, cx| input.set_value(with, window, cx));
        })
    };
    set_replacement("dog", cx);
    cx.update(|window, cx| pane.update(cx, |pane, cx| pane.replace_one(window, cx)));
    draw(cx);
    assert_eq!(
        live_state(&pane, cx),
        (11..14, "1 of 2".into()),
        "on to the next"
    );
    assert_eq!(
        written(&host, cx).as_deref(),
        Some("a dog, a **cat**, a cat\n")
    );

    cx.update(|window, cx| pane.update(cx, |pane, cx| pane.replace_all(window, cx)));
    draw(cx);
    assert_eq!(live_state(&pane, cx).1, "No results");
    assert_eq!(
        written(&host, cx).as_deref(),
        Some("a dog, a **dog**, a dog\n")
    );

    let live = cx.update(|_, cx| pane.read(cx).live.clone().unwrap());
    cx.update(|window, cx| window.focus(&live.0, cx));
    cx.simulate_keystrokes("ctrl-z");
    draw(cx);
    assert_eq!(
        written(&host, cx).as_deref(),
        Some("a dog, a **cat**, a cat\n")
    );
    cx.simulate_keystrokes("ctrl-z");
    draw(cx);
    assert_eq!(
        written(&host, cx).as_deref(),
        Some("a cat, a **cat**, a cat\n")
    );
}

#[gpui_kit::test]
fn the_live_find_bar_closes_when_the_tab_leaves_live(cx: &mut TestAppContext) {
    let (pane, _, cx) = open_live("one cat\n", cx);
    cx.simulate_keystrokes("ctrl-f");
    type_query(&pane, "cat", cx);
    cx.update(|_, cx| {
        pane.update(cx, |pane, _| {
            pane.record.view_mode = Some(AppTabViewMode::Source)
        })
    });
    draw(cx);
    assert!(cx.update(|_, cx| pane.read(cx).find.is_none()));
    // The source editor's find opens on the query Live's closed with.
    cx.update(|window, cx| pane.update(cx, |pane, cx| pane.open_find(false, window, cx)));
    draw(cx);
    assert_eq!(label(&pane, cx), "? of 1");
}

#[gpui_kit::test]
fn a_click_on_the_find_bar_stays_out_of_the_live_view_under_it(cx: &mut TestAppContext) {
    let (pane, _, cx) = open_live(LIVE_CATS, cx);
    cx.simulate_keystrokes("ctrl-f");
    type_query(&pane, "cat", cx);
    assert_eq!(live_state(&pane, cx).1, "1 of 7");
    let next = cx.update(|window, _| window.find_by_label("Next Match (Enter)").bounds().center());
    cx.simulate_click(next, gpui_kit::Modifiers::none());
    draw(cx);
    assert_eq!(live_state(&pane, cx).1, "2 of 7", "stepped from the match");
    assert!(
        !live_focused(&pane, cx),
        "the click did not land in the view"
    );
}
