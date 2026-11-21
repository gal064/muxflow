//! Approach A: the Kit code editor (`EditorState`) with `TextDecoration`s.
//!
//! The cheapest possible attempt at live Markdown: leave the editor's text
//! alone and dress the markers up with decorations. Shows concretely what the
//! decoration and fold APIs can and cannot do:
//! - `TextDecoration` carries a `HighlightStyle`: colour and face only. A
//!   transparent colour hides a marker's ink but the marker keeps its width.
//! - The display map's folds hide whole rows, never a span inside a line.
//! - There is no replacement or block decoration, so a table cannot be drawn
//!   in place of its pipes.
//!
//! This renders the fixture so the gaps are visible; REPORT.md has the verdict.

use std::ops::Range;

use gpui_kit::base::input::{Editor, EditorState, TextDecoration, TextDecorationCollection};
use gpui_kit::*;

use spike_shared::theme;

use crate::projection::{Active, SegKind, parse, project};

/// Source ranges the projection hides when nothing is active (the markers).
pub fn marker_ranges(source: &str) -> Vec<Range<usize>> {
    let p = project(source, &parse(source), &Active::default());
    p.leaves
        .iter()
        .flat_map(|l| &l.segs)
        .filter(|s| s.kind == SegKind::Hidden && !s.src.is_empty())
        .map(|s| s.src.clone())
        .collect()
}

pub struct KitProbe {
    state: Entity<EditorState>,
    _decorations: TextDecorationCollection,
}

impl KitProbe {
    pub fn new(source: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let markers = marker_ranges(&source);
        let state = cx.new(|cx| EditorState::new(window, cx).default_value(source.clone()));
        let decorations = state.update(cx, |state, cx| {
            let transparent = HighlightStyle {
                color: Some(theme::dark().chrome_bg.into()),
                ..Default::default()
            };
            state.create_decorations_collection(markers.into_iter().map(|r| TextDecoration::new(r, transparent)).collect(), cx)
        });
        state.update(cx, |state, cx| state.focus(window, cx));
        Self { state, _decorations: decorations }
    }
}

impl Render for KitProbe {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .p_6()
            .bg(theme::dark().chrome_bg)
            .text_color(theme::dark().chrome_ink)
            .font_family(spike_shared::fonts::MONO)
            .child(Editor::new(&self.state))
    }
}
