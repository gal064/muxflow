#[derive(Debug, PartialEq, Eq)]
pub(super) struct SeedBuild {
    pub bytes: Vec<u8>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CaptureMetadata {
    cursor_x: u16,
    cursor_y: u16,
    pub alternate_screen: bool,
    bracketed_paste: Option<bool>,
    mouse_standard: bool,
    mouse_button: bool,
    mouse_any: bool,
    mouse_sgr: bool,
    mouse_utf8: bool,
    cursor_visible: bool,
    application_cursor: bool,
    application_keypad: bool,
    wrap: bool,
    focus_reporting: Option<bool>,
}

#[cfg(test)]
pub(super) fn build_seed(
    pane_id: &str,
    visible_lines: Vec<Vec<u8>>,
    saved_normal_lines: Vec<Vec<u8>>,
    metadata_lines: &[Vec<u8>],
) -> Option<SeedBuild> {
    let metadata = capture_metadata(metadata_lines, pane_id)?;
    Some(build_seed_with_metadata(
        visible_lines,
        saved_normal_lines,
        metadata,
    ))
}

pub(super) fn capture_metadata(
    metadata_lines: &[Vec<u8>],
    pane_id: &str,
) -> Option<CaptureMetadata> {
    metadata_lines
        .iter()
        .rev()
        .find_map(|line| parse_capture_metadata(line, pane_id))
}

/// Rebuilds one pane's screen from its two captures.
///
/// The two captures are not "primary and alternate": plain `capture-pane`
/// returns whatever tmux is *displaying*, and `capture-pane -a` returns the
/// saved **normal** grid that exists only while a pane is in the alternate
/// screen (verified against tmux 3.7b). Painting them the other way round put
/// an agent TUI's visible frame into the hidden normal buffer and the stale
/// shell scrollback into the buffer the user sees — every seed of a
/// claude/codex pane came up blank or stale until the program repainted
/// (P12-U003, found by `tests/performance/runtime/run-vt-parity.sh`).
pub(super) fn build_seed_with_metadata(
    visible_lines: Vec<Vec<u8>>,
    saved_normal_lines: Vec<Vec<u8>>,
    metadata: CaptureMetadata,
) -> SeedBuild {
    let (normal_lines, alternate_lines) = if metadata.alternate_screen {
        (&saved_normal_lines, Some(&visible_lines))
    } else {
        (&visible_lines, None)
    };
    let mut seed = b"\x1b[2J\x1b[H".to_vec();
    // `capture-pane -J` returns logical lines by joining cells marked as soft
    // wrapped. Re-enable wrapping while repainting at the authoritative pane
    // width, then restore the child's current wrap mode below.
    set_private_mode(&mut seed, 7, true);
    paint_capture(&mut seed, normal_lines);
    // `capture-pane -e` ends wherever the last cell's attributes left off, so
    // without this the erases below run with that attribute still active and
    // paint it into every cleared cell.
    seed.extend_from_slice(b"\x1b[m");
    if let Some(alternate_lines) = alternate_lines {
        seed.extend_from_slice(b"\x1b[?1049h\x1b[2J\x1b[H");
        paint_capture(&mut seed, alternate_lines);
        seed.extend_from_slice(b"\x1b[m");
    }
    let mut diagnostics = Vec::new();
    if let Some(bracketed_paste) = metadata.bracketed_paste {
        set_private_mode(&mut seed, 2004, bracketed_paste);
    } else {
        set_private_mode(&mut seed, 2004, false);
        diagnostics.push(
            "tmux does not expose bracketed-paste state for this version; PTY termios has no DEC private-mode evidence, so the renderer was explicitly reset off"
                .into(),
        );
    }
    for mode in [1000, 1002, 1003, 1005, 1006] {
        set_private_mode(&mut seed, mode, false);
    }
    let mouse_mode = if metadata.mouse_any {
        Some(1003)
    } else if metadata.mouse_button {
        Some(1002)
    } else if metadata.mouse_standard {
        Some(1000)
    } else {
        None
    };
    if let Some(mode) = mouse_mode {
        set_private_mode(&mut seed, mode, true);
    }
    set_private_mode(&mut seed, 1006, metadata.mouse_sgr);
    set_private_mode(&mut seed, 1005, metadata.mouse_utf8);
    if let Some(focus_reporting) = metadata.focus_reporting {
        set_private_mode(&mut seed, 1004, focus_reporting);
    } else {
        set_private_mode(&mut seed, 1004, false);
        diagnostics.push(
            "tmux does not expose focus-reporting state and PTY termios has no DEC private-mode evidence; renderer explicitly reset off"
                .into(),
        );
    }
    set_private_mode(&mut seed, 25, metadata.cursor_visible);
    set_private_mode(&mut seed, 1, metadata.application_cursor);
    seed.extend_from_slice(if metadata.application_keypad {
        b"\x1b="
    } else {
        b"\x1b>"
    });
    set_private_mode(&mut seed, 7, metadata.wrap);
    seed.extend_from_slice(
        format!("\x1b[{};{}H", metadata.cursor_y + 1, metadata.cursor_x + 1).as_bytes(),
    );
    SeedBuild {
        bytes: seed,
        diagnostics,
    }
}

fn paint_capture(seed: &mut Vec<u8>, lines: &[Vec<u8>]) {
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            seed.extend_from_slice(b"\r\n");
        }
        seed.extend_from_slice(line);
    }
}

pub(super) fn parse_capture_metadata(line: &[u8], pane_id: &str) -> Option<CaptureMetadata> {
    let line = std::str::from_utf8(line).ok()?;
    let values: Vec<_> = line.split(':').collect();
    if values.len() != 17 || values[0] != "__ADE_META__" || values[1] != pane_id {
        return None;
    }
    let flag = |index: usize, unavailable_default: bool| match values[index] {
        "0" => Some(false),
        "1" => Some(true),
        "" => Some(unavailable_default),
        _ => None,
    };
    let observable_flag = |index: usize| match values[index] {
        "0" => Some(Some(false)),
        "1" => Some(Some(true)),
        "" => Some(None),
        _ => None,
    };
    let pane_width: u16 = values[15].parse().ok()?;
    if pane_width == 0 {
        return None;
    }
    Some(CaptureMetadata {
        cursor_x: values[2].parse().ok()?,
        cursor_y: values[3].parse().ok()?,
        alternate_screen: flag(4, false)?,
        bracketed_paste: observable_flag(5)?,
        mouse_standard: flag(6, false)?,
        mouse_button: flag(7, false)?,
        mouse_any: flag(8, false)?,
        mouse_sgr: flag(9, false)?,
        mouse_utf8: flag(10, false)?,
        cursor_visible: flag(11, true)?,
        application_cursor: flag(12, false)?,
        application_keypad: flag(13, false)?,
        wrap: flag(14, true)?,
        focus_reporting: observable_flag(16)?,
    })
}

fn set_private_mode(seed: &mut Vec<u8>, mode: u16, enabled: bool) {
    seed.extend_from_slice(format!("\x1b[?{mode}{}", if enabled { 'h' } else { 'l' }).as_bytes());
}
