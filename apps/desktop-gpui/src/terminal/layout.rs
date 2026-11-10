//! Where panes sit inside the surface. Port of `features/terminal/layout.ts`
//! and the mount rule from `features/shell/model.ts`.

use tmux_control::Pane;

use super::cell_metrics::PixelBox;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowGrid {
    pub width: u32,
    pub height: u32,
}

pub fn window_grid(panes: &[&Pane]) -> WindowGrid {
    let mut grid = WindowGrid {
        width: 1,
        height: 1,
    };
    for pane in panes {
        grid.width = grid.width.max(u32::from(pane.left) + u32::from(pane.width));
        grid.height = grid
            .height
            .max(u32::from(pane.top) + u32::from(pane.height));
    }
    grid
}

/// The panes a surface mounts: a zoomed window shows its active pane only.
pub fn mounted_panes<'a>(panes: &'a [Pane], window_id: &str, zoomed: bool) -> Vec<&'a Pane> {
    let in_window: Vec<&Pane> = panes
        .iter()
        .filter(|pane| pane.window_id == window_id)
        .collect();
    if !zoomed {
        return in_window;
    }
    let active = in_window
        .iter()
        .find(|pane| pane.active)
        .or(in_window.first());
    active.into_iter().copied().collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaneRect {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

/// A pane's place in the surface, as its share of the window. tmux counts the
/// divider cells into the following pane's offset, so shares land on the edges.
pub fn pane_rect(pane: &Pane, grid: WindowGrid, surface: PixelBox, zoomed: bool) -> PaneRect {
    if zoomed {
        return PaneRect {
            left: 0.,
            top: 0.,
            width: surface.width,
            height: surface.height,
        };
    }
    let (columns, rows) = (grid.width as f32, grid.height as f32);
    PaneRect {
        left: surface.width * f32::from(pane.left) / columns,
        top: surface.height * f32::from(pane.top) / rows,
        width: surface.width * f32::from(pane.width) / columns,
        height: surface.height * f32::from(pane.height) / rows,
    }
}

/// Which of a pane's edges face another pane: `(right, bottom)`. tmux leaves a
/// divider cell there; the window's outer edges have nothing to separate.
pub fn separators(pane: &Pane, grid: WindowGrid, multiple: bool, zoomed: bool) -> (bool, bool) {
    if !multiple || zoomed {
        return (false, false);
    }
    (
        u32::from(pane.left) + u32::from(pane.width) < grid.width,
        u32::from(pane.top) + u32::from(pane.height) < grid.height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: &str, left: u16, top: u16, width: u16, height: u16, active: bool) -> Pane {
        Pane {
            id: id.into(),
            session_id: "$1".into(),
            window_id: "@1".into(),
            index: 0,
            active,
            width,
            height,
            left,
            top,
            current_path: "/".into(),
            current_command: "bash".into(),
            pane_pid: 1,
            start_command: String::new(),
            title: String::new(),
        }
    }

    #[test]
    fn a_window_grid_is_the_furthest_pane_edge_and_never_empty() {
        let (a, b) = (
            pane("%1", 0, 0, 50, 24, true),
            pane("%2", 51, 0, 49, 24, false),
        );
        assert_eq!(
            window_grid(&[&a, &b]),
            WindowGrid {
                width: 100,
                height: 24,
            }
        );
        assert_eq!(
            window_grid(&[]),
            WindowGrid {
                width: 1,
                height: 1,
            }
        );
    }

    #[test]
    fn a_zoomed_window_mounts_its_active_pane_and_fills_the_surface() {
        let panes = [
            pane("%1", 0, 0, 50, 24, false),
            pane("%2", 51, 0, 49, 24, true),
        ];
        let shown = mounted_panes(&panes, "@1", true);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].id, "%2");
        assert_eq!(mounted_panes(&panes, "@1", false).len(), 2);
        assert!(mounted_panes(&panes, "@9", false).is_empty());
        let surface = PixelBox {
            width: 800.,
            height: 600.,
        };
        let grid = WindowGrid {
            width: 100,
            height: 24,
        };
        let rect = pane_rect(shown[0], grid, surface, true);
        assert_eq!(
            rect,
            PaneRect {
                left: 0.,
                top: 0.,
                width: 800.,
                height: 600.,
            }
        );
    }

    #[test]
    fn a_pane_gets_its_share_of_the_surface() {
        let right = pane("%2", 51, 0, 49, 24, false);
        let grid = WindowGrid {
            width: 100,
            height: 24,
        };
        let surface = PixelBox {
            width: 1000.,
            height: 480.,
        };
        assert_eq!(
            pane_rect(&right, grid, surface, false),
            PaneRect {
                left: 510.,
                top: 0.,
                width: 490.,
                height: 480.,
            }
        );
    }

    #[test]
    fn separators_sit_only_on_internal_split_boundaries() {
        let grid = WindowGrid {
            width: 80,
            height: 24,
        };
        let alone = pane("%1", 0, 0, 80, 24, true);
        assert_eq!(separators(&alone, grid, false, false), (false, false));
        let left = pane("%1", 0, 0, 39, 24, true);
        let right = pane("%2", 40, 0, 40, 24, false);
        assert_eq!(separators(&left, grid, true, false), (true, false));
        assert_eq!(separators(&right, grid, true, false), (false, false));
        let top = pane("%1", 0, 0, 80, 11, true);
        let bottom = pane("%3", 0, 12, 80, 12, false);
        assert_eq!(separators(&top, grid, true, false), (false, true));
        assert_eq!(separators(&bottom, grid, true, false), (false, false));
        assert_eq!(separators(&left, grid, true, true), (false, false));
    }
}
