//! A table wider than its column scrolls sideways, as Tauri's does
//! (`overflow-x: auto`). gpui-base wraps a table's columns down to a floor of
//! `min(content, 4rem)` each and clips what is left over. Live draws a table
//! that would be clipped at the width of those floors and moves it sideways
//! inside the column. gpui-base's own scrolling table is not used: it keeps
//! its scroll position to itself, so the caret's cell could not be kept in
//! view, and it starts to scroll at 160 px columns.
//!
//! Each table's scroll is kept in `LiveDoc`, by where the table starts.

use gpui_kit::*;

use crate::projection::{Leaf, Projection, Style};

/// gpui-base's table metrics (`render_table`): the least width it measures
/// a cell at, a cell's horizontal padding, the divider after every column
/// but the last, and the frame's border on either side.
const CELL_MIN: f32 = 48.;
const CELL_PAD: f32 = 24.;
const DIVIDER: f32 = 1.;
const BORDER: f32 = 1.;

/// A table drawn wider than its column.
pub(super) struct Wide {
    /// Where the table is drawn: its frame's whole width.
    pub(super) track: Pixels,
    /// Each column from the line before it to the line after it, from the
    /// frame's left edge.
    pub(super) columns: Vec<(Pixels, Pixels)>,
}

/// The table with these rows of leaves, when it is wider than `avail` at
/// its columns' floors: their sum, as gpui-base lays them out.
pub(super) fn measure(
    p: &Projection,
    rows: &[Vec<usize>],
    avail: Pixels,
    window: &mut Window,
    cx: &App,
) -> Option<Wide> {
    let count = rows.iter().map(Vec::len).max()?;
    let rem = window.rem_size();
    let floor = f32::from(rems(4.).to_pixels(rem));
    // No floor is wider than 4rem: most tables need no measuring.
    if count as f32 * floor + 2. * BORDER <= f32::from(avail) {
        return None;
    }
    let divider = |col: usize| if col + 1 < count { DIVIDER } else { 0. };
    // As gpui-base's `TableTextStyles`: the header row in the head's weight.
    let body = window.text_style();
    let mut head = body.clone();
    head.font_weight = FontWeight::SEMIBOLD;
    // gpui-base draws a table a step denser than the body.
    let size = px(super::FONT_SIZE * 14. / 15.);
    let mono = base::Theme::global(cx).tokens.typography.mono;
    let mut widths = vec![CELL_MIN; count];
    for (row, leaves) in rows.iter().enumerate() {
        let style = if row == 0 { &head } else { &body };
        for (col, &leaf) in leaves.iter().enumerate() {
            let text = text_width(&p.leaves[leaf], style, size, &mono, window);
            widths[col] = widths[col].max(text + CELL_PAD + divider(col));
        }
    }
    // Rounded up to device pixels, as gpui-base rounds them.
    let scale = window.scale_factor();
    let ceil = |w: f32| (w * scale).ceil() / scale;
    let mut x = BORDER;
    let mut columns = Vec::with_capacity(count);
    for (col, &w) in widths.iter().enumerate() {
        let cell = ceil(w.min(floor) - divider(col)).min(ceil(w - divider(col)));
        columns.push((px(x - 1.), px(x + cell + 1.)));
        x += cell + divider(col);
    }
    let track = px(x + BORDER);
    (track > avail).then_some(Wide { track, columns })
}

/// The widest line of a cell, trimmed, as gpui-base measures it: bold and
/// italic runs in their faces, inline code in the mono face at 7/8 of the
/// size with 4px either side.
fn text_width(
    leaf: &Leaf,
    style: &TextStyle,
    size: Pixels,
    mono: &SharedString,
    window: &Window,
) -> f32 {
    let has = |run: &[usize], what: fn(&Style) -> bool| {
        (leaf.marks.iter()).any(|(r, s)| what(s) && r.start <= run[0] && r.end >= run[1])
    };
    let mut widest = 0f32;
    let mut start = 0;
    for line in leaf.text.split('\n') {
        let from = start + (line.len() - line.trim_start().len());
        start += line.len() + 1;
        let to = from + line.trim().len();
        let mut cuts = vec![from, to];
        cuts.extend(
            (leaf.marks.iter())
                .flat_map(|(r, _)| [r.start, r.end])
                .filter(|&at| at > from && at < to),
        );
        cuts.sort_unstable();
        cuts.dedup();
        // A stretch of code, or of the rest, at a time: each has its size.
        let runs: Vec<&[usize]> = cuts.windows(2).collect();
        let mut width = 0f32;
        let mut i = 0;
        while i < runs.len() {
            let code = has(runs[i], |s| s.code);
            let n = (runs[i..].iter())
                .take_while(|run| has(run, |s| s.code) == code)
                .count();
            let stretch = &runs[i..i + n];
            i += n;
            let text = &leaf.text[stretch[0][0]..stretch[n - 1][1]];
            let shaped: Vec<TextRun> = (stretch.iter())
                .map(|run| {
                    let mut out = style.to_run(run[1] - run[0]);
                    if has(run, |s| s.bold) {
                        out.font.weight = FontWeight::BOLD;
                    }
                    if has(run, |s| s.italic) {
                        out.font.style = FontStyle::Italic;
                    }
                    if code {
                        out.font.family = mono.clone();
                    }
                    out
                })
                .collect();
            let (size, pad) = if code { (size * 0.875, 8.) } else { (size, 0.) };
            let line = window.text_system().layout_line(text, size, &shaped, None);
            width += pad + f32::from(line.width);
        }
        widest = widest.max(width);
    }
    widest
}
