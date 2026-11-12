//! Editing behaviour, from the spike's tests and the cases of
//! `liveMarkdown.test.ts`, `liveTable.test.ts` and
//! `LiveMarkdownEditor.test.tsx` this crate covers so far.

use super::*;

fn doc(text: &str, caret: usize) -> LiveDoc {
    let mut d = LiveDoc::new(text);
    d.doc.set_selection(Selection::caret(caret));
    d.refresh();
    d
}

fn table_cells(d: &mut LiveDoc) -> usize {
    d.projection().tables[0].cells.len()
}

fn head(d: &LiveDoc) -> usize {
    d.selection().head
}

#[test]
fn right_walks_into_and_out_of_revealed_markers_without_skipping_text() {
    let text = "ab **cd** ef";
    let mut d = doc(text, 2);
    let mut seen = vec![head(&d)];
    for _ in 0..12 {
        d.move_horizontal(true, false);
        seen.push(head(&d));
    }
    // Every source offset is reached: when the caret touches the construct
    // its markers are revealed and are ordinary characters.
    assert_eq!(seen, vec![2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 12, 12]);
    let mut d = doc(text, text.len());
    for _ in 0..10 {
        d.move_horizontal(false, false);
    }
    assert_eq!(head(&d), 2);
}

#[test]
fn caret_never_rests_inside_a_hidden_range_it_does_not_reveal() {
    // A bullet and a hidden heading prefix on inactive lines are atomic.
    let text = "- one\n- two";
    let mut d = doc(text, 2);
    // Start of the second item's text, then left: over the bullet.
    d.set_caret(8);
    assert_eq!(head(&d), 8);
    d.move_horizontal(false, false);
    assert_eq!(
        head(&d),
        5,
        "left from item text goes to the previous line's end"
    );
    // A click on the bullet lands on the text.
    d.set_caret(6);
    assert_eq!(head(&d), 8);
}

#[test]
fn soft_break_is_one_caret_stop_on_each_side() {
    let text = "one\ntwo";
    let mut d = doc(text, 3);
    d.move_horizontal(true, false);
    assert_eq!(head(&d), 4);
    d.move_horizontal(false, false);
    assert_eq!(head(&d), 3);
}

#[test]
fn typing_at_a_revealed_marker_edits_the_source() {
    let mut d = doc("**bold**", 8);
    d.insert_text("x");
    assert_eq!(d.text(), "**bold**x");
}

#[test]
fn backspace_deletes_atomic_markers_whole() {
    // Mirrors liveMarkdown.test.ts "deletes a block marker whole".
    let mut d = doc("- [ ] task", 6);
    d.backspace();
    assert_eq!(d.text(), "- task");
    d.backspace();
    assert_eq!(d.text(), "task");
}

#[test]
fn backspace_across_a_soft_break_joins_lines_in_the_file() {
    let text = "one two\nthree four\n\nnext";
    let mut d = doc(text, 8);
    d.backspace();
    assert_eq!(d.text(), "one twothree four\n\nnext");
}

#[test]
fn enter_continues_lists_and_starts_paragraphs() {
    let mut d = doc("- one", 5);
    d.enter();
    assert_eq!(d.text(), "- one\n- ");
    let mut d = doc("  9) nine", 9);
    d.enter();
    assert_eq!(d.text(), "  9) nine\n  10) ");
    let mut d = doc("- [x] done", 10);
    d.enter();
    assert_eq!(d.text(), "- [x] done\n- [ ] ");
    let mut d = doc("Hello", 5);
    d.enter();
    assert_eq!(d.text(), "Hello\n\n");
    let mut d = doc("> quoted", 8);
    d.enter();
    assert_eq!(d.text(), "> quoted\n>\n> ");
    let mut d = doc("- one\n- ", 8);
    d.enter();
    assert_eq!(d.text(), "- one\n");
}

#[test]
fn enter_keeps_nesting_and_quote_prefix() {
    let mut d = doc("- one\n  - nested", 16);
    d.enter();
    assert_eq!(d.text(), "- one\n  - nested\n  - ");
    let mut d = doc("> - a", 5);
    d.enter();
    assert_eq!(d.text(), "> - a\n> - ");
}

#[test]
fn caret_is_kept_off_markup_an_edit_creates() {
    let mut d = doc("see - note", 4);
    d.enter();
    assert_eq!(d.text(), "see \n\n- note");
    let head = d.selection().head;
    assert!(d.projection().is_stop(head));
}

#[test]
fn delete_takes_a_soft_break_whole() {
    let mut d = doc("> one\n> two", 5);
    d.delete_forward();
    assert_eq!(d.text(), "> onetwo");
}

#[test]
fn extending_selects_across_hidden_markers_in_both_directions() {
    let text = "aa **bb** cc";
    let mut d = doc(text, 1);
    for _ in 0..4 {
        d.move_horizontal(true, true);
    }
    assert_eq!(d.selection().range(), 1..5);
    d.move_horizontal(false, false);
    assert_eq!(head(&d), 1, "Left collapses to the selection start");
    let mut d = doc(text, text.len());
    d.move_horizontal(false, true);
    assert_eq!(d.selection().range(), text.len() - 1..text.len());
}

#[test]
fn a_selection_from_a_cell_stays_in_the_cell() {
    let text = "| ab | cd |\n|---|---|\n";
    let mut d = doc(text, text.find("ab").unwrap() + 1);
    for _ in 0..4 {
        d.move_horizontal(true, true);
    }
    let cd = text.find("ab").unwrap() + 2;
    assert_eq!(head(&d), cd);
    d.insert_text("x");
    assert_eq!(d.text(), "| ax | cd |\n|---|---|\n");
}

#[test]
fn a_selection_from_outside_takes_the_table_whole() {
    let text = "before\n\n| ab | cd |\n|---|---|\n\nafter";
    let mut d = doc(text, 0);
    let inside = text.find("cd").unwrap();
    d.set_head(inside);
    let end = text.find("\n\nafter").unwrap();
    assert_eq!(
        d.selection().range(),
        0..end + 1,
        "the head rests on the stop past the table"
    );
    assert!(d.projection().is_stop(head(&d)));
}

#[test]
fn a_selection_over_a_table_retracts_with_the_keyboard() {
    let text = "b\n\n| a |\n|---|\n\nafter";
    let mut d = doc(text, 0);
    d.set_head(text.find('a').unwrap());
    let past = head(&d);
    d.move_horizontal(false, true);
    assert!(head(&d) < past);
    assert_eq!(head(&d), 2, "stops just before the table");
}

#[test]
fn enter_in_a_cell_above_a_short_row_never_splits_it() {
    let text = "| a | b |\n|---|---|\n| 1 |\n| 3 | 4 |\n";
    let mut d = doc(text, text.find('b').unwrap());
    d.enter();
    assert_eq!(d.text(), text);
    assert_eq!(head(&d), text.find('1').unwrap());
}

#[test]
fn cell_writes_keep_pipes_escaped_on_every_edit() {
    let text = "| 1 | 2 |\n|---|---|\n";
    let mut d = doc(text, text.find('1').unwrap() + 1);
    d.insert_text("\\");
    d.insert_text("|");
    assert_eq!(
        table_cells(&mut d),
        2,
        "typed backslash then pipe stays one cell: {}",
        d.text()
    );
    let text = "| x\\|y | 2 |\n|---|---|\n";
    let mut d = doc(text, text.find('|').unwrap() + 3 + 1);
    d.backspace();
    assert_eq!(
        table_cells(&mut d),
        2,
        "deleting the escape re-escapes: {}",
        d.text()
    );
    assert!(d.text().contains("x\\|y"));
}

#[test]
fn a_pipe_in_a_preedit_stays_in_its_cell() {
    let text = "| 1 | 2 |\n|---|---|\n";
    let mut d = doc(text, text.find('1').unwrap() + 1);
    d.set_preedit("x|y", None);
    assert_eq!(table_cells(&mut d), 2);
    d.insert_text("x|y");
    assert_eq!(table_cells(&mut d), 2);
    assert_eq!(d.text(), "| 1x\\|y | 2 |\n|---|---|\n");
}

#[test]
fn home_and_end_stay_in_the_cell() {
    let text = "| ab     | cd |\n|---|---|\n| 1 | 2 |\n";
    let a = text.find("ab").unwrap();
    let mut d = doc(text, a + 1);
    d.move_line_edge(true, false);
    assert_eq!(head(&d), a + 2);
    d.move_line_edge(false, false);
    assert_eq!(head(&d), a);
}

#[test]
fn padding_is_skipped_but_typed_spaces_are_caret_stops() {
    let text = "| ab     | cd |\n|---|---|\n";
    let a = text.find("ab").unwrap();
    let mut d = doc(text, a + 2);
    d.move_horizontal(true, false);
    assert_eq!(head(&d), text.find("cd").unwrap());
    let mut d = doc(text, a + 2);
    d.insert_text(" ");
    d.insert_text("x");
    assert_eq!(d.text(), "| ab x     | cd |\n|---|---|\n");
}

#[test]
fn up_and_down_in_a_table_keep_the_column_and_stop_at_its_edges() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut d = doc(text, text.find('4').unwrap());
    assert!(d.cell_vertical(false));
    assert_eq!(head(&d), text.find('2').unwrap() + 1);
    assert!(d.cell_vertical(false));
    assert_eq!(head(&d), text.find('b').unwrap() + 1);
    assert!(
        !d.cell_vertical(false),
        "the top row falls through to pixel motion"
    );
    let mut d = doc(text, text.find('4').unwrap());
    assert!(!d.cell_vertical(true));
}

#[test]
fn cell_navigation_prev_and_down() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let two = text.find('2').unwrap();
    let mut d = doc(text, two);
    assert!(d.cell_navigate(CellDir::Prev));
    assert_eq!(
        head(&d),
        text.find('1').unwrap() + 1,
        "Shift-Tab lands at the end of the previous cell"
    );
    let mut d = doc(text, text.find('b').unwrap());
    assert!(d.cell_navigate(CellDir::Down));
    assert_eq!(head(&d), two);
}

#[test]
fn backspace_ends_a_composition_before_deleting() {
    let mut d = doc("abcdef", 1);
    d.set_preedit("x", None);
    d.backspace();
    assert_eq!(d.marked(), None);
    d.set_preedit("Q", None);
    assert_eq!(d.text(), "aQbcdef");
}

#[test]
fn a_cell_edit_writes_only_that_cell_and_undoes() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut d = doc(text, text.find('2').unwrap());
    d.insert_text("x");
    assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1 | x2 |\n");
    d.undo();
    assert_eq!(d.text(), text);
}

#[test]
fn typing_a_pipe_in_a_cell_escapes_it() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut d = doc(text, text.find('1').unwrap() + 1);
    d.insert_text("|");
    assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1\\| | 2 |\n");
}

#[test]
fn a_trailing_backslash_stays_off_the_closing_pipe() {
    let text = "|a|\n|-|\n|b|\n";
    let mut d = doc(text, text.find('b').unwrap() + 1);
    d.insert_text("\\");
    assert_eq!(d.text(), "|a|\n|-|\n|b\\ |\n");
}

#[test]
fn tab_moves_between_cells_and_adds_a_row_at_the_end() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let mut d = doc(text, text.find('2').unwrap());
    assert!(d.cell_navigate(CellDir::Next));
    assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1 | 2 |\n|  |  |\n");
    d.insert_text("z");
    assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1 | 2 |\n| z |  |\n");
}

#[test]
fn backspace_in_an_empty_row_removes_it() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |\n|  |  |\n";
    let row = text.rfind("|  |").unwrap() + 1;
    let mut d = doc(text, row + 1);
    d.backspace();
    assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1 | 2 |\n");
}

#[test]
fn right_across_table_cells_skips_pipes_and_padding() {
    let text = "| ab | cd |\n|---|---|\n| 1 | 2 |\n";
    let mut d = doc(text, text.find("ab").unwrap());
    let cd = text.find("cd").unwrap();
    for _ in 0..2 {
        d.move_horizontal(true, false);
    }
    assert_eq!(head(&d), text.find("ab").unwrap() + 2);
    d.move_horizontal(true, false);
    assert_eq!(
        head(&d),
        cd,
        "from the end of one cell to the start of the next"
    );
}

#[test]
fn preedit_replaces_itself_commits_as_one_undo_and_can_be_cancelled() {
    let mut d = doc("ab", 1);
    d.set_preedit("に", None);
    assert_eq!(d.text(), "aにb");
    assert_eq!(d.marked(), Some(1..4));
    d.set_preedit("日本", None);
    assert_eq!(d.text(), "a日本b");
    d.insert_text("日本語");
    assert_eq!(d.text(), "a日本語b");
    assert_eq!(d.marked(), None);
    d.undo();
    assert_eq!(d.text(), "ab");
    // Cancelling removes the preedit.
    let mut d = doc("ab", 1);
    d.set_preedit("x", None);
    d.set_preedit("", None);
    assert_eq!(d.text(), "ab");
}

#[test]
fn saved_bytes_are_the_source_and_edits_only_touch_their_range() {
    let text = "# T\n\n- *a*\n\n| x |\n|---|\n| y |\n";
    let mut d = doc(text, 0);
    assert_eq!(d.text().as_bytes(), text.as_bytes());
    // Move all around without editing: bytes untouched.
    d.set_caret(text.find('x').unwrap());
    d.move_horizontal(true, false);
    d.projection();
    assert_eq!(d.text().as_bytes(), text.as_bytes());
    d.insert_text("!");
    assert_eq!(d.text(), "# T\n\n- *a*\n\n| x! |\n|---|\n| y |\n");
}

#[test]
fn selection_across_hidden_markers_replaces_source_including_them() {
    let text = "aa **bb** cc";
    let mut d = doc(text, 0);
    d.doc.set_selection(Selection { anchor: 3, head: 9 });
    d.insert_text("x");
    assert_eq!(d.text(), "aa x cc");
}

#[test]
fn undo_restores_the_source_across_reveal_state() {
    // Edit while the caret reveals markers, then move away; undo goes back
    // to bytes, whatever is currently rendered.
    let mut d = doc("**bold** rest", 2);
    d.insert_text("x");
    assert_eq!(d.text(), "**xbold** rest");
    d.set_caret(13);
    d.undo();
    assert_eq!(d.text(), "**bold** rest");
    assert_eq!(head(&d), 2);
}

fn unfocused(text: &str, caret: usize) -> LiveDoc {
    let mut d = doc(text, caret);
    d.focused = false;
    d
}

// The cases below mirror liveMarkdown.test.ts.

#[test]
fn moves_and_edits_the_caret_through_a_revealed_heading_prefix() {
    let mut d = doc("### Title", 4);
    d.move_horizontal(false, false);
    assert_eq!(head(&d), 3);
    d.set_caret(0);
    assert_eq!(head(&d), 0);
    d.set_caret(3);
    d.backspace();
    assert_eq!(d.text(), "## Title");
}

#[test]
fn deletes_a_block_marker_whole_with_backspace_and_keeps_the_caret_off_it() {
    let mut d = unfocused("## Title\n- [ ] task", 3);
    d.set_caret(0);
    assert_eq!(
        head(&d),
        3,
        "a caret placed on hidden markup moves to the text"
    );
    d.backspace();
    assert_eq!(d.text(), "Title\n- [ ] task");
    let at = d.text().len() - 4;
    d.set_caret(at);
    d.backspace();
    assert_eq!(d.text(), "Title\n- task");
    d.backspace();
    assert_eq!(d.text(), "Title\ntask");
}

#[test]
fn ends_the_list_on_an_empty_item() {
    let mut d = doc("- one\n- ", 8);
    d.enter();
    assert_eq!(d.text(), "- one\n");
}

#[test]
fn starts_a_new_paragraph_quote_paragraph_or_item() {
    let mut d = doc("Hello", 5);
    d.enter();
    assert_eq!(d.text(), "Hello\n\n");
    let mut d = doc("> quoted", 8);
    d.enter();
    assert_eq!(d.text(), "> quoted\n>\n> ");
    let mut d = doc("- item that\n  wraps", 19);
    d.enter();
    assert_eq!(d.text(), "- item that\n  wraps\n- ");
}

#[test]
fn draws_a_hard_wrapped_paragraph_as_one_line_and_backspace_joins_it_in_the_file() {
    let mut d = doc("one two\nthree four\n\nnext", 8);
    let shown: Vec<String> = d
        .projection()
        .leaves
        .iter()
        .map(|l| l.text.clone())
        .collect();
    assert_eq!(shown, vec!["one two three four", "", "next"]);
    d.backspace();
    assert_eq!(d.text(), "one twothree four\n\nnext");
}

// The cases below mirror LiveMarkdownEditor.test.tsx.

#[test]
fn never_writes_a_document_the_user_has_not_edited() {
    let source = "*   odd   bullets\n\n__strong__ and  | a |b|\n|-|-|\n";
    let mut d = doc(source, 0);
    for _ in 0..source.len() {
        d.move_horizontal(true, false);
        d.projection();
    }
    d.focused = false;
    d.projection();
    assert_eq!(d.text(), source);
    assert!(!d.take_edited());
}

#[test]
fn does_not_emit_edits_when_heading_markers_reveal_and_hide_on_focus() {
    let mut d = unfocused("### Title\n\nbody", 0);
    assert_eq!(d.projection().leaves[0].text, "Title");
    d.focused = true;
    assert_eq!(d.projection().leaves[0].text, "### Title");
    let end = d.text().len();
    d.set_caret(end);
    assert_eq!(d.projection().leaves[0].text, "Title");
    assert!(!d.take_edited());
}

#[test]
fn hands_the_host_each_edit_once_and_never_an_open_preedit() {
    let mut d = doc("ab", 1);
    d.insert_text("x");
    assert!(d.take_edited());
    assert!(!d.take_edited());
    d.set_preedit("に", None);
    d.set_preedit("日", None);
    assert!(!d.take_edited(), "a preedit is not an edit");
    d.insert_text("日本");
    assert!(d.take_edited());
    assert_eq!(d.text(), "ax日本b");
    d.set_preedit("x", None);
    d.commit_composition();
    assert!(d.edited(), "a commit by unmarking is an edit");
}

#[test]
fn an_outside_change_drops_an_open_composition() {
    let mut d = doc("ab", 1);
    d.set_preedit("x", None);
    d.reset_text("zab");
    assert_eq!(d.text(), "zab");
    assert_eq!(d.marked(), None);
    assert!(!d.take_edited());
}

// The cases below mirror liveTable.test.ts.

#[test]
fn changes_only_the_cell_typed_into() {
    let text = "| Name   | Value |\n|:-------|------:|\n| alpha  |     1 |\n\nafter";
    let mut d = doc(text, text.find("alpha").unwrap() + 5);
    d.insert_text("bet");
    assert_eq!(d.text(), text.replace("| alpha  |", "| alphabet  |"));
    let one = d.text().find("1 |").unwrap();
    d.doc.set_selection(Selection {
        anchor: one,
        head: one + 1,
    });
    d.insert_text("a|b");
    assert!(d.text().contains("|     a\\|b |"), "{}", d.text());
}

#[test]
fn follows_an_edit_made_outside_the_table() {
    let text = "| a |\n| - |\n| 1 |\n\nend";
    let mut d = doc(text, text.len());
    d.reset_text(&text.replace("| 1 |", "| **two** |"));
    let p = d.projection();
    let leaf = &p.leaves[p.tables[0].cells[1].leaf];
    assert_eq!(leaf.text, "two");
    assert!(leaf.marks.iter().any(|(r, s)| r == &(0..3) && s.bold));
}

// Not from the spike or Tauri: the drawn projection is reused until the text
// or the selection changes.

#[test]
fn the_drawn_projection_is_made_once_per_text_and_selection() {
    let mut d = doc("a **b** c", 0);
    let first = d.projection();
    assert!(Rc::ptr_eq(&first, &d.projection()));
    d.set_caret(4);
    let moved = d.projection();
    assert!(!Rc::ptr_eq(&first, &moved));
    d.insert_text("x");
    assert!(!Rc::ptr_eq(&moved, &d.projection()));
}

#[test]
fn queries_near_the_caret_match_the_whole_document() {
    let text = "# A\n\npara **b**\n\n- item\n\n| x | y |\n|---|---|\n\n> q\n\nend";
    let mut d = doc(text, 0);
    for pos in 0..=text.len() {
        if !text.is_char_boundary(pos) {
            continue;
        }
        let sel = Selection::caret(pos);
        let near = d.near(sel, pos..pos);
        let whole = crate::projection::project(text, &d.ast, &d.active(sel));
        assert_eq!(near.is_stop(pos), whole.is_stop(pos), "stop at {pos}");
        assert_eq!(
            near.atomic_before(pos),
            whole.atomic_before(pos),
            "atom before {pos}"
        );
        assert_eq!(
            near.jump_over(pos, true),
            whole.jump_over(pos, true),
            "jump at {pos}"
        );
    }
}
