//! The custom GPUI element: paints one pane's grid from `Term` damage with a
//! per-row shaped-text cache. Backgrounds, cursor, bold, dim, italic, underline
//! and IME preedit. All GPUI types live here and in `view.rs`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::CursorShape;
use gpui_kit::*;

use crate::feed::{Listener, Pane};
use crate::grid::{self, CacheStats, DARK, Decoration, RowCache, RowSpec, TextStyle};
use crate::view::{PaneView, Shared};

/// Font, size, and integer-pixel cell size the grid is laid out on.
#[derive(Clone)]
pub struct CellMetrics {
    pub font: Font,
    pub font_size: Pixels,
    pub cell_w: Pixels,
    pub cell_h: Pixels,
}

/// Ligature-disabling font features for a terminal: JetBrains Mono ligates via
/// `calt`, so disabling only `liga` would not be enough.
pub fn terminal_font_features() -> FontFeatures {
    FontFeatures(Arc::new(vec![
        ("calt".to_string(), 0),
        ("liga".to_string(), 0),
        ("clig".to_string(), 0),
        ("dlig".to_string(), 0),
    ]))
}

/// Cell width is the `m` advance rounded up to whole device pixels, as a
/// terminal cell must be, so glyphs stay on a stable integer grid.
pub fn cell_metrics(window: &Window, family: &str, font_size: Pixels) -> CellMetrics {
    let font = Font {
        family: family.to_string().into(),
        features: terminal_font_features(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    };
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&font);
    let advance = text_system
        .advance(font_id, font_size, 'm')
        .expect("font has 'm'")
        .width;
    let scale = window.scale_factor();
    let cell_w = px((f32::from(advance) * scale).ceil() / scale);
    let ascent = text_system.ascent(font_id, font_size);
    let descent = text_system.descent(font_id, font_size);
    let cell_h = px(((f32::from(ascent) - f32::from(descent)) * scale).ceil() / scale);
    CellMetrics {
        font,
        font_size,
        cell_w,
        cell_h,
    }
}

fn hsla(color: u32) -> Hsla {
    rgb(color).into()
}

fn run_for(style: TextStyle, len: usize, metrics: &CellMetrics) -> TextRun {
    let mut font = metrics.font.clone();
    if style.bold {
        font.weight = FontWeight::BOLD;
    }
    if style.italic {
        font.style = FontStyle::Italic;
    }
    let underline = match style.underline {
        Decoration::None => None,
        kind => Some(UnderlineStyle {
            color: style.underline_color.map(hsla),
            thickness: px(1.),
            wavy: matches!(kind, Decoration::Curl),
        }),
    };
    TextRun {
        len,
        font,
        color: hsla(style.fg),
        background_color: None,
        underline,
        strikethrough: style.strikeout.then(|| StrikethroughStyle {
            thickness: px(1.),
            color: None,
        }),
    }
}

pub struct ShapedSeg {
    pub col: usize,
    pub line: ShapedLine,
}

fn shape_row(spec: &RowSpec, metrics: &CellMetrics, window: &Window) -> Vec<ShapedSeg> {
    let system = window.text_system();
    spec.segments
        .iter()
        .map(|segment| {
            let run = run_for(segment.style, segment.text.len(), metrics);
            let force = px(f32::from(metrics.cell_w) * segment.cell_span as f32);
            let line = system.shape_line(
                SharedString::from(segment.text.clone()),
                metrics.font_size,
                &[run],
                Some(force),
            );
            ShapedSeg {
                col: segment.col,
                line,
            }
        })
        .collect()
}

pub struct CursorPaint {
    col: usize,
    row: usize,
    cell_span: usize,
    shape: CursorShape,
    glyph: Option<ShapedLine>,
}

pub struct Prepaint {
    cursor: Option<CursorPaint>,
    preedit: Option<ShapedLine>,
    origin: Point<Pixels>,
}

/// The per-pane element. Holds handles only; state lives in the view and `Term`.
pub struct TerminalElement {
    pub view: Entity<PaneView>,
    pub pane: Arc<Pane>,
    pub shared: Arc<Shared>,
    pub focus: FocusHandle,
    pub cache: Rc<RefCell<RowCache<ShapedSeg>>>,
    pub metrics: Rc<CellMetrics>,
    pub cursor_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub preedit: SharedString,
    pub focused: bool,
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

struct CopiedState {
    specs: Vec<(usize, RowSpec)>,
    damaged: usize,
    cursor: Option<CopiedCursor>,
}

struct CopiedCursor {
    col: usize,
    row: usize,
    cell_span: usize,
    shape: CursorShape,
    glyph: Option<(String, TextStyle)>,
}

fn copy_from_term(
    pane: &Pane,
    cache: &mut RowCache<ShapedSeg>,
    shared: &Shared,
    focused: bool,
) -> CopiedState {
    let mut term = pane.term.lock();
    let generation = pane.generation.load(Ordering::Acquire);
    let cols = term.columns();
    let rows = term.screen_lines();
    let resized = cache.cols != cols || cache.rows.len() != rows;
    cache.ensure_size(cols, rows);
    let damage = grid::damaged_rows(&mut term);
    let damaged = damage.rows.len();
    let dirty: Vec<usize> = if resized || damage.full {
        (0..rows).collect()
    } else {
        damage.rows
    };
    let specs = dirty
        .iter()
        .map(|&row| (row, grid::build_row(&term, row as i32, &DARK)))
        .collect();
    // Glyph verification must look at the same grid state the frame paints.
    if let Some((marker, at_cursor)) =
        shared
            .probe
            .on_paint(&pane.id, generation, shared.log.frame_begin())
    {
        shared.probe.mark_verified(grid_has(&term, marker, at_cursor));
    }
    let cursor = copied_cursor(&term, focused);
    CopiedState {
        specs,
        damaged,
        cursor,
    }
}

fn copied_cursor(term: &Term<Listener>, focused: bool) -> Option<CopiedCursor> {
    let cursor = term.renderable_content().cursor;
    if matches!(cursor.shape, CursorShape::Hidden) {
        return None;
    }
    let row = cursor.point.line.0.max(0) as usize;
    let col = cursor.point.column.0;
    let cell = &term.grid()[Line(row as i32)][Column(col)];
    let is_wide = cell.flags.contains(Flags::WIDE_CHAR);
    let shape = if focused {
        cursor.shape
    } else {
        CursorShape::HollowBlock
    };
    // A block cursor redraws the character in inverse so it stays readable.
    let glyph = (focused && matches!(shape, CursorShape::Block) && !cell.c.is_whitespace() && cell.c != '\0')
        .then(|| {
            let mut text = String::from(cell.c);
            if let Some(extra) = cell.zerowidth() {
                text.extend(extra);
            }
            let style = TextStyle {
                bold: cell.flags.contains(Flags::BOLD),
                italic: cell.flags.contains(Flags::ITALIC),
                fg: DARK.cursor_text,
                underline: Decoration::None,
                underline_color: None,
                strikeout: false,
            };
            (text, style)
        });
    Some(CopiedCursor {
        col,
        row,
        cell_span: if is_wide { 2 } else { 1 },
        shape,
        glyph,
    })
}

fn grid_has(term: &Term<Listener>, marker: u8, at_cursor: bool) -> bool {
    let target = marker as char;
    if at_cursor {
        let point = term.grid().cursor.point;
        if point.column.0 == 0 {
            return false;
        }
        return term.grid()[point.line][Column(point.column.0 - 1)].c == target;
    }
    for line in 0..term.screen_lines() as i32 {
        for col in 0..term.columns() {
            if term.grid()[Line(line)][Column(col)].c == target {
                return true;
            }
        }
    }
    false
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let metrics = self.metrics.clone();
        let started = Instant::now();
        let mut cache = self.cache.borrow_mut();
        let copied = copy_from_term(&self.pane, &mut cache, &self.shared, self.focused);
        let copy_elapsed = started.elapsed();

        let shape_started = Instant::now();
        let mut stats = CacheStats::default();
        for (row, spec) in copied.specs {
            cache.update(row, spec, &mut stats, |spec| shape_row(spec, &metrics, window));
        }
        let shape_elapsed = shape_started.elapsed();

        let origin = bounds.origin;
        let cursor = copied.cursor.map(|c| {
            let glyph = c.glyph.map(|(text, style)| {
                let run = run_for(style, text.len(), &metrics);
                window.text_system().shape_line(
                    text.into(),
                    metrics.font_size,
                    &[run],
                    Some(px(f32::from(metrics.cell_w) * c.cell_span as f32)),
                )
            });
            CursorPaint {
                col: c.col,
                row: c.row,
                cell_span: c.cell_span,
                shape: c.shape,
                glyph,
            }
        });
        let preedit = (!self.preedit.is_empty() && self.focused).then(|| {
            let run = TextRun {
                len: self.preedit.len(),
                font: metrics.font.clone(),
                color: hsla(DARK.fg),
                background_color: None,
                underline: Some(UnderlineStyle {
                    color: Some(hsla(DARK.fg)),
                    thickness: px(1.),
                    wavy: false,
                }),
                strikethrough: None,
            };
            window
                .text_system()
                .shape_line(self.preedit.clone(), metrics.font_size, &[run], None)
        });
        if let Some(c) = &cursor {
            let width = preedit
                .as_ref()
                .map(|line| line.width())
                .unwrap_or_else(|| px(f32::from(metrics.cell_w) * c.cell_span as f32));
            self.cursor_bounds.set(Some(Bounds {
                origin: point(
                    origin.x + px(f32::from(metrics.cell_w) * c.col as f32),
                    origin.y + px(f32::from(metrics.cell_h) * c.row as f32),
                ),
                size: size(width, metrics.cell_h),
            }));
        }

        self.shared.log.note_pane_prepaint(
            copied.damaged,
            stats,
            copy_elapsed,
            shape_elapsed,
        );

        Prepaint {
            cursor,
            preedit,
            origin,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let metrics = self.metrics.clone();
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        let origin = prepaint.origin;
        let (cell_w, cell_h) = (metrics.cell_w, metrics.cell_h);
        let cache = self.cache.borrow();
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (row_index, entry) in cache.rows.iter().enumerate() {
                let Some(row) = entry else {
                    continue;
                };
                let y = origin.y + px(f32::from(cell_h) * row_index as f32);
                for span in &row.spec.bg {
                    let rect = Bounds {
                        origin: point(origin.x + px(f32::from(cell_w) * span.start as f32), y),
                        size: size(px(f32::from(cell_w) * (span.end - span.start) as f32), cell_h),
                    };
                    window.paint_quad(fill(rect, hsla(span.color)));
                }
                for segment in &row.shaped {
                    let x = origin.x + px(f32::from(cell_w) * segment.col as f32);
                    let _ = segment.line.paint(
                        point(x, y),
                        cell_h,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            }
            if let Some(cursor) = &prepaint.cursor {
                let at = point(
                    origin.x + px(f32::from(cell_w) * cursor.col as f32),
                    origin.y + px(f32::from(cell_h) * cursor.row as f32),
                );
                let width = px(f32::from(cell_w) * cursor.cell_span as f32);
                let cell = Bounds {
                    origin: at,
                    size: size(width, cell_h),
                };
                // While composing, the preedit replaces the cursor: a block
                // cursor under it would hide its first glyph.
                if prepaint.preedit.is_none() {
                    match cursor.shape {
                        CursorShape::Block => {
                            window.paint_quad(fill(cell, hsla(DARK.cursor)));
                            if let Some(glyph) = &cursor.glyph {
                                let _ = glyph.paint(at, cell_h, TextAlign::Left, None, window, cx);
                            }
                        }
                        CursorShape::HollowBlock => {
                            window.paint_quad(outline(cell, hsla(DARK.cursor), BorderStyle::Solid));
                        }
                        CursorShape::Underline => {
                            window.paint_quad(fill(
                                Bounds {
                                    origin: point(at.x, at.y + cell_h - px(2.)),
                                    size: size(width, px(2.)),
                                },
                                hsla(DARK.cursor),
                            ));
                        }
                        CursorShape::Beam => {
                            window.paint_quad(fill(
                                Bounds {
                                    origin: at,
                                    size: size(px(2.), cell_h),
                                },
                                hsla(DARK.cursor),
                            ));
                        }
                        CursorShape::Hidden => {}
                    }
                }
                if let Some(line) = &prepaint.preedit {
                    window.paint_quad(fill(
                        Bounds {
                            origin: at,
                            size: size(line.width(), cell_h),
                        },
                        hsla(0x3e4451),
                    ));
                    let _ = line.paint(at, cell_h, TextAlign::Left, None, window, cx);
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(at.x + line.width(), at.y),
                            size: size(px(2.), cell_h),
                        },
                        hsla(DARK.cursor),
                    ));
                }
            }
        });
    }
}
