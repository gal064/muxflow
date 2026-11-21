//! List commands: Enter continues an item or ends the list on an empty one,
//! Tab and Shift-Tab move whole items in and out a level, and both renumber
//! the ordered lists they touch, in the same undo step. Ports of
//! `continueMarkup`, `changeListIndent` and `listNumberChanges` in
//! `liveMarkdown.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use markdown::mdast::Node;

use super::LiveDoc;
use super::changes::Changes;
use crate::doc::{EditKind, Selection};
use crate::projection::parse;

/// Columns a tab advances to, as CodeMirror's default `tabSize`.
const TAB: usize = 4;

impl LiveDoc {
    /// Enter in a list item: the next item, numbered in turn, or on an empty
    /// item the end of the list. In a later paragraph of the item, the next
    /// item takes the marker of the item's first line.
    pub(super) fn continue_list(&mut self, caret: usize) {
        self.refresh();
        let text = self.doc.text();
        let lists = Lists::new(text, self.tree.ast());
        let Some(item) = lists.at(caret).map(|at| lists.items[at].range.clone()) else {
            return self.insert_plain(caret, "\n");
        };
        // The item's first line, so indentation and quote markers carry over.
        let from = text[..item.start].rfind('\n').map_or(0, |i| i + 1);
        let to = text[item.start..]
            .find('\n')
            .map_or(text.len(), |i| item.start + i);
        // Before the end of a visible number, Enter only breaks the line.
        let Some(marker) = list_marker(&text[from..to]).filter(|m| caret >= from + m.len) else {
            self.insert_plain(caret, "\n");
            return;
        };
        if caret <= to && text[from + marker.len..to].trim().is_empty() {
            let from = item.start;
            self.doc.replace(from..caret, "", from, EditKind::Other);
            self.settle();
            return;
        }
        let next = format!("\n{}", marker.next());
        // Later ordered siblings move up a number in the same step.
        let added = Changes::new(vec![(caret..caret, next)]);
        let after = added.apply(text);
        let affected: Vec<usize> = lists.at(caret).into_iter().collect();
        let numbers = number_changes(text, &lists, &added, &after, &affected, &[]);
        let head = numbers.map(added.map(caret, true), false);
        self.set_source(&numbers.apply(&after), Selection::caret(head));
    }

    /// Enter on a quote's line, outside code: a blank quote line and a new
    /// paragraph at the same depth; on an empty quote line, the end of the
    /// quote. False on any other line.
    pub(super) fn continue_quote(&mut self, caret: usize) -> bool {
        self.refresh();
        let text = self.doc.text();
        let from = text[..caret].rfind('\n').map_or(0, |i| i + 1);
        let to = text[caret..].find('\n').map_or(text.len(), |i| caret + i);
        let line = &text[from..to];
        let mut len = 0;
        // `^(\s*>\s?)+`, as Tauri's QUOTE_ONLY.
        while let Some(gt) = line[len..].find(|c: char| c != ' ' && c != '\t')
            && line[len + gt..].starts_with('>')
        {
            len += gt + 1;
            len += usize::from(line[len..].starts_with([' ', '\t']));
        }
        if len == 0 || in_code(self.tree.ast(), caret) {
            return false;
        }
        if caret < from + len {
            self.insert_plain(caret, "\n");
        } else if line[len..].trim().is_empty() && caret == to {
            self.doc.replace(from..to, "", from, EditKind::Other);
            self.settle();
        } else {
            let marker = &line[..len];
            self.insert_plain(caret, &format!("\n{}\n{marker}", marker.trim_end()));
        }
        true
    }

    /// Tab (`out` false) or Shift-Tab: the selected items, with their
    /// continuation lines and children, move under the item before them or
    /// out to their parent's level. False where there is no list item to
    /// move, so the key does something else; true, with nothing changed,
    /// where the move is not possible.
    pub fn change_list_indent(&mut self, out: bool) -> bool {
        self.commit_composition();
        self.refresh();
        let text = self.doc.text();
        let range = self.doc.selection().range();
        if range.is_empty() && in_code(self.tree.ast(), range.start) {
            return false;
        }
        let lists = Lists::new(text, self.tree.ast());
        let last = if range.end > range.start {
            range.end - 1
        } else {
            range.end
        };
        let mut selected = BTreeSet::new();
        for line in lines(text, range.start, last) {
            let line_text = &text[line.clone()];
            if line_text.trim().is_empty() {
                continue;
            }
            let (from, indent) = indentation(line_text, usize::MAX);
            let pos = (line.start + from + indent.len()).max(range.start.min(line.end));
            let Some(item) = lists.at(pos) else {
                return false;
            };
            selected.insert(item);
        }
        if selected.is_empty() {
            return false;
        }
        // Items are indexed in document order; a child moves with its parent.
        let mut roots: Vec<usize> = Vec::new();
        for item in selected {
            if roots
                .last()
                .is_none_or(|&root| lists.items[root].range.end < lists.items[item].range.end)
            {
                roots.push(item);
            }
        }
        let column = |item: usize| {
            let it = &lists.items[item];
            indent_columns(&text[line_at(text, it.range.start)], it.quotes) as isize
        };
        let mut changes = Vec::new();
        let mut moved = Vec::new();
        // Adjacent selected siblings move together beneath the sibling before the group.
        let mut group: Option<(usize, usize, isize)> = None;
        for &root in &roots {
            let item = &lists.items[root];
            let previous = lists.previous(root);
            let grouped = group.is_some_and(|(list, end, _)| {
                list == item.list && previous.map(|p| lists.items[p].range.start) == Some(end)
            });
            let delta = match group {
                Some((_, _, delta)) if grouped => delta,
                _ if !out => previous.map_or(0, |p| {
                    let prev = &lists.items[p];
                    let line = line_at(text, prev.range.start);
                    let rest = &text[prev.mark.end..line.end];
                    let gap = rest.len() - rest.trim_start_matches([' ', '\t']).len();
                    let padding = columns(&text[line.start..prev.mark.end + gap])
                        - columns(&text[line.start..prev.mark.end]);
                    // Five or more spaces start code inside the item after one separator.
                    let padding = if padding > 4 { 1 } else { padding.max(1) };
                    column(p) + (prev.mark.len() + padding) as isize - column(root)
                }),
                _ => item
                    .parent
                    .map_or(0, |parent| column(parent) - column(root)),
            };
            group = Some((item.list, item.range.start, delta));
            if delta == 0 {
                continue;
            }
            moved.push(root);
            move_item(text, &lists, root, delta, &mut changes);
        }
        if changes.is_empty() {
            return true;
        }
        let moves = Changes::new(changes);
        let after = moves.apply(text);
        let numbers = number_changes(text, &lists, &moves, &after, &moved, &moved);
        let sel = self.doc.selection();
        let map = |at: usize| numbers.map(moves.map(at, false), false);
        let selection = Selection {
            anchor: map(sel.anchor),
            head: map(sel.head),
        };
        self.set_source(&numbers.apply(&after), selection);
        true
    }
}

fn spaces(count: isize) -> String {
    " ".repeat(count.max(0) as usize)
}

/// Moves every line of `item` by `delta` columns.
fn move_item(
    text: &str,
    lists: &Lists,
    item: usize,
    delta: isize,
    changes: &mut Vec<(Range<usize>, String)>,
) {
    let it = &lists.items[item];
    for line in lines(text, it.range.start, it.range.end) {
        let line_text = &text[line.clone()];
        if line_text.trim().is_empty() {
            continue;
        }
        let (from, indent) = indentation(line_text, it.quotes);
        let width = columns(&line_text[..from + indent.len()]) - columns(&line_text[..from]);
        if delta != 0 {
            let at = line.start + from;
            changes.push((at..at + indent.len(), spaces(width as isize + delta)));
        }
    }
}

/// Renumbers only the ordered lists a structural edit touched, as changes
/// against the edited text. Existing lists keep their starting number; a
/// newly nested list starts at one. `affected` and `moved` are items of
/// `before`; `moved` ones changed list.
fn number_changes(
    before_text: &str,
    before: &Lists,
    edit: &Changes,
    after_text: &str,
    affected: &[usize],
    moved: &[usize],
) -> Changes {
    if !affected
        .iter()
        .any(|&i| before.lists[before.items[i].list].ordered)
    {
        return Changes::new(Vec::new());
    }
    // Structural changes include every sibling, on screen or not.
    let after_ast = parse(after_text);
    let after = Lists::new(after_text, &after_ast);
    let inverse = edit.invert(before_text);
    let moved_starts: BTreeSet<usize> =
        moved.iter().map(|&i| before.items[i].range.start).collect();
    let moved_item = |i: usize| after.at(edit.map(before.items[i].mark.start, true) + 1);
    let mut lists = BTreeSet::new();
    let mut include = |item: Option<usize>| {
        if let Some(item) = item
            && after.lists[after.items[item].list].ordered
        {
            lists.insert(after.items[item].list);
        }
    };
    let mut sources = BTreeSet::new();
    for &item in affected {
        include(moved_item(item));
        sources.insert(before.items[item].list);
    }
    // Removing items can split the old list into several lists.
    for &source in &sources {
        for &sibling in &before.lists[source].items {
            if !moved_starts.contains(&before.items[sibling].range.start) {
                include(moved_item(sibling));
            }
        }
    }
    let mut changes = Vec::new();
    let mut indents: BTreeMap<usize, (usize, usize, isize)> = BTreeMap::new();
    for list in lists {
        let children = &after.lists[list].items;
        // A surviving item identifies the existing list's original start, even
        // when the first item was moved away or a group was inserted before it.
        let mut number = 1;
        for &child in children {
            let mark = after.items[child].mark.start;
            let Some(original) = before.at(inverse.map(mark, true) + 1) else {
                continue;
            };
            let original = &before.items[original];
            if moved_starts.contains(&original.range.start) || !before.lists[original.list].ordered
            {
                continue;
            }
            number = before.number(before_text, before.lists[original.list].items[0]);
            break;
        }
        for &child in children {
            let item = &after.items[child];
            let digits = item.mark.start..item.mark.end - 1;
            let old = &after_text[digits.clone()];
            let next = number.to_string();
            number += 1;
            if old == next {
                continue;
            }
            let delta = next.len() as isize - old.len() as isize;
            changes.push((digits, next));
            if delta == 0 {
                continue;
            }
            // Keep wrapped paragraphs, child lists and fenced blocks at the
            // item's content column when a marker crosses a digit boundary.
            let first = line_at(after_text, item.range.start);
            let prefix = indentation(&after_text[first.clone()], item.quotes).0;
            let content = columns(&after_text[first.start + prefix..item.mark.end]) + 1;
            for line in lines(after_text, first.end + 1, item.range.end) {
                let line_text = &after_text[line.clone()];
                if line_text.trim().is_empty() {
                    continue;
                }
                let (from, indent) = indentation(line_text, item.quotes);
                let width =
                    columns(&line_text[..from + indent.len()]) - columns(&line_text[..from]);
                // Lazy continuation needs no indentation.
                if width < content {
                    continue;
                }
                let at = line.start + from;
                indents.entry(at).or_insert((at + indent.len(), width, 0)).2 += delta;
            }
        }
    }
    for (from, (to, width, delta)) in indents {
        changes.push((from..to, spaces(width as isize + delta)));
    }
    Changes::new(changes)
}

/// Whether `pos` is inside a fenced or indented code block.
fn in_code(node: &Node, pos: usize) -> bool {
    node.children().into_iter().flatten().any(|child| {
        let Some(p) = child.position() else {
            return false;
        };
        let inside = p.start.offset < pos && pos <= p.end.offset;
        match child {
            Node::Code(_) => inside,
            Node::List(_) | Node::ListItem(_) | Node::Blockquote(_) => {
                inside && in_code(child, pos)
            }
            _ => false,
        }
    })
}

/// A list item, flattened out of the AST with what the list commands ask of it.
struct Item {
    range: Range<usize>,
    /// The bullet, or the number with its delimiter.
    mark: Range<usize>,
    list: usize,
    /// The item this one's list is nested in.
    parent: Option<usize>,
    quotes: usize,
}

struct List {
    ordered: bool,
    items: Vec<usize>,
}

/// Every list and item in the document, items in document order.
#[derive(Default)]
struct Lists {
    items: Vec<Item>,
    lists: Vec<List>,
}

impl Lists {
    fn new(src: &str, ast: &Node) -> Self {
        let mut lists = Lists::default();
        lists.walk(src, ast, 0, None);
        lists
    }

    fn walk(&mut self, src: &str, node: &Node, quotes: usize, parent: Option<usize>) {
        for child in node.children().into_iter().flatten() {
            match child {
                Node::List(list) => {
                    let index = self.lists.len();
                    self.lists.push(List {
                        ordered: list.ordered,
                        items: Vec::new(),
                    });
                    for item in &list.children {
                        let Some(p) = item.position() else {
                            continue;
                        };
                        let range = p.start.offset..p.end.offset;
                        let at = self.items.len();
                        self.items.push(Item {
                            mark: mark_at(src, range.start),
                            range,
                            list: index,
                            parent,
                            quotes,
                        });
                        self.lists[index].items.push(at);
                        self.walk(src, item, quotes, Some(at));
                    }
                }
                Node::Blockquote(_) => self.walk(src, child, quotes + 1, parent),
                _ => {}
            }
        }
    }

    /// The innermost item holding `pos`.
    fn at(&self, pos: usize) -> Option<usize> {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.range.start <= pos && pos <= item.range.end)
            .max_by_key(|(_, item)| item.range.start)
            .map(|(at, _)| at)
    }

    fn previous(&self, item: usize) -> Option<usize> {
        let siblings = &self.lists[self.items[item].list].items;
        let at = siblings.iter().position(|&i| i == item)?;
        at.checked_sub(1).map(|at| siblings[at])
    }

    fn number(&self, src: &str, item: usize) -> u64 {
        let mark = &self.items[item].mark;
        src[mark.start..mark.end - 1].parse().unwrap_or(1)
    }
}

fn mark_at(src: &str, at: usize) -> Range<usize> {
    // An item's range can begin with its indentation.
    let at = at + src[at..].len() - src[at..].trim_start_matches([' ', '\t']).len();
    let digits = src[at..].bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 && matches!(src.as_bytes().get(at + digits), Some(b'.' | b')')) {
        at..at + digits + 1
    } else {
        at..at + 1
    }
}

/// The line holding `pos`, without its line break.
fn line_at(src: &str, pos: usize) -> Range<usize> {
    let start = src[..pos].rfind('\n').map_or(0, |i| i + 1);
    let end = src[pos..].find('\n').map_or(src.len(), |i| pos + i);
    start..end
}

/// The lines from the one holding `from` to the one holding `to`.
fn lines(src: &str, from: usize, to: usize) -> impl Iterator<Item = Range<usize>> + '_ {
    let mut next = (from <= to && from <= src.len()).then(|| line_at(src, from).start);
    std::iter::from_fn(move || {
        let start = next.filter(|&start| start <= to)?;
        let line = line_at(src, start);
        next = (line.end < src.len()).then_some(line.end + 1);
        Some(line)
    })
}

/// Columns `text` spans, with a tab to the next multiple of [`TAB`].
fn columns(text: &str) -> usize {
    text.chars().fold(0, |col, c| {
        if c == '\t' {
            col + TAB - col % TAB
        } else {
            col + 1
        }
    })
}

/// The indentation inside up to `quotes` explicit quote markers, keeping the
/// quotes themselves intact: where it starts in `text`, and the indentation.
fn indentation(text: &str, quotes: usize) -> (usize, &str) {
    let blank = |s: &str| s.len() - s.trim_start_matches([' ', '\t']).len();
    let mut from = 0;
    for _ in 0..quotes {
        let lead = blank(&text[from..]);
        if !text[from + lead..].starts_with('>') {
            break;
        }
        from += lead + 1;
        if text[from..].starts_with([' ', '\t']) {
            from += 1;
        }
    }
    (from, &text[from..from + blank(&text[from..])])
}

/// The indentation's width in columns, after the quote prefix.
fn indent_columns(text: &str, quotes: usize) -> usize {
    let (from, indent) = indentation(text, quotes);
    columns(&text[..from + indent.len()]) - columns(&text[..from])
}

struct ListMarker<'a> {
    indent: &'a str,
    bullet: &'a str,
    number: Option<u32>,
    delim: &'a str,
    spacing: &'a str,
    task: bool,
    len: usize,
}

impl ListMarker<'_> {
    /// The marker of the item after this one.
    fn next(&self) -> String {
        let bullet = match self.number {
            Some(n) => format!("{}{}", n + 1, self.delim),
            None => self.bullet.to_string(),
        };
        let task = if self.task { "[ ] " } else { "" };
        format!("{}{}{}{}", self.indent, bullet, self.spacing, task)
    }
}

fn list_marker(line: &str) -> Option<ListMarker<'_>> {
    let indent_len = line.len() - line.trim_start_matches([' ', '\t', '>']).len();
    let (indent, rest) = line.split_at(indent_len);
    let (bullet, after) = if rest.starts_with(['-', '*', '+']) {
        rest.split_at(1)
    } else {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !rest[digits..].starts_with(['.', ')']) {
            return None;
        }
        rest.split_at(digits + 1)
    };
    let number = bullet.strip_suffix(['.', ')']).and_then(|n| n.parse().ok());
    let delim = if number.is_some() {
        &bullet[bullet.len() - 1..]
    } else {
        ""
    };
    let spacing_len = after.len() - after.trim_start_matches([' ', '\t']).len();
    if spacing_len == 0 && !after.is_empty() {
        return None;
    }
    let (spacing, after) = after.split_at(spacing_len);
    let task = ["[ ] ", "[x] ", "[X] "]
        .iter()
        .any(|box_| after.starts_with(box_));
    let len = indent.len() + bullet.len() + spacing.len() + if task { 4 } else { 0 };
    Some(ListMarker {
        indent,
        bullet,
        number,
        delim,
        spacing,
        task,
        len,
    })
}
