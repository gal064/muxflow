//! Mouse reports for a program that asked for the mouse (DEC modes 1000, 1002,
//! 1003) in the encoding it asked for (1006 SGR, 1005 UTF-8, or the original
//! X10 bytes), written from the xterm control-sequence documentation.

use alacritty_terminal::term::TermMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportButton {
    Left,
    Middle,
    Right,
    /// Motion with no button held.
    None,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportAction {
    Press,
    Release,
    Motion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseReport {
    pub button: ReportButton,
    pub action: ReportAction,
    /// Zero-based cell.
    pub column: usize,
    pub row: usize,
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

/// The bytes for `report`, or `None` when the program's mode does not ask for
/// this event or its encoding cannot carry the cell.
pub fn encode_mouse(report: MouseReport, mode: TermMode) -> Option<Vec<u8>> {
    let wanted = match (report.action, report.button) {
        (ReportAction::Motion, ReportButton::None) => mode.contains(TermMode::MOUSE_MOTION),
        (ReportAction::Motion, _) => mode.intersects(TermMode::MOUSE_MOTION | TermMode::MOUSE_DRAG),
        // Wheels report presses only.
        (ReportAction::Release, ReportButton::WheelUp | ReportButton::WheelDown) => false,
        _ => mode.intersects(TermMode::MOUSE_MODE),
    };
    if !wanted {
        return None;
    }
    let sgr = mode.contains(TermMode::SGR_MOUSE);
    let mut code: u32 = match report.button {
        ReportButton::Left => 0,
        ReportButton::Middle => 1,
        ReportButton::Right => 2,
        ReportButton::None => 3,
        ReportButton::WheelUp => 64,
        ReportButton::WheelDown => 65,
    };
    // Only SGR can say which button came up.
    if report.action == ReportAction::Release && !sgr {
        code = 3;
    }
    if report.action == ReportAction::Motion {
        code += 32;
    }
    code += report.shift as u32 * 4 + report.alt as u32 * 8 + report.control as u32 * 16;
    let (x, y) = (report.column as u32 + 1, report.row as u32 + 1);
    if sgr {
        let last = if report.action == ReportAction::Release {
            'm'
        } else {
            'M'
        };
        return Some(format!("\x1b[<{code};{x};{y}{last}").into_bytes());
    }
    let mut bytes = b"\x1b[M".to_vec();
    bytes.push(32 + code as u8);
    for value in [x, y] {
        let value = value + 32;
        if mode.contains(TermMode::UTF8_MOUSE) {
            // xterm's limit for this encoding.
            let ch = char::from_u32(value).filter(|_| value < 2048)?;
            let mut buffer = [0; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buffer).as_bytes());
        } else {
            bytes.push(u8::try_from(value).ok()?);
        }
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(
        button: ReportButton,
        action: ReportAction,
        column: usize,
        row: usize,
    ) -> MouseReport {
        MouseReport {
            button,
            action,
            column,
            row,
            shift: false,
            alt: false,
            control: false,
        }
    }

    const CLICK: TermMode = TermMode::MOUSE_REPORT_CLICK;

    #[test]
    fn x10_bytes_offset_every_field_by_32() {
        let press = report(ReportButton::Left, ReportAction::Press, 0, 0);
        assert_eq!(encode_mouse(press, CLICK).unwrap(), b"\x1b[M !!");
        let release = report(ReportButton::Right, ReportAction::Release, 4, 2);
        assert_eq!(encode_mouse(release, CLICK).unwrap(), b"\x1b[M#%#");
        // 223 is the last column a byte can carry.
        let far = report(ReportButton::Left, ReportAction::Press, 222, 0);
        assert_eq!(encode_mouse(far, CLICK).unwrap(), b"\x1b[M \xff!");
        let beyond = report(ReportButton::Left, ReportAction::Press, 223, 0);
        assert_eq!(encode_mouse(beyond, CLICK), None);
    }

    #[test]
    fn sgr_names_the_released_button_and_has_no_limit() {
        let mode = CLICK | TermMode::SGR_MOUSE;
        let press = report(ReportButton::Middle, ReportAction::Press, 299, 9);
        assert_eq!(encode_mouse(press, mode).unwrap(), b"\x1b[<1;300;10M");
        let release = report(ReportButton::Middle, ReportAction::Release, 299, 9);
        assert_eq!(encode_mouse(release, mode).unwrap(), b"\x1b[<1;300;10m");
        let modified = MouseReport {
            shift: true,
            alt: true,
            control: true,
            ..report(ReportButton::Left, ReportAction::Press, 0, 0)
        };
        assert_eq!(encode_mouse(modified, mode).unwrap(), b"\x1b[<28;1;1M");
    }

    #[test]
    fn utf8_encodes_large_cells_as_characters() {
        let mode = CLICK | TermMode::UTF8_MOUSE;
        let press = report(ReportButton::Left, ReportAction::Press, 299, 0);
        let mut want = b"\x1b[M ".to_vec();
        want.extend_from_slice("\u{14c}!".as_bytes());
        assert_eq!(encode_mouse(press, mode).unwrap(), want);
    }

    #[test]
    fn motion_is_reported_only_in_the_modes_that_ask() {
        let drag = report(ReportButton::Left, ReportAction::Motion, 1, 1);
        let hover = report(ReportButton::None, ReportAction::Motion, 1, 1);
        assert_eq!(encode_mouse(drag, CLICK), None);
        assert_eq!(encode_mouse(hover, CLICK), None);
        let drag_mode = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        assert_eq!(encode_mouse(drag, drag_mode).unwrap(), b"\x1b[<32;2;2M");
        assert_eq!(encode_mouse(hover, drag_mode), None);
        let any_mode = TermMode::MOUSE_MOTION | TermMode::SGR_MOUSE;
        assert_eq!(encode_mouse(hover, any_mode).unwrap(), b"\x1b[<35;2;2M");
        assert_eq!(encode_mouse(drag, TermMode::empty()), None);
    }

    #[test]
    fn wheels_report_presses_only() {
        let mode = CLICK | TermMode::SGR_MOUSE;
        let up = report(ReportButton::WheelUp, ReportAction::Press, 0, 0);
        assert_eq!(encode_mouse(up, mode).unwrap(), b"\x1b[<64;1;1M");
        let down = report(ReportButton::WheelDown, ReportAction::Press, 0, 0);
        assert_eq!(encode_mouse(down, CLICK).unwrap(), b"\x1b[Ma!!");
        let release = report(ReportButton::WheelUp, ReportAction::Release, 0, 0);
        assert_eq!(encode_mouse(release, mode), None);
    }
}
