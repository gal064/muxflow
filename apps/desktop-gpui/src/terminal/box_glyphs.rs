//! Box drawing and block elements, drawn as geometry so they tile across rows
//! and columns. The row pitch is larger than the face's own glyphs, so font
//! glyphs leave gaps here, and xterm's WebGL renderer draws these itself too.

use gpui_kit::{Bounds, Hsla, PathBuilder, Pixels, Window, bounds, fill, point, px, size};

/// Up, right, down, left weight per U+2500..=U+254B: `0xURDL`, 0 for an absent
/// arm, 1 light, 2 heavy. Generated from the Unicode names; dashed lines are 0
/// and drawn as `Shape::Dashed`.
const LINES: [u16; 76] = [
    0x0101, 0x0202, 0x1010, 0x2020, 0x0000, 0x0000, 0x0000, 0x0000, //
    0x0000, 0x0000, 0x0000, 0x0000, 0x0110, 0x0210, 0x0120, 0x0220, //
    0x0011, 0x0012, 0x0021, 0x0022, 0x1100, 0x1200, 0x2100, 0x2200, //
    0x1001, 0x1002, 0x2001, 0x2002, 0x1110, 0x1210, 0x2110, 0x1120, //
    0x2120, 0x2210, 0x1220, 0x2220, 0x1011, 0x1012, 0x2011, 0x1021, //
    0x2021, 0x2012, 0x1022, 0x2022, 0x0111, 0x0112, 0x0211, 0x0212, //
    0x0121, 0x0122, 0x0221, 0x0222, 0x1101, 0x1102, 0x1201, 0x1202, //
    0x2101, 0x2102, 0x2201, 0x2202, 0x1111, 0x1112, 0x1211, 0x1212, //
    0x2111, 0x1121, 0x2121, 0x2112, 0x2211, 0x1122, 0x1221, 0x2212, //
    0x1222, 0x2122, 0x2221, 0x2222,
];

/// Up, right, down, left per U+2550..=U+256C: `0xURDL`, 0 absent, 1 single,
/// 3 double. Generated from the Unicode names.
const DOUBLE_LINES: [u16; 29] = [
    0x0303, 0x3030, 0x0310, 0x0130, 0x0330, 0x0013, 0x0031, 0x0033, //
    0x1300, 0x3100, 0x3300, 0x1003, 0x3001, 0x3003, 0x1310, 0x3130, //
    0x3330, 0x1013, 0x3031, 0x3033, 0x0313, 0x0131, 0x0333, 0x1303, //
    0x3101, 0x3303, 0x1313, 0x3131, 0x3333,
];

const fn arms(packed: u16) -> [u8; 4] {
    [
        (packed >> 12) as u8,
        ((packed >> 8) & 0xf) as u8,
        ((packed >> 4) & 0xf) as u8,
        (packed & 0xf) as u8,
    ]
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Shape {
    /// Up, right, down, left weights.
    Lines([u8; 4]),
    /// Up, right, down, left, where a double arm is two strokes.
    DoubleLines([u8; 4]),
    Dashed {
        horizontal: bool,
        heavy: bool,
        dashes: u8,
    },
    /// The rounded corners, by the two edges they join.
    Arc {
        right: bool,
        down: bool,
    },
    /// A fraction of the cell: `[left, top, right, bottom]` in eighths.
    Block([u8; 4]),
    /// Quadrants of the cell, top-left, top-right, bottom-left, bottom-right.
    Quadrants([bool; 4]),
    Shade(f32),
}

fn shape(glyph: char) -> Option<Shape> {
    let code = u32::from(glyph);
    Some(match code {
        0x2504..=0x250b => {
            let index = code - 0x2504;
            Shape::Dashed {
                horizontal: index % 4 < 2,
                heavy: index % 2 == 1,
                dashes: if index < 4 { 3 } else { 4 },
            }
        }
        0x254c..=0x254f => {
            let index = code - 0x254c;
            Shape::Dashed {
                horizontal: index < 2,
                heavy: index % 2 == 1,
                dashes: 2,
            }
        }
        0x2500..=0x254b => Shape::Lines(arms(LINES[(code - 0x2500) as usize])),
        0x2550..=0x256c => Shape::DoubleLines(arms(DOUBLE_LINES[(code - 0x2550) as usize])),
        0x256d => Shape::Arc {
            right: true,
            down: true,
        },
        0x256e => Shape::Arc {
            right: false,
            down: true,
        },
        0x256f => Shape::Arc {
            right: false,
            down: false,
        },
        0x2570 => Shape::Arc {
            right: true,
            down: false,
        },
        0x2574 => Shape::Lines([0, 0, 0, 1]),
        0x2575 => Shape::Lines([1, 0, 0, 0]),
        0x2576 => Shape::Lines([0, 1, 0, 0]),
        0x2577 => Shape::Lines([0, 0, 1, 0]),
        0x2578 => Shape::Lines([0, 0, 0, 2]),
        0x2579 => Shape::Lines([2, 0, 0, 0]),
        0x257a => Shape::Lines([0, 2, 0, 0]),
        0x257b => Shape::Lines([0, 0, 2, 0]),
        0x257c => Shape::Lines([0, 2, 0, 1]),
        0x257d => Shape::Lines([1, 0, 2, 0]),
        0x257e => Shape::Lines([0, 1, 0, 2]),
        0x257f => Shape::Lines([2, 0, 1, 0]),
        0x2580 => Shape::Block([0, 0, 8, 4]),
        0x2581..=0x2588 => Shape::Block([0, 8 - (code - 0x2580) as u8, 8, 8]),
        0x2589..=0x258f => Shape::Block([0, 0, 8 - (code - 0x2588) as u8, 8]),
        0x2590 => Shape::Block([4, 0, 8, 8]),
        0x2591 => Shape::Shade(0.25),
        0x2592 => Shape::Shade(0.5),
        0x2593 => Shape::Shade(0.75),
        0x2594 => Shape::Block([0, 0, 8, 1]),
        0x2595 => Shape::Block([7, 0, 8, 8]),
        0x2596 => Shape::Quadrants([false, false, true, false]),
        0x2597 => Shape::Quadrants([false, false, false, true]),
        0x2598 => Shape::Quadrants([true, false, false, false]),
        0x2599 => Shape::Quadrants([true, false, true, true]),
        0x259a => Shape::Quadrants([true, false, false, true]),
        0x259b => Shape::Quadrants([true, true, true, false]),
        0x259c => Shape::Quadrants([true, true, false, true]),
        0x259d => Shape::Quadrants([false, true, false, false]),
        0x259e => Shape::Quadrants([false, true, true, false]),
        0x259f => Shape::Quadrants([false, true, true, true]),
        _ => return None,
    })
}

/// Each stroke of one arm of a double-line glyph: its offset across the arm from
/// the middle, and how far along it starts past the middle. `low` and `high` are
/// the arms on either side; a stroke turns into a double arm on its own side,
/// and runs to the far stroke of one across from it.
fn double_strokes(weight: u8, low: u8, high: u8, light: Pixels) -> Vec<(Pixels, Pixels)> {
    let half = light / 2.;
    let outer = light * 2.;
    let joint = |near: u8, across: u8| match (near, across) {
        (3, _) => light,
        (0, 3) => -outer,
        _ => -half,
    };
    if weight == 3 {
        vec![(-outer, joint(low, high)), (light, joint(high, low))]
    } else if low == 3 || high == 3 {
        vec![(-half, -outer)]
    } else {
        vec![(-half, -half)]
    }
}

pub fn covers(glyph: char) -> bool {
    shape(glyph).is_some()
}

/// Snaps to the device grid, so a stroke is a whole number of device pixels.
fn snap(value: Pixels, scale: f32) -> Pixels {
    px((f32::from(value) * scale).round() / scale)
}

fn rect(left: Pixels, top: Pixels, right: Pixels, bottom: Pixels) -> Bounds<Pixels> {
    bounds(point(left, top), size(right - left, bottom - top))
}

/// Draws `glyph` in `cell`.
pub fn paint(glyph: char, cell: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let scale = window.scale_factor();
    let Some(shape) = shape(glyph) else {
        return;
    };
    let origin = point(snap(cell.origin.x, scale), snap(cell.origin.y, scale));
    let right = snap(cell.origin.x + cell.size.width, scale);
    let bottom = snap(cell.origin.y + cell.size.height, scale);
    let cell = rect(origin.x, origin.y, right, bottom);
    let middle = point(
        snap(cell.origin.x + cell.size.width / 2., scale),
        snap(cell.origin.y + cell.size.height / 2., scale),
    );
    // One light stroke: at least a device pixel, and about a 13th of the cell.
    let light = px((f32::from(cell.size.height) / 13. * scale).round().max(1.) / scale);
    let fill_rect = |rect: Bounds<Pixels>, color: Hsla, window: &mut Window| {
        window.paint_quad(fill(rect, color));
    };
    match shape {
        Shape::Lines(arms) => {
            for (arm, weight) in arms.into_iter().enumerate() {
                if weight == 0 {
                    continue;
                }
                // Heavy is double the light stroke, centered on the same axis.
                let thick = if weight == 2 { light * 2. } else { light };
                let half = thick / 2.;
                let area = match arm {
                    0 => rect(
                        middle.x - half,
                        cell.origin.y,
                        middle.x + half,
                        middle.y + half,
                    ),
                    1 => rect(middle.x - half, middle.y - half, right, middle.y + half),
                    2 => rect(middle.x - half, middle.y - half, middle.x + half, bottom),
                    _ => rect(
                        cell.origin.x,
                        middle.y - half,
                        middle.x + half,
                        middle.y + half,
                    ),
                };
                fill_rect(area, color, window);
            }
        }
        Shape::DoubleLines(arms) => {
            for (arm, weight) in arms.into_iter().enumerate() {
                if weight == 0 {
                    continue;
                }
                let horizontal = arm % 2 == 1;
                // A stroke's low and high side, and which way the arm runs.
                let (low, high) = if horizontal {
                    (arms[0], arms[2])
                } else {
                    (arms[3], arms[1])
                };
                let forward = arm == 1 || arm == 2;
                let (axis, near, far) = if horizontal {
                    (middle.x, origin.x, right)
                } else {
                    (middle.y, origin.y, bottom)
                };
                for (across, along) in double_strokes(weight, low, high, light) {
                    let (from, to) = if forward {
                        (axis + along, far)
                    } else {
                        (near, axis - along)
                    };
                    let area = if horizontal {
                        let top = middle.y + across;
                        rect(from, top, to, top + light)
                    } else {
                        let left = middle.x + across;
                        rect(left, from, left + light, to)
                    };
                    fill_rect(area, color, window);
                }
            }
        }
        Shape::Dashed {
            horizontal,
            heavy,
            dashes,
        } => {
            let thick = if heavy { light * 2. } else { light };
            let half = thick / 2.;
            let (start, end) = if horizontal {
                (origin.x, right)
            } else {
                (origin.y, bottom)
            };
            let period = (end - start) / f32::from(dashes);
            for dash in 0..dashes {
                // Centered in its period, so the rhythm carries across cells.
                let at = start + period * f32::from(dash);
                let from = snap(at + period / 6., scale);
                let to = snap(at + period * 5. / 6., scale);
                let area = if horizontal {
                    rect(from, middle.y - half, to, middle.y + half)
                } else {
                    rect(middle.x - half, from, middle.x + half, to)
                };
                fill_rect(area, color, window);
            }
        }
        Shape::Arc {
            right: to_right,
            down,
        } => {
            let start = if down {
                point(middle.x, bottom)
            } else {
                point(middle.x, origin.y)
            };
            let end = if to_right {
                point(right, middle.y)
            } else {
                point(origin.x, middle.y)
            };
            let mut path = PathBuilder::stroke(light);
            path.move_to(start);
            path.curve_to(end, middle);
            if let Ok(built) = path.build() {
                window.paint_path(built, color);
            }
        }
        Shape::Block([left, top, block_right, block_bottom]) => {
            let x = |eighths: u8| {
                snap(
                    origin.x + cell.size.width * (f32::from(eighths) / 8.),
                    scale,
                )
            };
            let y = |eighths: u8| {
                snap(
                    origin.y + cell.size.height * (f32::from(eighths) / 8.),
                    scale,
                )
            };
            fill_rect(
                rect(x(left), y(top), x(block_right), y(block_bottom)),
                color,
                window,
            );
        }
        Shape::Quadrants(quadrants) => {
            let (x, y) = (middle.x, middle.y);
            let areas = [
                rect(origin.x, origin.y, x, y),
                rect(x, origin.y, right, y),
                rect(origin.x, y, x, bottom),
                rect(x, y, right, bottom),
            ];
            for (on, area) in quadrants.into_iter().zip(areas) {
                if on {
                    fill_rect(area, color, window);
                }
            }
        }
        Shape::Shade(alpha) => {
            fill_rect(cell, color.opacity(alpha), window);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(glyph: char) -> [u8; 4] {
        match shape(glyph) {
            Some(Shape::Lines(arms)) => arms,
            other => panic!("{glyph:?} is {other:?}"),
        }
    }

    #[test]
    fn line_glyphs_have_the_arms_their_names_say() {
        assert_eq!(lines('\u{2500}'), [0, 1, 0, 1]); // ─
        assert_eq!(lines('\u{2503}'), [2, 0, 2, 0]); // ┃
        assert_eq!(lines('\u{250c}'), [0, 1, 1, 0]); // ┌
        assert_eq!(lines('\u{251c}'), [1, 1, 1, 0]); // ├
        assert_eq!(lines('\u{2517}'), [2, 2, 0, 0]); // ┗
        assert_eq!(lines('\u{2515}'), [1, 2, 0, 0]); // ┕
        assert_eq!(lines('\u{2516}'), [2, 1, 0, 0]); // ┖
        assert_eq!(lines('\u{252f}'), [0, 2, 1, 2]); // ┯
        assert_eq!(lines('\u{253c}'), [1, 1, 1, 1]); // ┼
        assert_eq!(lines('\u{253d}'), [1, 1, 1, 2]); // ┽
        assert_eq!(lines('\u{2540}'), [2, 1, 1, 1]); // ╀
        assert_eq!(lines('\u{254b}'), [2, 2, 2, 2]); // ╋
    }

    #[test]
    fn diagonals_and_text_stay_with_the_font() {
        for glyph in ['\u{2571}', 'a'] {
            assert!(!covers(glyph), "{glyph:?}");
        }
    }

    #[test]
    fn double_lines_have_the_arms_their_names_say() {
        let doubled = |glyph: char| match shape(glyph) {
            Some(Shape::DoubleLines(arms)) => arms,
            other => panic!("{glyph:?} is {other:?}"),
        };
        assert_eq!(doubled('\u{2550}'), [0, 3, 0, 3]); // ═
        assert_eq!(doubled('\u{2554}'), [0, 3, 3, 0]); // ╔
        assert_eq!(doubled('\u{2552}'), [0, 3, 1, 0]); // ╒
        assert_eq!(doubled('\u{2553}'), [0, 1, 3, 0]); // ╓
        assert_eq!(doubled('\u{255d}'), [3, 0, 0, 3]); // ╝
        assert_eq!(doubled('\u{2560}'), [3, 3, 3, 0]); // ╠
        assert_eq!(doubled('\u{2564}'), [0, 3, 1, 3]); // ╤
        assert_eq!(doubled('\u{256b}'), [3, 1, 3, 1]); // ╫
        assert_eq!(doubled('\u{256c}'), [3, 3, 3, 3]); // ╬
    }

    #[test]
    fn a_double_corner_joins_outer_to_outer_and_inner_to_inner() {
        let light = px(2.);
        // ╔, its right arm: the top stroke runs back to the outer corner, the
        // lower one starts at the inner corner.
        assert_eq!(
            double_strokes(3, 0, 3, light),
            [(-px(4.), -px(4.)), (px(2.), px(2.))],
        );
    }

    #[test]
    fn dashes_count_by_their_name() {
        assert_eq!(
            shape('\u{2506}'), // ┆
            Some(Shape::Dashed {
                horizontal: false,
                heavy: false,
                dashes: 3,
            }),
        );
        assert_eq!(
            shape('\u{2509}'), // ┉
            Some(Shape::Dashed {
                horizontal: true,
                heavy: true,
                dashes: 4,
            }),
        );
        assert_eq!(
            shape('\u{254f}'), // ╏
            Some(Shape::Dashed {
                horizontal: false,
                heavy: true,
                dashes: 2,
            }),
        );
    }

    #[test]
    fn blocks_cover_their_fraction() {
        assert_eq!(shape('\u{2588}'), Some(Shape::Block([0, 0, 8, 8]))); // █
        assert_eq!(shape('\u{2584}'), Some(Shape::Block([0, 4, 8, 8]))); // ▄
        assert_eq!(shape('\u{258c}'), Some(Shape::Block([0, 0, 4, 8]))); // ▌
        assert_eq!(shape('\u{2581}'), Some(Shape::Block([0, 7, 8, 8]))); // ▁
    }
}
