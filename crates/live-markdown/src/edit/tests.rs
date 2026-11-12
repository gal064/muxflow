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
    // The short row's missing cell, drawn as GFM draws it, has the caret.
    let end = text.find("| 1 |").unwrap() + 5;
    assert_eq!((head(&d), d.missing()), (end, Some((end, 1))));
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
fn up_and_down_in_a_table_keep_the_column_and_leave_at_its_edges() {
    let text = "top\n\n| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n";
    let mut d = doc(text, text.find('4').unwrap());
    assert!(d.table_key(TableKey::Up));
    assert_eq!(head(&d), text.find('2').unwrap() + 1);
    assert!(d.table_key(TableKey::Up));
    assert_eq!(head(&d), text.find('b').unwrap() + 1);
    assert!(d.table_key(TableKey::Up));
    assert_eq!(head(&d), 4, "out onto the line above the table");
    let mut d = doc(text, text.find('2').unwrap());
    assert!(d.table_key(TableKey::Down));
    assert_eq!(head(&d), text.find('4').unwrap() + 1);
}

#[test]
fn shift_tab_and_enter_move_by_cell() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |\n";
    let two = text.find('2').unwrap();
    let mut d = doc(text, two);
    assert!(d.table_key(TableKey::ShiftTab));
    assert_eq!(
        head(&d),
        text.find('1').unwrap() + 1,
        "Shift-Tab lands at the end of the previous cell"
    );
    assert!(d.table_key(TableKey::ShiftTab));
    assert_eq!(head(&d), text.find('b').unwrap() + 1, "and up a row");
    let mut d = doc(text, text.find('b').unwrap() + 1);
    assert!(d.table_key(TableKey::Enter));
    assert_eq!(head(&d), two, "Enter goes down to the start of the cell");
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
    assert!(d.table_key(TableKey::Tab));
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
    assert_eq!(head(&d), text.find('2').unwrap() + 1, "to the cell above");
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

/// As the Tauri tests' `type`: the cell's whole text replaced by `text`.
fn type_cell(d: &mut LiveDoc, row: usize, col: usize, text: &str) {
    let p = d.projection();
    let cell = p.tables[0]
        .cells
        .iter()
        .find(|c| c.row == row && c.col == col)
        .unwrap();
    d.select_range(cell.from..cell.limit);
    d.insert_text(text);
}

#[test]
fn types_word_by_word_without_leaving_padding_and_never_merges_cells_over_a_backslash() {
    let text = "|a|b|\n|-|-|\n|1|2|";
    let mut d = doc(text, text.find('1').unwrap() + 1);
    for typed in [" ", "x", " ", "y"] {
        d.insert_text(typed);
    }
    assert_eq!(d.text(), "|a|b|\n|-|-|\n|1 x y|2|");
    type_cell(&mut d, 1, 0, "1\\");
    assert_eq!(d.text(), "|a|b|\n|-|-|\n|1\\ |2|");
    d.insert_text("z");
    assert_eq!(d.text(), "|a|b|\n|-|-|\n|1\\z|2|");
}

#[test]
fn typing_that_ends_the_table_in_a_cell_takes_the_caret_out_with_the_text() {
    let text = "a | b\n-- | --\n1 | 2\n3 | 4";
    let mut d = doc(text, text.find('1').unwrap() + 1);
    type_cell(&mut d, 1, 0, "- 1");
    assert_eq!(d.text(), "a | b\n-- | --\n- 1 | 2\n3 | 4");
    // The row became a list item, so the table is the header alone.
    assert_eq!(d.projection().tables[0].layout.rows.len(), 1);
    assert_eq!(head(&d), "a | b\n-- | --\n- 1".len());
}

#[test]
fn leaves_with_escape_onto_a_line_below_the_table_never_inside_its_last_row() {
    let text = "| a |\n|---|\n| 1 |\n\nnext";
    let mut d = doc(text, text.find('1').unwrap());
    assert!(d.table_key(TableKey::Escape));
    assert_eq!(head(&d), text.find("next").unwrap());
    assert_eq!(d.text(), text);
}

#[test]
fn adds_a_row_from_the_last_cell_and_removes_an_empty_one_with_backspace() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |";
    let mut d = doc(text, text.find('2').unwrap());
    d.table_key(TableKey::Tab);
    assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1 | 2 |\n|  |  |");
    d.backspace();
    assert_eq!(d.text(), text);
    assert_eq!(head(&d), text.find('1').unwrap() + 1);
    d.enter();
    assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1 | 2 |\n|  |  |");
}

#[test]
fn leaves_below_the_table_onto_a_line_of_its_own_not_into_a_new_row() {
    let text = "| a |\n|---|\n| 1 |";
    let mut d = doc(text, text.find('1').unwrap());
    assert!(d.table_key(TableKey::Down));
    assert_eq!(d.text(), "| a |\n|---|\n| 1 |\n\n");
    assert_eq!(head(&d), d.text().len());
    // A document ending in the table's line break gets one more.
    let text = "| a |\n|---|\n| 1 |\n";
    let mut d = doc(text, text.find('1').unwrap() + 1);
    assert!(d.table_key(TableKey::Right));
    assert_eq!(d.text(), "| a |\n|---|\n| 1 |\n\n");
    // Under a heading that ends the table, a blank line goes between.
    let text = "| a |\n|---|\n| 1 |\n# h";
    let mut d = doc(text, text.find('1').unwrap());
    assert!(d.table_key(TableKey::Down));
    assert_eq!(d.text(), "| a |\n|---|\n| 1 |\n\n\n# h");
    assert_eq!(head(&d), text.find("\n# h").unwrap() + 2);
}

#[test]
fn undoes_a_cell_edit_from_inside_the_cell() {
    let text = "| a |\n| - |\n| 1 |\n\nend";
    let mut d = doc(text, text.find('1').unwrap() + 1);
    type_cell(&mut d, 1, 0, "12");
    assert!(d.text().contains("| 12 |"));
    d.undo();
    assert_eq!(d.text(), text);
    let p = d.projection();
    assert_eq!(p.leaves[p.tables[0].cells[1].leaf].text, "1");
}

#[test]
fn is_not_editable_in_a_read_only_document() {
    let text = "x\n\n| a |\n| - |\n| 1 |\n\ny";
    let table = text.find('|').unwrap()..text.find("\n\ny").unwrap();
    let below = table.end + 1;
    let mut d = doc(text, text.find('1').unwrap());
    d.set_read_only(true);
    assert!(!table.contains(&head(&d)), "the caret moves off the table");
    let p = d.projection();
    assert!(
        table.clone().all(|pos| !p.is_stop(pos)),
        "no cell takes the caret"
    );
    assert!(!d.table_key(TableKey::Tab));
    assert!(!d.toggle_marker_in_cell("**"));
    // The caret crosses the table whole, both ways.
    let mut d = doc(text, 2);
    d.set_read_only(true);
    d.move_horizontal(true, false);
    assert_eq!(head(&d), below);
    d.move_horizontal(false, false);
    assert_eq!(head(&d), 2);
    assert!(d.enter_table(table.start + 2, true));
    assert_eq!(head(&d), below);
}

#[test]
fn arrows_at_a_cells_edges_move_by_cell_and_leave_the_table() {
    let text = "top\n\n| ab | c |\n|---|---|\n| 1 | 2 |\n\nend";
    let a = text.find("ab").unwrap();
    let mut d = doc(text, a + 1);
    assert!(
        !d.table_key(TableKey::Right),
        "inside the text, the editor's"
    );
    let mut d = doc(text, a + 2);
    assert!(d.table_key(TableKey::Right));
    assert_eq!(head(&d), text.find('c').unwrap());
    assert!(d.table_key(TableKey::Left));
    assert_eq!(head(&d), a + 2);
    let mut d = doc(text, text.find('c').unwrap() + 1);
    assert!(d.table_key(TableKey::Right));
    assert_eq!(head(&d), text.find('1').unwrap(), "to the next row");
    assert!(d.table_key(TableKey::Left));
    assert_eq!(head(&d), text.find('c').unwrap() + 1, "and back up");
    let mut d = doc(text, text.find('2').unwrap() + 1);
    assert!(d.table_key(TableKey::Right));
    assert_eq!(head(&d), text.find("end").unwrap(), "out below");
    let mut d = doc(text, a);
    assert!(d.table_key(TableKey::Left));
    assert_eq!(head(&d), 4, "out above");
    // A selection is the editor's to collapse.
    let mut d = doc(text, a);
    d.select_range(a..a + 2);
    assert!(!d.table_key(TableKey::Left));
}

#[test]
fn a_table_at_the_start_gets_a_line_above_when_left_upward() {
    let text = "| a |\n|---|\n| 1 |";
    let mut d = doc(text, text.find('a').unwrap());
    assert!(d.table_key(TableKey::ShiftTab));
    assert_eq!(d.text(), format!("\n{text}"));
    assert_eq!(head(&d), 0);
}

#[test]
fn arrows_enter_a_table_from_outside_at_its_first_or_last_cell() {
    let text = "top\n\n| a | b |\n|---|---|\n| 1 |\n\nend";
    let mut d = doc(text, 4);
    assert!(d.table_key(TableKey::Right));
    assert_eq!(head(&d), text.find('a').unwrap());
    // From the line below, into the short last row's missing cell.
    let end = text.find("| 1 |").unwrap() + 5;
    let mut d = doc(text, end + 1);
    assert!(d.table_key(TableKey::Left));
    assert_eq!((head(&d), d.missing()), (end, Some((end, 1))));
    // Up and Down land wherever the pointer row is, then enter at the edge.
    let mut d = doc(text, 4);
    assert!(d.enter_table(text.find('b').unwrap(), true));
    assert_eq!(head(&d), text.find('a').unwrap());
    let mut d = doc(text, text.len());
    assert!(d.enter_table(text.find('a').unwrap(), false));
    assert_eq!(head(&d), end);
    assert!(!d.enter_table(2, false), "not in a table");
}

#[test]
fn a_short_rows_missing_cells_take_the_caret_and_typing_fills_the_row() {
    let text = "| a | b | c |\n|---|---|---|\n| 1 |\n\nend";
    let end = text.find("| 1 |").unwrap() + 5;
    let mut d = doc(text, end - 2);
    let p = d.projection();
    assert_eq!(p.tables[0].cells.len(), 6, "drawn to the header's width");
    assert!(
        !p.is_stop(end),
        "a missing cell takes the caret only when put there"
    );
    d.table_key(TableKey::Tab);
    assert_eq!(d.missing(), Some((end, 1)));
    assert!(d.projection().is_stop(end));
    d.table_key(TableKey::Tab);
    assert_eq!(d.missing(), Some((end, 2)));
    assert_eq!(d.text(), text, "moving through one writes nothing");
    d.insert_text("z");
    assert_eq!(
        d.text(),
        "| a | b | c |\n|---|---|---|\n| 1 |  | z |\n\nend"
    );
    d.undo();
    assert_eq!(d.text(), text, "filling and typing undo together");
    // A click puts the caret there too; Home and End keep it there; Ctrl+B
    // writes the markers.
    d.set_caret_in_missing(end, 1);
    d.move_line_edge(true, false);
    d.move_line_edge(false, false);
    assert_eq!(d.missing(), Some((end, 1)));
    assert!(d.toggle_marker_in_cell("**"));
    assert_eq!(
        d.text(),
        "| a | b | c |\n|---|---|---|\n| 1 | **** |\n\nend"
    );
    assert_eq!(head(&d), end + 3);
}

#[test]
fn a_row_without_outer_pipes_fills_out_too() {
    let text = "a | b\n-|-\n1\n\nend";
    let end = text.find("\n\nend").unwrap();
    let mut d = doc(text, end - 1);
    d.table_key(TableKey::Tab);
    assert_eq!(d.missing(), Some((end, 1)));
    d.insert_text("z");
    assert_eq!(d.text(), "a | b\n-|-\n1 | z |\n\nend");
}

#[test]
fn cells_past_the_headers_width_are_not_drawn() {
    let text = "| a |\n|---|\n| 1 | 2 |\n";
    let mut d = doc(text, text.len());
    let p = d.projection();
    let cells: Vec<_> = p.tables[0]
        .cells
        .iter()
        .map(|c| p.leaves[c.leaf].text.clone())
        .collect();
    assert_eq!(cells, vec!["a", "1"]);
}

#[test]
fn backspace_in_a_missing_cell_of_an_empty_row_removes_the_row() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |\n|  |\n";
    let row = text.rfind("|  |").unwrap();
    let mut d = doc(text, row + 2);
    d.table_key(TableKey::Tab);
    assert_eq!(d.missing(), Some((row + 4, 1)));
    d.backspace();
    assert_eq!(d.text(), "| a | b |\n|---|---|\n| 1 | 2 |\n");
    assert_eq!(head(&d), text.find('2').unwrap() + 1);
}

#[test]
fn alignment_rows_survive_cell_edits_and_new_rows() {
    let text = "| a | b |\n|:--|--:|\n| 1 | 2 |";
    let mut d = doc(text, text.find('2').unwrap() + 1);
    d.insert_text("0");
    d.enter();
    d.insert_text("x");
    assert_eq!(d.text(), "| a | b |\n|:--|--:|\n| 1 | 20 |\n|  | x |");
}

#[test]
fn bold_and_italic_toggle_in_a_cell_and_keep_the_text_selected() {
    let text = "| a | two |\n|---|---|\n";
    let two = text.find("two").unwrap();
    let mut d = doc(text, two);
    d.select_range(two..two + 3);
    assert!(d.toggle_marker_in_cell("**"));
    assert_eq!(d.text(), "| a | **two** |\n|---|---|\n");
    assert_eq!(d.selected_text(), "two");
    // Selecting the markers too, the same key takes them off.
    d.select_range(two..two + 7);
    assert!(d.toggle_marker_in_cell("**"));
    assert_eq!(d.text(), text);
    assert_eq!(d.selected_text(), "two");
    assert!(d.toggle_marker_in_cell("*"));
    assert_eq!(d.text(), "| a | *two* |\n|---|---|\n");
    // With nothing selected, the caret goes between the markers.
    let mut d = doc(text, two - 3);
    assert!(d.toggle_marker_in_cell("**"));
    assert_eq!(d.text(), "| a**** | two |\n|---|---|\n");
    assert_eq!(head(&d), two - 1);
    // A non-breaking space before the text is written as a plain space, and
    // the selection follows it.
    let text = "| x\u{a0}é |\n|---|\n";
    let e = text.find('é').unwrap();
    let mut d = doc(text, e);
    d.select_range(e..e + 2);
    assert!(d.toggle_marker_in_cell("**"));
    assert_eq!(d.text(), "| x **é** |\n|---|\n");
    assert_eq!(d.selected_text(), "é");
    // Outside a cell it is not the table's.
    let mut d = doc("plain", 2);
    assert!(!d.toggle_marker_in_cell("**"));
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
