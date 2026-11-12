//! Enter in a list item: continue the item's marker, or end the list on an
//! empty item. Indent, outdent, renumbering and the other list commands are
//! T7.2's.

use std::ops::Range;

use super::LiveDoc;
use crate::doc::EditKind;

impl LiveDoc {
    pub(super) fn continue_list(&mut self, item: Range<usize>, caret: usize) {
        let text = self.doc.text();
        // The whole line, so indentation and quote markers carry over.
        let from = text[..item.start].rfind('\n').map_or(0, |i| i + 1);
        let to = text[item.start..]
            .find('\n')
            .map_or(text.len(), |i| item.start + i);
        let Some(marker) = list_marker(&text[from..to]) else {
            self.insert_plain(caret, "\n");
            return;
        };
        if text[from + marker.len..to].trim().is_empty() {
            let from = item.start;
            self.doc.replace(from..caret, "", from, EditKind::Other);
            self.settle();
            return;
        }
        let next = format!("\n{}", marker.next());
        self.insert_plain(caret, &next);
    }
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
