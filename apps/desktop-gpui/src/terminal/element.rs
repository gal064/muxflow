//! The custom GPUI element: paints one pane's grid from the terminal's damage
//! through a per-row cache of shaped text. Lives alone so nothing else has to
//! know how a pane reaches the screen.

use std::sync::Arc;
use std::time::Instant;

use alacritty_terminal::grid::Dimensions;
use gpui_kit::{
    App, BorderStyle, Bounds, ContentMask, Element, ElementId, Entity, FontStyle, FontWeight,
    GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId, LineLayout, Pixels, Rgba,
    ShapedLine, SharedString, StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle,
    Window, fill, outline, point, px, quad, relative, rgb, size,
};

use crate::theme::{self, Palette as ThemePalette, metrics::ui};

use super::box_glyphs;
use super::cell_metrics::CellMetrics;
use super::grid::{
    self, CacheStats, CursorShape, CursorSpec, Decoration, Palette, RowSpec, TextStyle,
};
use super::input;
use super::link;
use super::paint_gate::PanePaintGate;
use super::pane::{PaintedGrid, TerminalPane};
use super::reading;

/// Shaped text for one segment of a row, positioned by column.
pub struct ShapedSegment {
    column: usize,
    line: ShapedLine,
}

fn rgb24(color: Rgba) -> grid::Rgb24 {
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u32;
    (channel(color.r) << 16) | (channel(color.g) << 8) | channel(color.b)
}

fn grid_palette(palette: &ThemePalette) -> Palette {
    let ansi = [
        palette.term_0,
        palette.term_1,
        palette.term_2,
        palette.term_3,
        palette.term_4,
        palette.term_5,
        palette.term_6,
        palette.term_7,
        palette.term_8,
        palette.term_9,
        palette.term_10,
        palette.term_11,
        palette.term_12,
        palette.term_13,
        palette.term_14,
        palette.term_15,
    ]
    .map(rgb24);
    Palette {
        ansi,
        fg: rgb24(palette.term_fg),
        bg: rgb24(palette.term_bg),
        cursor: rgb24(palette.term_cursor),
        cursor_text: rgb24(palette.term_cursor_text),
    }
}

fn hsla(color: grid::Rgb24) -> Hsla {
    rgb(color).into()
}

fn text_run(style: TextStyle, len: usize, metrics: &CellMetrics) -> TextRun {
    let mut font = metrics.font.clone();
    if style.bold {
        font.weight = FontWeight::BOLD;
    }
    if style.italic {
        font.style = FontStyle::Italic;
    }
    let mut color = hsla(style.fg);
    // xterm paints dim text at half strength.
    if style.dim {
        color.a *= 0.5;
    }
    let underline = (style.underline != Decoration::None).then(|| UnderlineStyle {
        color: style.underline_color.map(hsla),
        thickness: metrics.line_width,
        wavy: style.underline == Decoration::Curl,
    });
    TextRun {
        len,
        font,
        color,
        background_color: None,
        underline,
        strikethrough: style.strikeout.then_some(StrikethroughStyle {
            thickness: metrics.line_width,
            color: None,
        }),
    }
}

/// GPUI moves a glyph onto its forced cell only once it drifts more than a
/// pixel off it, so a face a fraction narrower than its cell walks up to a pixel
/// off the grid along a run. Every base glyph goes on its cell here; a mark
/// keeps its offset from its base, which is GPUI's own rule for telling them
/// apart.
fn pin_to_cells(line: &mut ShapedLine, cell: Pixels) {
    let mut runs = line.runs.clone();
    let (mut next, mut base_was, mut base_is) = (0_usize, px(f32::NEG_INFINITY), px(0.));
    for glyph in runs.iter_mut().flat_map(|run| run.glyphs.iter_mut()) {
        let x = glyph.position.x;
        if x > base_was + cell * 0.5 {
            base_was = x;
            base_is = cell * next as f32;
            next += 1;
            glyph.position.x = base_is;
        } else {
            glyph.position.x = base_is + (x - base_was);
        }
    }
    // GPUI ends a trailing underline at the line's width, so it must match the
    // pinned cells or the last cell is left without one.
    let pinned = LineLayout {
        font_size: line.font_size,
        width: cell * next as f32,
        ascent: line.ascent,
        descent: line.descent,
        runs,
        len: line.len,
    };
    let layout: &mut Arc<LineLayout> = line;
    *layout = Arc::new(pinned);
}

fn shape_row(spec: &RowSpec, metrics: &CellMetrics, window: &Window) -> Vec<ShapedSegment> {
    let system = window.text_system();
    spec.segments
        .iter()
        .map(|segment| {
            let run = text_run(segment.style, segment.text.len(), metrics);
            let cell = metrics.cell_width * segment.cell_span as f32;
            let mut line = system.shape_line(
                SharedString::from(segment.text.clone()),
                metrics.font_size,
                &[run],
                Some(cell),
            );
            pin_to_cells(&mut line, cell);
            ShapedSegment {
                column: segment.column,
                line,
            }
        })
        .collect()
}

pub struct TerminalElement {
    pane: Entity<TerminalPane>,
    focused: bool,
}

impl TerminalElement {
    pub fn new(pane: Entity<TerminalPane>, focused: bool) -> Self {
        Self { pane, focused }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

pub struct PaneFrame {
    /// `None` until the engine has mounted the pane.
    grid: Option<GridFrame>,
    background: Hsla,
}

struct GridFrame {
    origin: gpui_kit::Point<Pixels>,
    metrics: std::rc::Rc<CellMetrics>,
    cursor: Option<(CursorSpec, Option<ShapedLine>)>,
    cursor_color: Hsla,
    preedit: Option<ShapedLine>,
    rows: usize,
    rendered_rows: usize,
    stats: CacheStats,
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = PaneFrame;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> PaneFrame {
        let palette = theme::palette(cx);
        let background = palette.term_bg.into();
        let grid_palette = grid_palette(&palette);
        let pane = self.pane.read(cx);
        let Some(terminal) = pane.terminal().cloned() else {
            return PaneFrame {
                grid: None,
                background,
            };
        };
        let metrics = pane.metrics();
        let cache = pane.cache.clone();
        let repaint_all = pane.repaint_all.clone();
        let cursor_on = pane.cursor_on();
        let revealed = pane.revealed();
        let preedit = pane.input.preedit.clone();

        // Before reading the grid: anything written after this raises the flag
        // again and wakes the pane, so no update can fall between.
        terminal.take_dirty();
        let (rows_to_shape, cursor, rows) = {
            let mut term = terminal.lock();
            let rows = term.screen_lines();
            let columns = term.columns();
            let resized = cache.borrow_mut().ensure_size(columns, rows);
            let damage = grid::damaged_rows(&mut term);
            let highlights = reading::highlights(pane, &term);
            // Consumed on every paint: a stale flag would turn the next damage
            // into a full rebuild.
            let forced = repaint_all.replace(false);
            let row_list: Vec<usize> = if resized || damage.full || forced {
                (0..rows).collect()
            } else {
                damage.rows
            };
            let specs: Vec<(usize, RowSpec)> = row_list
                .into_iter()
                .map(|row| (row, grid::build_row(&term, row, &grid_palette, &highlights)))
                .collect();
            // Until the pane has its content there is only an empty grid.
            let cursor = revealed
                .then(|| grid::cursor_spec(&term, self.focused, &grid_palette))
                .flatten();
            (specs, cursor, rows)
        };

        let rendered_rows = rows_to_shape.len();
        let mut stats = CacheStats::default();
        {
            let mut cache = cache.borrow_mut();
            for (row, spec) in rows_to_shape {
                cache.update(row, spec, &mut stats, |spec| {
                    shape_row(spec, &metrics, window)
                });
            }
        }

        let cursor = cursor
            // An unfocused cursor is parked, as xterm's is.
            .filter(|cursor| !self.focused || !cursor.blinking || cursor_on)
            .map(|cursor| {
                let glyph = cursor.glyph.as_ref().map(|(text, style)| {
                    let run = text_run(*style, text.len(), &metrics);
                    let cell = metrics.cell_width * cursor.cell_span as f32;
                    let mut line = window.text_system().shape_line(
                        text.clone().into(),
                        metrics.font_size,
                        &[run],
                        Some(cell),
                    );
                    pin_to_cells(&mut line, cell);
                    line
                });
                (cursor, glyph)
            });
        let preedit = preedit.filter(|_| self.focused).map(|text| {
            let run = TextRun {
                len: text.len(),
                font: metrics.font.clone(),
                color: hsla(grid_palette.fg),
                background_color: None,
                underline: Some(UnderlineStyle {
                    color: Some(hsla(grid_palette.fg)),
                    thickness: metrics.line_width,
                    wavy: false,
                }),
                strikethrough: None,
            };
            window
                .text_system()
                .shape_line(text, metrics.font_size, &[run], None)
        });
        let scale = window.scale_factor();
        let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
        let origin = point(snap(bounds.origin.x), snap(bounds.origin.y));
        PaneFrame {
            grid: Some(GridFrame {
                origin,
                metrics,
                cursor,
                cursor_color: hsla(grid_palette.cursor),
                preedit,
                rows,
                rendered_rows,
                stats,
            }),
            background,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        frame: &mut PaneFrame,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.paint_quad(fill(bounds, frame.background));
        let Some(grid) = frame.grid.as_ref() else {
            return;
        };
        input::paint(&self.pane, bounds, window, cx);

        let pane = self.pane.read(cx);
        let (cache, painted_at, note_painted, dirty_since, perf, gate_key, probes) = (
            pane.cache.clone(),
            pane.painted_at.clone(),
            pane.note_painted.clone(),
            pane.dirty_since.clone(),
            pane.perf.clone(),
            pane.gate_key().cloned(),
            pane.probes(),
        );
        let pane_id = pane.pane_id().clone();
        let epoch = pane.link().read(cx).connection_epoch();

        let origin = grid.origin;
        let (cell_w, cell_h) = (grid.metrics.cell_width, grid.metrics.cell_height);
        painted_at.set(Some(PaintedGrid {
            origin,
            cell: size(cell_w, cell_h),
        }));
        let active_ring = self.pane.read(cx).active.then(|| {
            quad(
                bounds,
                px(0.),
                gpui_kit::transparent_black(),
                ui(theme::metrics::FOCUS_RING).to_pixels(window.rem_size()),
                Hsla::from(theme::palette(cx).accent),
                BorderStyle::Solid,
            )
        });
        let cache_ref = cache.borrow();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (row, entry) in cache_ref.rows.iter().enumerate().take(grid.rows) {
                let Some(entry) = entry else {
                    continue;
                };
                let y = origin.y + cell_h * row as f32;
                for span in &entry.spec.bg {
                    let left = origin.x + cell_w * span.start as f32;
                    let width = cell_w * (span.end - span.start) as f32;
                    window.paint_quad(fill(
                        Bounds::new(point(left, y), size(width, cell_h)),
                        hsla(span.color),
                    ));
                }
            }
            // Under the text: the first column starts at this edge.
            if let Some(ring) = active_ring {
                window.paint_quad(ring);
            }
            for (row, entry) in cache_ref.rows.iter().enumerate().take(grid.rows) {
                let Some(entry) = entry else {
                    continue;
                };
                let y = origin.y + cell_h * row as f32;
                for segment in &entry.shaped {
                    let x = origin.x + cell_w * segment.column as f32;
                    let _ =
                        segment
                            .line
                            .paint(point(x, y), cell_h, TextAlign::Left, None, window, cx);
                }
                for cell in &entry.spec.boxes {
                    let at = point(origin.x + cell_w * cell.column as f32, y);
                    let color = hsla(cell.color);
                    box_glyphs::paint(
                        cell.glyph,
                        Bounds::new(at, size(cell_w, cell_h)),
                        color,
                        window,
                    );
                }
            }
            paint_cursor(grid, cell_w, cell_h, origin, window, cx);
        });

        let now = Instant::now();
        let waited = dirty_since
            .take()
            .map(|since| now.saturating_duration_since(since));
        let slow_record = {
            let mut probes = link::lock(&probes);
            let record = waited.and_then(|waited| {
                probes
                    .tail
                    .note_paint(pane_id.as_ref(), waited, window.is_window_active(), now)
            });
            probes.tail.note_frame(now);
            record
        };
        let backend = self.pane.read(cx).link().read(cx).backend().clone();
        if let Some(record) = slow_record {
            backend.record_incident("render.paintSlow", record);
        }
        self.pane.read(cx).last_stats.set(grid.stats);
        let perf_record = perf.borrow_mut().note_paint(
            pane_id.as_ref(),
            grid.rendered_rows,
            grid.stats.reshaped,
            epoch,
            now,
        );
        if let Some(record) = perf_record {
            record_perf(record);
        }
        if note_painted.take()
            && let Some(key) = gate_key
        {
            cx.default_global::<PanePaintGate>()
                .note_painted(key.as_ref());
        }
    }
}

fn record_perf(record: serde_json::Value) {
    if !desktop_core::perf_log::sink::perf_log_enabled() {
        return;
    }
    let line = serde_json::json!({
        "t": chrono::Utc::now().timestamp_millis(),
        "kind": "perf.terminalPane",
    });
    let mut merged = line.as_object().cloned().unwrap_or_default();
    if let serde_json::Value::Object(fields) = record {
        merged.extend(fields);
    }
    let text = serde_json::Value::Object(merged).to_string();
    desktop_core::platform::spawn(async move {
        // A probe that fails is never a reason to disturb the terminal.
        let _ = desktop_core::perf_log::sink::append_perf_log(vec![text]).await;
    });
}

fn paint_cursor(
    grid: &GridFrame,
    cell_w: Pixels,
    cell_h: Pixels,
    origin: gpui_kit::Point<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some((cursor, glyph)) = grid.cursor.as_ref() else {
        return;
    };
    let stroke = ui(2.).to_pixels(window.rem_size());
    let at = point(
        origin.x + cell_w * cursor.column as f32,
        origin.y + cell_h * cursor.row as f32,
    );
    let width = cell_w * cursor.cell_span as f32;
    let cell = Bounds::new(at, size(width, cell_h));
    let color = grid.cursor_color;
    if let Some(preedit) = grid.preedit.as_ref() {
        // While composing, the preedit replaces the cursor: a block cursor
        // under it would hide its first glyph.
        window.paint_quad(fill(
            Bounds::new(at, size(preedit.width(), cell_h)),
            color.opacity(0.25),
        ));
        let _ = preedit.paint(at, cell_h, TextAlign::Left, None, window, cx);
        window.paint_quad(fill(
            Bounds::new(point(at.x + preedit.width(), at.y), size(stroke, cell_h)),
            color,
        ));
        return;
    }
    match cursor.shape {
        CursorShape::Block => {
            window.paint_quad(fill(cell, color));
            if let Some(line) = glyph {
                let _ = line.paint(at, cell_h, TextAlign::Left, None, window, cx);
            }
        }
        CursorShape::Hollow => {
            window.paint_quad(outline(cell, color, gpui_kit::BorderStyle::Solid));
        }
        CursorShape::Underline => {
            window.paint_quad(fill(
                Bounds::new(point(at.x, at.y + cell_h - stroke), size(width, stroke)),
                color,
            ));
        }
        CursorShape::Beam => {
            window.paint_quad(fill(Bounds::new(at, size(stroke, cell_h)), color));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{FontId, GlyphId, ShapedGlyph, ShapedRun};

    fn glyph(index: usize, x: f32) -> ShapedGlyph {
        ShapedGlyph {
            id: GlyphId(0),
            position: point(px(x), px(0.)),
            index,
            is_emoji: false,
        }
    }

    #[test]
    fn every_base_glyph_lands_on_its_cell_and_marks_follow_their_base() {
        // A 7.8 px face in 8 px cells, as GPUI leaves it: within a pixel of
        // the grid, so not forced, until the drift passes a pixel.
        let mut line = ShapedLine::default();
        let layout: &mut Arc<LineLayout> = &mut line;
        *layout = Arc::new(LineLayout {
            runs: vec![ShapedRun {
                font_id: FontId(0),
                glyphs: vec![
                    glyph(0, 0.),
                    glyph(1, 7.8),
                    glyph(2, 15.6),
                    glyph(3, 16.),
                    glyph(5, 23.4),
                    glyph(6, 31.2),
                ],
            }],
            ..LineLayout::default()
        });
        pin_to_cells(&mut line, px(8.));
        let xs: Vec<f32> = line.runs[0]
            .glyphs
            .iter()
            .map(|glyph| f32::from(glyph.position.x))
            .collect();
        assert_eq!(xs, [0., 8., 16., 16.4, 24., 32.]);
        assert_eq!(line.width, px(40.));
    }
}
