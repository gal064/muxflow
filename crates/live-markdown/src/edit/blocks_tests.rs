//! T7.2's editing cases: list commands, ⌘B and ⌘I, the task toggle, Enter
//! before a heading, word motion and click selection. Most mirror
//! `liveMarkdown.test.ts` "live Markdown commands".

use super::*;

fn doc(text: &str, caret: usize) -> LiveDoc {
    let mut d = LiveDoc::new(text);
    d.doc.set_selection(Selection::caret(caret));
    d.refresh();
    d
}

fn selected(text: &str, anchor: usize, head: usize) -> LiveDoc {
    let mut d = LiveDoc::new(text);
    d.doc.set_selection(Selection { anchor, head });
    d.refresh();
    d
}

fn head(d: &LiveDoc) -> usize {
    d.selection().head
}

#[test]
fn indents_whole_items_using_the_preceding_markers_content_column() {
    for (text, indented) in [
        ("- parent\n- item\n- last", "- parent\n  - item\n- last"),
        (
            "1. parent\n2. item\n3. last",
            "1. parent\n   1. item\n2. last",
        ),
        (
            "9. first\n10. parent\n11. item",
            "9. first\n10. parent\n    1. item",
        ),
        ("- [x] parent\n- [ ] item", "- [x] parent\n  - [ ] item"),
        ("- parent\n1. item", "- parent\n1. item"),
        ("> - parent\n> - item", "> - parent\n>   - item"),
        ("-     parent\n- item", "-     parent\n  - item"),
        ("10. parent\n11. `item`", "10. parent\n    1. `item`"),
        (
            "- root\n  1.\tparent\n  2. item",
            "- root\n  1.\tparent\n        1. item",
        ),
        (
            "- parent\n- item\n  > quote",
            "- parent\n  - item\n    > quote",
        ),
        (
            "- parent\n- item\n  ```\n  > code\n  ```",
            "- parent\n  - item\n    ```\n    > code\n    ```",
        ),
        (
            "> - parent\n> - item\n>   > quote",
            "> - parent\n>   - item\n>     > quote",
        ),
    ] {
        let mut d = doc(text, text.find("item").unwrap());
        assert!(d.change_list_indent(false), "{text}");
        assert_eq!(d.text(), indented, "{text}");
        if indented != text {
            let nested = d.projection().blocks.iter().any(|b| {
                matches!(
                    b.kind,
                    BlockKind::ListItem {
                        depth: 1,
                        marker: Some(_),
                        ..
                    }
                )
            });
            assert!(nested, "drawn nested: {indented}");
            assert!(d.change_list_indent(true));
            assert_eq!(d.text(), text);
        }
    }
}

#[test]
fn renumbers_new_and_existing_nested_lists_without_losing_structure() {
    for (text, expected) in [
        (
            "1. parent\n   1. existing\n2. item\n3. last",
            "1. parent\n   1. existing\n   2. item\n2. last",
        ),
        (
            "1) parent\n2) item\n3) last",
            "1) parent\n   1) item\n2) last",
        ),
        (
            "> 1. parent\n> 2. item\n> 3. last",
            "> 1. parent\n>    1. item\n> 2. last",
        ),
        (
            "9. parent\n10. item\n    continued\n    - child",
            "9. parent\n   1. item\n      continued\n      - child",
        ),
    ] {
        let mut d = doc(text, text.find("item").unwrap() + 4);
        d.change_list_indent(false);
        assert_eq!(d.text(), expected, "{text}");
        assert_eq!(&d.text()[head(&d) - 4..head(&d)], "item");
    }
}

#[test]
fn renumbers_both_remaining_children_and_outer_siblings_when_outdenting() {
    let text = "1. parent\n   1. first\n   2. item\n   3. remaining\n2. last";
    let mut d = doc(text, text.find("item").unwrap());
    d.change_list_indent(true);
    assert_eq!(
        d.text(),
        "1. parent\n   1. first\n2. item\n   1. remaining\n3. last"
    );
}

#[test]
fn preserves_deliberate_starts_and_adjusts_descendants_across_digit_boundaries() {
    let text = "8. first\n9. parent\n   1. item\n      continued\n      - child\n10. last";
    let mut d = doc(text, text.find("item").unwrap());
    d.change_list_indent(true);
    assert_eq!(
        d.text(),
        "8. first\n9. parent\n10. item\n    continued\n    - child\n11. last"
    );
}

#[test]
fn keeps_ordered_indentation_and_numbering_in_one_undo_step() {
    let text = "1. parent\n2. item\n3. last";
    let mut d = doc(text, text.find("item").unwrap());
    d.change_list_indent(false);
    assert_eq!(d.text(), "1. parent\n   1. item\n2. last");
    d.undo();
    assert_eq!(d.text(), text);
    d.redo();
    assert_eq!(d.text(), "1. parent\n   1. item\n2. last");
}

#[test]
fn keeps_offscreen_siblings_numbered_when_moving_an_item_in_a_long_list() {
    let text: Vec<String> = (1..=1000).map(|i| format!("{i}. item {i}")).collect();
    let text = text.join("\n");
    let mut d = doc(&text, text.find("item 2").unwrap());
    d.change_list_indent(false);
    let mut expected = vec!["1. item 1".to_string(), "   1. item 2".to_string()];
    expected.extend((0..998).map(|i| format!("{}. item {}", i + 2, i + 3)));
    assert_eq!(d.text(), expected.join("\n"));
}

#[test]
fn moves_every_descendant_with_its_parent() {
    let children: Vec<String> = (0..500).map(|i| format!("  - child {i}")).collect();
    let mut lines = vec!["- before".to_string(), "- parent".to_string()];
    lines.extend(children.iter().cloned());
    let text = lines.join("\n");
    let mut d = doc(&text, text.find("parent").unwrap());
    d.change_list_indent(false);
    let mut expected = vec!["- before".to_string(), "  - parent".to_string()];
    expected.extend(children.iter().map(|line| format!("  {line}")));
    assert_eq!(d.text(), expected.join("\n"));
}

#[test]
fn keeps_numbering_changes_local_to_the_moved_lists() {
    let text = "1. parent\n2. item\n3. last\n\nOutside\n\n7. untouched\n9. authored";
    let mut d = doc(text, text.find("item").unwrap());
    d.change_list_indent(false);
    assert_eq!(
        d.text(),
        "1. parent\n   1. item\n2. last\n\nOutside\n\n7. untouched\n9. authored"
    );
}

#[test]
fn moves_continuation_lines_and_children_once_when_their_parent_is_selected() {
    let text = "- parent\n- item\n  continued\n  - child\n- last";
    let mut d = selected(
        text,
        text.find("- item").unwrap(),
        text.find("- last").unwrap(),
    );
    d.change_list_indent(false);
    assert_eq!(
        d.text(),
        "- parent\n  - item\n    continued\n    - child\n- last"
    );
    d.change_list_indent(true);
    assert_eq!(d.text(), text);
}

#[test]
fn moves_tab_indented_items_by_columns_while_retaining_their_child_structure() {
    let text = "- root\n\t- parent\n\t- item\n\t  - child";
    let mut d = doc(text, text.find("item").unwrap());
    d.change_list_indent(false);
    assert_eq!(
        d.text(),
        "- root\n\t- parent\n      - item\n        - child"
    );
    let depth2 = d
        .projection()
        .blocks
        .iter()
        .filter(|b| matches!(b.kind, BlockKind::ListItem { depth: 2, .. }))
        .count();
    assert_eq!(depth2, 1);
    d.change_list_indent(true);
    assert_eq!(d.text(), "- root\n\t- parent\n    - item\n      - child");
}

#[test]
fn indents_selected_siblings_as_one_group_and_restores_them_together() {
    let text = "1. parent\n2. first\n3. second\n4. last";
    let mut d = selected(text, text.find("2.").unwrap(), text.find("4.").unwrap());
    d.change_list_indent(false);
    assert_eq!(d.text(), "1. parent\n   1. first\n   2. second\n2. last");
    d.change_list_indent(true);
    assert_eq!(d.text(), text);
}

#[test]
fn keeps_sibling_selection_structural_across_blank_separators_in_a_loose_list() {
    let text = "1. parent\n\n2. first\n\n3. second\n\n4. last";
    let mut d = selected(text, text.find("2.").unwrap(), text.find("4.").unwrap());
    d.change_list_indent(false);
    assert_eq!(
        d.text(),
        "1. parent\n\n   1. first\n\n   2. second\n\n2. last"
    );
    d.change_list_indent(true);
    assert_eq!(d.text(), text);
    assert!(!doc("\n\n", 1).change_list_indent(false));
}

#[test]
fn keeps_invalid_nesting_operations_as_no_ops_and_leaves_other_text_alone() {
    let mut first = doc("- first\n- second", 2);
    assert!(first.change_list_indent(false));
    assert!(first.change_list_indent(true));
    assert_eq!(first.text(), "- first\n- second");
    first.select_all();
    first.change_list_indent(false);
    assert_eq!(first.text(), "- first\n- second");
    assert!(!doc("plain", 2).change_list_indent(false));
    assert!(!doc("```\n- code\n```", 8).change_list_indent(false));
}

#[test]
fn updates_nesting_through_repeated_commands_and_undoes() {
    let text = "- parent\n- item";
    let mut d = doc(text, text.len());
    let nested = |d: &mut LiveDoc| {
        d.projection()
            .blocks
            .iter()
            .filter(|b| matches!(b.kind, BlockKind::ListItem { depth: 1, .. }))
            .count()
    };
    for _ in 0..4 {
        d.change_list_indent(false);
        assert_eq!(nested(&mut d), 1);
        d.change_list_indent(true);
        assert_eq!(d.text(), text);
        assert_eq!(nested(&mut d), 0);
    }
    d.change_list_indent(false);
    d.undo();
    assert_eq!(d.text(), text);
    d.redo();
    assert_eq!(d.text(), "- parent\n  - item");
}

#[test]
fn keeps_an_empty_ordered_items_caret_after_its_marker_spacing() {
    let text = "1. parent\n   1. x";
    let mut d = doc(text, text.len());
    d.backspace();
    assert_eq!(d.text(), "1. parent\n   1. ");
    assert_eq!(head(&d), d.text().len());
    d.insert_text("y");
    assert_eq!(d.text(), "1. parent\n   1. y");
}

#[test]
fn renumbers_later_ordered_siblings_when_enter_adds_an_item() {
    for (text, word, expected) in [
        ("1. one\n2. two", "one", "1. one\n2. \n3. two"),
        ("9) nine\n10) ten", "nine", "9) nine\n10) \n11) ten"),
        (
            "1. parent\n   1. one\n   2. two\n2. last",
            "one",
            "1. parent\n   1. one\n   2. \n   3. two\n2. last",
        ),
        (
            "8. eight\n9. nine\n   - child\n10. last",
            "eight",
            "8. eight\n9. \n10. nine\n    - child\n11. last",
        ),
        (
            "1. [x] done\n2. [ ] next",
            "done",
            "1. [x] done\n2. [ ] \n3. [ ] next",
        ),
    ] {
        let mut d = doc(text, text.find(word).unwrap() + word.len());
        d.enter();
        assert_eq!(d.text(), expected, "{text}");
        assert_eq!(&d.text()[head(&d)..head(&d) + 1], "\n");
    }
}

#[test]
fn keeps_enter_and_following_number_updates_in_one_undo_step() {
    let text = "1. one\n2. two";
    let mut d = doc(text, 6);
    d.enter();
    assert_eq!(d.text(), "1. one\n2. \n3. two");
    d.undo();
    assert_eq!(d.text(), text);
    d.redo();
    assert_eq!(d.text(), "1. one\n2. \n3. two");
}

#[test]
fn does_not_continue_a_thematic_break_as_a_list() {
    for rule in ["* * *", "- - -"] {
        let mut d = doc(rule, rule.len());
        d.enter();
        assert_eq!(d.text(), format!("{rule}\n"));
    }
}

#[test]
fn opens_a_line_above_a_heading_from_the_start_of_its_text() {
    let mut d = doc("# Title\nbody", 2);
    d.enter();
    assert_eq!(d.text(), "\n# Title\nbody");
    assert_eq!(head(&d), 3);
    let mut d = doc("# Title", 4);
    d.enter();
    assert_eq!(d.text(), "# Ti\ntle");
    // From the revealed prefix itself, Enter splits as anywhere else.
    let mut d = doc("### Title", 3);
    d.backspace();
    d.set_caret(3);
    d.enter();
    assert_eq!(d.text(), "\n## Title");
}

#[test]
fn wraps_and_unwraps_bold() {
    let mut d = selected("make bold", 5, 9);
    d.toggle_inline("**");
    assert_eq!(d.text(), "make **bold**");
    assert_eq!(d.selection().range(), 7..11);
    d.toggle_inline("**");
    assert_eq!(d.text(), "make bold");
    assert_eq!(d.selection().range(), 5..9);
}

#[test]
fn makes_bold_text_bold_italic_rather_than_italic() {
    let mut inside = selected("**bold**", 2, 6);
    inside.toggle_inline("*");
    assert_eq!(inside.text(), "***bold***");
    inside.toggle_inline("*");
    assert_eq!(inside.text(), "**bold**");
    let mut whole = selected("**bold**", 0, 8);
    whole.toggle_inline("*");
    assert_eq!(whole.text(), "***bold***");
    let mut italic = selected("*it*", 1, 3);
    italic.toggle_inline("*");
    assert_eq!(italic.text(), "it");
}

// Mirrors "keeps a nascent horizontal rule editable while entering bold on an
// empty line".
#[test]
fn keeps_a_nascent_rule_editable_while_entering_bold_on_an_empty_line() {
    let text = "Unicode \u{2013} \u{1F600}\n\n";
    let mut d = doc(text, text.len());
    d.toggle_inline("**");
    assert_eq!(d.text(), format!("{text}****"));
    let rule = d
        .projection()
        .blocks
        .iter()
        .any(|b| b.kind == BlockKind::Rule { revealed: false });
    assert!(
        !rule,
        "the pair is not drawn as a rule while the caret is in it"
    );
    d.insert_text("bold");
    assert_eq!(d.text(), format!("{text}**bold**"));
}

#[test]
fn toggles_a_task_without_touching_anything_else() {
    let text = "- [ ] open\n\nelsewhere";
    let mut d = doc(text, text.len());
    d.toggle_task(2);
    assert_eq!(d.text(), "- [x] open\n\nelsewhere");
    assert_eq!(head(&d), text.len(), "the caret stays");
    d.toggle_task(2);
    assert_eq!(d.text(), text);
    d.undo();
    assert_eq!(d.text(), "- [x] open\n\nelsewhere");
    // Only at a box.
    d.toggle_task(0);
    assert_eq!(d.text(), "- [x] open\n\nelsewhere");
}

#[test]
fn word_motion_skips_spaces_then_one_run() {
    let text = "one two,  three\nfour";
    let mut d = doc(text, 0);
    let mut seen = Vec::new();
    for _ in 0..6 {
        d.move_word(true, false);
        seen.push(head(&d));
    }
    assert_eq!(seen, vec![3, 7, 8, 15, 20, 20]);
    let mut back = Vec::new();
    for _ in 0..6 {
        d.move_word(false, false);
        back.push(head(&d));
    }
    assert_eq!(back, vec![16, 10, 7, 4, 0, 0]);
    // Shift extends from the anchor; without it a selection collapses first.
    let mut d = doc(text, 4);
    d.move_word(true, true);
    assert_eq!(d.selection().range(), 4..7);
    d.move_word(false, false);
    assert_eq!(head(&d), 4);
}

#[test]
fn word_motion_steps_over_hidden_markup() {
    let text = "- one\n- two";
    let mut d = LiveDoc::new(text);
    d.focused = false;
    d.set_caret(5);
    // The line break and the bullet are the run; the caret lands past the
    // bullet, before the word.
    d.move_word(true, false);
    assert_eq!(head(&d), 8);
    d.move_word(false, false);
    assert_eq!(head(&d), 5);
    let text = "a\n> quoted word";
    let mut d = doc(text, 1);
    d.move_word(true, false);
    assert_eq!(head(&d), text.find("quoted").unwrap());
    let text = "a **bold** b";
    let mut d = doc(text, 1);
    d.move_word(true, false);
    assert_eq!(&text[..head(&d)], "a **", "punctuation is a run of its own");
}

#[test]
fn double_click_selects_a_word_and_triple_click_a_drawn_line() {
    let text = "one two,  three\nnext\n\n> quoted\n> line";
    let mut d = doc(text, 0);
    d.select_word(5, false);
    assert_eq!(d.selection().range(), 4..7);
    d.select_word(7, false);
    assert_eq!(d.selection().range(), 7..8, "punctuation");
    d.select_word(9, false);
    assert_eq!(d.selection().range(), 8..10, "spaces");
    d.select_word(8, true);
    assert_eq!(d.selection().range(), 7..8, "the right half of the comma");
    d.select_word(15, false);
    assert_eq!(
        d.selection().range(),
        10..15,
        "at a line's end, the word before"
    );
    // A paragraph's lines are drawn as one: the triple click takes them all.
    d.select_block(2);
    assert_eq!(d.selection().range(), 0..21);
    let quoted = text.find("> quoted").unwrap();
    d.select_block(text.len() - 1);
    assert_eq!(d.selection().range(), quoted..text.len());
}

#[test]
fn triple_click_in_a_cell_selects_the_cell() {
    let text = "| ab | cd |\n|---|---|\n";
    let mut d = doc(text, 0);
    let ab = text.find("ab").unwrap();
    d.select_block(ab + 1);
    assert_eq!(d.selection().range(), ab..ab + 2);
}

#[test]
fn home_and_end_go_to_the_edge_the_view_found_and_stay_in_a_cell() {
    let mut d = doc("- item text", 9);
    d.move_to_line_edge(false, false, 0);
    assert_eq!(head(&d), 2, "the line's start snaps past the bullet");
    d.move_to_line_edge(true, true, 11);
    assert_eq!(d.selection().range(), 2..11);
    let text = "| ab     | cd |\n|---|---|\n";
    let a = text.find("ab").unwrap();
    let mut d = doc(text, a + 1);
    d.move_to_line_edge(true, false, 0);
    assert_eq!(head(&d), a + 2);
}

#[test]
fn finds_the_link_under_a_position() {
    let mut d = doc("[a](x.md) and <https://a.dev>", 0);
    assert_eq!(d.link_at(1).as_deref(), Some("x.md"));
    assert_eq!(d.link_at(12), None);
    assert_eq!(d.link_at(16).as_deref(), Some("https://a.dev"));
}

// A fence or an HTML comment opened or closed above a table changes how it
// parses; the source is parsed whole, so the table follows.
#[test]
fn a_fence_or_comment_above_a_table_swallows_and_releases_it() {
    let text = "intro\n\nfiller\n\n| a |\n| - |\n| 1 |\n\nend";
    let mut d = doc(text, 0);
    assert_eq!(d.projection().tables.len(), 1);
    d.insert_text("```\n");
    assert_eq!(d.projection().tables.len(), 0);
    d.set_caret(4);
    d.insert_text("code\n```\n");
    assert_eq!(d.projection().tables.len(), 1);
    let text = "<!--\nstart\n\nmore\n\n| a |\n| - |\n| 1 |\n\nend";
    let mut d = doc(text, 10);
    assert_eq!(d.projection().tables.len(), 0);
    d.insert_text(" -->");
    assert_eq!(d.projection().tables.len(), 1);
}

#[test]
fn undo_survives_an_outside_change() {
    let mut d = doc("one\n\ntwo", 3);
    d.insert_text("!");
    d.reset_text("zero\n\none!\n\ntwo");
    d.undo();
    assert_eq!(d.text(), "zero\n\none\n\ntwo");
    d.redo();
    assert_eq!(d.text(), "zero\n\none!\n\ntwo");
}

// Mirrors "finds where a line's text starts after its hidden markup": a caret
// placed at a line's start lands where its text starts.
#[test]
fn a_caret_at_a_lines_start_lands_after_its_hidden_markup() {
    for (text, line, expected) in [
        ("## Title", 0, 3),
        ("> - [x] done", 0, 8),
        ("- a\n  - nested", 4, 8),
        ("1. first", 0, 0),
        ("plain", 0, 0),
        ("Title\n===", 0, 0),
    ] {
        let mut d = LiveDoc::new(text);
        d.focused = false;
        d.set_caret(line);
        assert_eq!(head(&d), expected, "{text}");
    }
}

#[test]
fn triple_click_takes_one_line_of_code_or_after_a_hard_break() {
    let text = "```\naaa\nbbb\n```\n\none  \ntwo\n";
    let mut d = doc(text, 0);
    let aaa = text.find("aaa").unwrap();
    d.select_block(aaa + 1);
    assert_eq!(d.selection().range(), aaa..aaa + 4);
    let two = text.find("two").unwrap();
    d.select_block(two + 1);
    assert_eq!(d.selection().range(), two..text.len());
}

// Mirrors continueMarkup's `range.head < line.from + marker.length`.
#[test]
fn enter_before_the_end_of_a_number_only_breaks_the_line() {
    for caret in [0, 1, 2] {
        let mut d = doc("1. text\n2. b", caret);
        d.enter();
        let mut want = "1. text\n2. b".to_string();
        want.insert(caret, '\n');
        assert_eq!(d.text(), want, "caret {caret}");
    }
}

#[test]
fn enter_in_a_quote_keeps_its_depth_and_an_empty_quote_line_ends_it() {
    let mut d = doc("> > a", 5);
    d.enter();
    assert_eq!(d.text(), "> > a\n> >\n> > ");
    assert_eq!(head(&d), d.text().len());
    let mut d = doc("> a\n> ", 6);
    d.enter();
    assert_eq!(d.text(), "> a\n");
    assert_eq!(head(&d), 4);
    let mut d = doc("> # h", 5);
    d.enter();
    assert_eq!(d.text(), "> # h\n>\n> ");
    // The caret waits on the new line, so typing starts the new paragraph.
    assert_eq!(head(&d), d.text().len());
    d.insert_text("b");
    assert_eq!(d.text(), "> # h\n>\n> b");
}

/// The items drawn, as (depth, text).
fn drawn_items(d: &mut LiveDoc) -> Vec<(usize, String)> {
    let p = d.projection();
    p.blocks
        .iter()
        .filter_map(|b| match b.kind {
            BlockKind::ListItem {
                depth,
                marker: Some(_),
                ..
            } => Some((depth, p.leaves[b.leaves[1]].text.clone())),
            _ => None,
        })
        .collect()
}

// Lezer, which Tauri's tests ran on, lets an empty item or one not numbered
// 1 break into a paragraph inside a list; so does the parse here.
#[test]
fn tab_on_a_new_empty_item_nests_it_and_enter_there_ends_the_nested_list() {
    for (text, nested, ended) in [
        ("- a\n- b\n- ", "- a\n- b\n  - ", "- a\n- b\n  "),
        ("1. a\n2. ", "1. a\n   1. ", "1. a\n   "),
    ] {
        let mut d = doc(text, text.len());
        assert!(d.change_list_indent(false));
        assert_eq!(d.text(), nested);
        let items = drawn_items(&mut d);
        assert_eq!(items.last(), Some(&(1, String::new())), "{text}");
        assert!(
            items
                .iter()
                .all(|(_, t)| !t.contains('-') && !t.contains("1."))
        );
        d.enter();
        assert_eq!(d.text(), ended);
    }
}

#[test]
fn outdenting_from_a_list_that_starts_at_five_keeps_the_rest_an_item() {
    let text = "1. parent\n\n   5. first\n   6. item\n   7. remaining\n2. last";
    let mut d = doc(text, text.find("item").unwrap());
    assert!(d.change_list_indent(true));
    assert_eq!(
        d.text(),
        "1. parent\n\n   5. first\n2. item\n   5. remaining\n3. last"
    );
    let items: Vec<_> = drawn_items(&mut d).into_iter().map(|(_, t)| t).collect();
    assert_eq!(items, ["parent", "first", "item", "remaining", "last"]);
}

// Mirrors continueMarkup: a later paragraph of an item takes the marker of
// the item's first line; a quote in an item is continued as a quote.
#[test]
fn enter_in_an_items_later_paragraph_or_quote_continues_it() {
    for (text, expected) in [
        ("- item\n\n  later", "- item\n\n  later\n- "),
        (
            "1. item\n\n   later\n2. next",
            "1. item\n\n   later\n2. \n3. next",
        ),
        ("- a\n  > q", "- a\n  > q\n  >\n  > "),
    ] {
        let mut d = doc(text, text.find(['r', 'q']).unwrap() + 1);
        d.enter();
        assert_eq!(d.text(), expected, "{text}");
    }
}

#[test]
fn text_after_an_empty_nested_item_stays_its_own() {
    for (text, line) in [
        ("- a\n  - \nSome text", "Some text"),
        ("- a\n  - \n  more", "more"),
        ("1. a\n   2. \nSome text", "Some text"),
    ] {
        let mut d = doc(text, text.len());
        let p = d.projection();
        assert!(p.leaves.iter().any(|l| l.text == line), "{text}");
        d.backspace();
        assert_eq!(d.text(), &text[..text.len() - 1], "{text}");
    }
}

#[test]
fn enter_on_an_empty_nested_item_leaves_the_caret_on_its_line() {
    let mut d = doc("- a\n  - ", 8);
    d.enter();
    assert_eq!(d.text(), "- a\n  ");
    assert_eq!(head(&d), 6);
    d.insert_text("b");
    assert_eq!(d.text(), "- a\n  b");
}

#[test]
fn a_space_typed_before_a_soft_break_keeps_the_caret_on_its_line() {
    let mut d = doc("first line\nsecond line", 10);
    d.insert_text(" ");
    assert_eq!(head(&d), 11);
    d.insert_text("ab");
    assert_eq!(d.text(), "first line ab\nsecond line");
}

#[test]
fn enter_on_an_empty_nested_item_between_items_keeps_the_caret_there() {
    let text = "- alpha\n  - \n- beta";
    let mut d = doc(text, 12);
    d.enter();
    assert_eq!(d.text(), "- alpha\n  \n- beta");
    assert_eq!(head(&d), 10);
    d.insert_text("X");
    assert_eq!(d.text(), "- alpha\n  X\n- beta");
}

// markdown-rs 1.0.0 panics on this source ("mismatched ListOrdered /
// ListUnordered"), which typing can reach.
#[test]
fn a_source_the_parser_panics_on_draws_as_plain_lines() {
    let bad = "  - a10.\n  - ```\n   2. \n   2. ";
    let mut d = doc("- item", 6);
    d.doc
        .edit(0..6, bad, Selection::caret(bad.len()), EditKind::Other);
    let drawn: Vec<_> = d
        .projection()
        .leaves
        .iter()
        .map(|l| l.text.clone())
        .collect();
    assert_eq!(drawn, ["  - a10.", "  - ```", "   2. ", "   2. "]);
    // Editing goes on, on the plain lines.
    d.set_caret(bad.find("```").unwrap());
    d.insert_text("x");
    assert_eq!(d.text(), "  - a10.\n  - x```\n   2. \n   2. ");
    // Text that parses again is drawn as Markdown again.
    d.undo();
    d.undo();
    assert_eq!(d.text(), "- item");
    assert_eq!(d.projection().leaves[0].text, "\u{2022}");
}

#[test]
fn ctrl_b_in_a_cell_follows_the_cells_rules() {
    let text = "| ab | cd |\n|---|---|\n";
    let ab = text.find("ab").unwrap();
    let mut through_inline = selected(text, ab, ab + 2);
    through_inline.toggle_inline("**");
    let mut in_cell = selected(text, ab, ab + 2);
    assert!(in_cell.toggle_marker_in_cell("**"));
    assert_eq!(through_inline.text(), in_cell.text());
    assert_eq!(through_inline.selection(), in_cell.selection());
}

// As prose: the same key again, with only the text still selected, unwraps.
#[test]
fn ctrl_b_or_i_twice_in_a_cell_or_in_prose_gives_the_text_back() {
    let text = "| a | b |\n|---|---|\n| 1 | 2 |\n\nword\n";
    for marker in ["**", "*"] {
        for at in [text.find('2').unwrap(), text.find("word").unwrap()] {
            let len = if text[at..].starts_with('2') { 1 } else { 4 };
            let mut d = selected(text, at, at + len);
            d.toggle_inline(marker);
            assert_ne!(d.text(), text);
            d.toggle_inline(marker);
            assert_eq!(d.text(), text, "{marker} at {at}");
            assert_eq!(d.selection().range(), at..at + len);
        }
    }
    // An empty pair from a caret goes away the same way.
    let one = text.find('1').unwrap();
    let mut d = doc(text, one + 1);
    d.toggle_inline("**");
    assert_eq!(&d.text()[one..one + 5], "1****");
    d.toggle_inline("**");
    assert_eq!(d.text(), text);
    // So does an empty italic pair, which is not a bold marker.
    let mut d = doc(text, one + 1);
    d.toggle_inline("*");
    d.toggle_inline("*");
    assert_eq!(d.text(), text);
    // Selected text starting with a lone `*` unwraps as in prose.
    let mut d = selected("| a |\n|---|\n| **x* |\n", 15, 17);
    d.toggle_inline("*");
    assert_eq!(d.text(), "| a |\n|---|\n| *x |\n");
    assert_eq!(d.selected_text(), "*x");
    // Italic around bold text wraps it, not half the bold marker.
    let bold = "| a |\n|---|\n| **2** |\n";
    let two = bold.find('2').unwrap();
    let mut d = selected(bold, two, two + 1);
    d.toggle_inline("*");
    assert_eq!(d.text(), "| a |\n|---|\n| ***2*** |\n");
}
