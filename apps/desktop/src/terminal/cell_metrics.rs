//! Cell size: the arithmetic the tmux client size comes from. Port of
//! `features/terminal/cellMetrics.ts`, without the half that reads xterm's
//! internals: here the cell is measured from the font, and every cell is whole
//! in device pixels, so columns never walk.

use std::sync::Arc;

use crate::theme::metrics::ui;
use gpui_kit::{Font, FontFeatures, FontStyle, FontWeight, Pixels, Window, px};
use terminal_engine::GridSize;

/// What a pane spends out of its box, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct BoxChrome {
    pub horizontal: f32,
    pub vertical: f32,
    pub scrollbar: f32,
}

/// xterm reserves this on the right whenever there is scrollback, and the
/// client size has always been computed with it. The scrollbar T3.5 paints
/// lives in this strip, so the column count stays what tmux already has.
pub const SCROLLBAR_WIDTH: f32 = 14.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelBox {
    pub width: f32,
    pub height: f32,
}

/// How much of the face's own pitch a row may give up to avoid being resampled.
///
/// At a ratio of 1.25 the smallest row whole in both spaces is a multiple of 4
/// CSS px, which would drag a 17.6 px pitch to 20, 14% of every row. The budget
/// is stated instead of falling out of an unbounded search.
const MAX_PITCH_SACRIFICE_PX: f32 = 1.;

/// The row pitch for a face measuring `char_height`: the nearest row whole in
/// both CSS and device pixels, or the face's own pitch when nothing inside the
/// budget is.
pub fn device_safe_row(char_height: f32, scale: f32) -> f32 {
    if char_height <= 0. {
        return 0.;
    }
    let ratio = if scale > 0. { scale } else { 1. };
    let native = (char_height * ratio).ceil() / ratio;
    let mut row = native.ceil();
    while row <= (native + MAX_PITCH_SACRIFICE_PX).floor() {
        // Compared in whole device pixels: `row * ratio` can miss an integer by
        // a rounding error.
        if ((row * ratio).round() - row * ratio).abs() < 1e-3 {
            return row;
        }
        row += 1.;
    }
    native
}

/// The cell width for a face advancing `advance` per glyph: rounded up to whole
/// device pixels, so a 13 px face is an 8 CSS px cell at every ratio, never
/// narrower than the face and under one device pixel wider.
pub fn device_safe_width(advance: f32, scale: f32) -> f32 {
    if advance <= 0. {
        return 0.;
    }
    let ratio = if scale > 0. { scale } else { 1. };
    (advance * ratio).ceil() / ratio
}

// NaN is not positive either, and a NaN measurement must not size a client.
fn is_positive(value: f32) -> bool {
    value > 0.
}

/// Cells that fit a box. Floors, never rounds: rounding up asks tmux for a grid
/// the surface cannot show, which is how a pane loses its bottom row.
pub fn cells_for_box(area: PixelBox, cell: PixelBox, chrome: BoxChrome) -> Option<GridSize> {
    if !is_positive(cell.width) || !is_positive(cell.height) {
        return None;
    }
    let width = area.width - chrome.horizontal - chrome.scrollbar;
    let height = area.height - chrome.vertical;
    if !is_positive(width) || !is_positive(height) {
        return None;
    }
    Some(GridSize {
        columns: (width / cell.width).floor() as usize,
        rows: (height / cell.height).floor() as usize,
    })
}

/// The face and cell one pane's text is laid out on.
#[derive(Clone, Debug, PartialEq)]
pub struct CellMetrics {
    pub font: Font,
    pub font_size: Pixels,
    pub cell_width: Pixels,
    pub cell_height: Pixels,
    /// An underline or strikeout: follows the UI scale with the font.
    pub line_width: Pixels,
    /// The strip kept clear for the scrollbar. It scales with the rest of the
    /// UI, so the column count does too.
    pub scrollbar_width: Pixels,
}

impl CellMetrics {
    pub fn cell(&self) -> PixelBox {
        PixelBox {
            width: f32::from(self.cell_width),
            height: f32::from(self.cell_height),
        }
    }
}

/// JetBrains Mono ligates through `calt`, which a terminal grid cannot show.
pub(crate) fn terminal_font_features() -> FontFeatures {
    FontFeatures(Arc::new(
        ["calt", "liga", "clig", "dlig"]
            .iter()
            .map(|feature| ((*feature).to_owned(), 0))
            .collect(),
    ))
}

/// Measures a cell for `family` at `font_size`, in this window's device pixels.
pub fn measure(window: &Window, family: &str, font_size: Pixels) -> CellMetrics {
    let font = Font {
        family: family.to_owned().into(),
        features: terminal_font_features(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    };
    let scale = window.scale_factor();
    let text = window.text_system();
    let font_id = text.resolve_font(&font);
    let advance = text
        .advance(font_id, font_size, 'm')
        .map(|size| f32::from(size.width))
        .unwrap_or(f32::from(font_size) * 0.6);
    let ascent = f32::from(text.ascent(font_id, font_size));
    let descent = f32::from(text.descent(font_id, font_size));
    CellMetrics {
        font,
        font_size,
        cell_width: px(device_safe_width(advance, scale)),
        cell_height: px(device_safe_row(ascent - descent, scale)),
        line_width: ui(1.).to_pixels(window.rem_size()),
        scrollbar_width: ui(SCROLLBAR_WIDTH).to_pixels(window.rem_size()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_13px_face_is_an_8px_cell_at_every_ratio() {
        for ratio in [1., 1.25, 1.5, 2., 2.5, 3.] {
            assert_eq!(device_safe_width(7.8, ratio), 8., "{ratio}x");
        }
    }

    #[test]
    fn a_cell_is_never_narrower_than_the_face_and_under_one_device_pixel_wider() {
        for ratio in [1_f32, 1.25, 1.5, 1.75, 2., 2.5, 3.] {
            for advance in [6., 7.2, 7.8, 8.4, 9., 10., 15.] {
                let cell = device_safe_width(advance, ratio);
                assert!(cell >= advance);
                assert!((cell - advance) * ratio < 1.);
                let device = cell * ratio;
                assert!((device.round() - device).abs() < 1e-3);
            }
        }
    }

    #[test]
    fn a_row_is_whole_in_both_pixel_spaces_for_at_most_a_css_pixel_of_pitch() {
        for ratio in [1_f32, 1.25, 1.5, 1.75, 2., 2.5, 3.] {
            for height in [12_f32, 15., 16.4, 17., 17.6, 18., 18.3, 19.2, 24.] {
                let row = device_safe_row(height, ratio);
                let native = (height * ratio).ceil() / ratio;
                assert!(row >= native - 1e-3, "{ratio}x {height}");
                assert!(row - native <= 1. + 1e-3, "{ratio}x {height}");
                let device = row * ratio;
                assert!((device.round() - device).abs() < 1e-3, "{ratio}x {height}");
            }
        }
        for ratio in [1., 1.5, 2., 2.5, 3.] {
            let row = device_safe_row(17., ratio);
            assert_eq!(row.fract(), 0., "{ratio}x CSS row");
        }
    }

    #[test]
    fn refuses_to_drag_a_row_past_the_budget() {
        // 1.25: the next whole row is 20 CSS px, too far. 800 px stays 45 rows.
        let row = device_safe_row(17., 1.25);
        assert_eq!(row, 22. / 1.25);
        assert_eq!((800_f32 / row).floor(), 45.);
        assert_eq!(device_safe_row(17., 1.75) * 1.75, 30.);
    }

    #[test]
    fn nothing_measured_is_nothing_derived_and_a_bad_ratio_means_one() {
        assert_eq!(device_safe_row(0., 2.), 0.);
        assert_eq!(device_safe_row(17., 0.), device_safe_row(17., 1.));
        assert_eq!(device_safe_width(0., 2.), 0.);
        assert_eq!(device_safe_width(7.8, -1.), 8.);
    }

    #[test]
    fn cells_for_box_floors_after_spending_the_chrome() {
        let cell = PixelBox {
            width: 8.,
            height: 17.,
        };
        let chrome = BoxChrome {
            horizontal: 12.,
            vertical: 12.,
            scrollbar: 14.,
        };
        let area = PixelBox {
            width: 1000.,
            height: 800.,
        };
        assert_eq!(
            cells_for_box(area, cell, chrome),
            Some(GridSize {
                columns: 121,
                rows: 46,
            })
        );
        let no_chrome = BoxChrome {
            horizontal: 0.,
            vertical: 0.,
            scrollbar: SCROLLBAR_WIDTH,
        };
        assert_eq!(
            cells_for_box(area, cell, no_chrome),
            Some(GridSize {
                columns: 123,
                rows: 47,
            }),
            "the FitAddon case from measureBox.test.ts"
        );
    }

    #[test]
    fn a_box_too_small_or_a_cell_unmeasured_has_no_answer() {
        let cell = PixelBox {
            width: 8.,
            height: 17.,
        };
        let chrome = BoxChrome {
            horizontal: 12.,
            vertical: 12.,
            scrollbar: 14.,
        };
        let tiny = PixelBox {
            width: 20.,
            height: 800.,
        };
        assert!(cells_for_box(tiny, cell, chrome).is_none());
        let flat = PixelBox {
            width: 1000.,
            height: 8.,
        };
        assert!(cells_for_box(flat, cell, chrome).is_none());
        let area = PixelBox {
            width: 1000.,
            height: 800.,
        };
        let no_cell = PixelBox {
            width: 0.,
            height: 17.,
        };
        assert!(cells_for_box(area, no_cell, chrome).is_none());
    }
}
