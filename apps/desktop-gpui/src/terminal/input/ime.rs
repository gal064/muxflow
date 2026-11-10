//! The pane as a platform text input: typed text, IME composition and its
//! candidate window, and dead keys all arrive here, not as key presses.
//!
//! The composition is never the terminal's text. It is drawn over the cursor
//! (the `preedit`) until the IME commits it, and only the committed text is
//! sent to the program.

use std::ops::Range;

use gpui_kit::{
    App, Bounds, Context, EntityInputHandler, Focusable, Global, Pixels, SharedString,
    UTF16Selection, Window, point, size,
};

use super::super::pane::TerminalPane;

/// The pane whose composition is open, so app shortcuts leave its keys to the IME.
#[derive(Default)]
struct Composing(Option<gpui_kit::FocusHandle>);

impl Global for Composing {}

/// True while the focused element is a terminal pane with an open composition.
pub fn is_composing(window: &Window, cx: &App) -> bool {
    cx.try_global::<Composing>()
        .and_then(|composing| composing.0.as_ref())
        .is_some_and(|focus| focus.is_focused(window))
}

pub(super) fn set_preedit(
    pane: &mut TerminalPane,
    text: Option<SharedString>,
    cx: &mut Context<TerminalPane>,
) {
    // Typed text commits with no composition open: nothing to redraw.
    if pane.input.preedit == text {
        return;
    }
    let focus = pane.focus_handle(cx);
    let composing = cx.default_global::<Composing>();
    if text.is_some() {
        composing.0 = Some(focus);
    } else if composing.0.as_ref() == Some(&focus) {
        composing.0 = None;
    }
    pane.input.preedit = text;
    cx.notify();
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

impl EntityInputHandler for TerminalPane {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let units: Vec<u16> = self.input.preedit.as_deref()?.encode_utf16().collect();
        let range = range.start.min(units.len())..range.end.min(units.len());
        *adjusted_range = Some(range.clone());
        String::from_utf16(&units[range]).ok()
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self.input.preedit.as_deref().map_or(0, utf16_len);
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.input.preedit.as_deref().map(|text| 0..utf16_len(text))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        set_preedit(self, None, cx);
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        set_preedit(self, None, cx);
        if !text.is_empty() {
            // A commit is typing, whichever keys the IME took to produce it.
            super::send_typed(self, text.as_bytes().to_vec(), cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = (!new_text.is_empty()).then(|| SharedString::from(new_text.to_owned()));
        set_preedit(self, text, cx);
    }

    /// Where the candidate window goes: under the composition, at the cursor.
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let painted = self.painted_at()?;
        let (row, column) = {
            let term = self.terminal()?.lock();
            let cursor = term.grid().cursor.point;
            let row = cursor.line.0 + term.grid().display_offset() as i32;
            (row.max(0) as usize, cursor.column.0)
        };
        let cell = painted.cell;
        Some(Bounds::new(
            point(
                painted.origin.x + cell.width * (column + range_utf16.start) as f32,
                painted.origin.y + cell.height * row as f32,
            ),
            size(cell.width, cell.height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        _: gpui_kit::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}
