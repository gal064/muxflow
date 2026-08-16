//! Correlating tmux command blocks with the pane they belong to, and turning a
//! rejected command into one readable line.
//!
//! tmux's control protocol names no pane in a `%begin`/`%end` block, so the host
//! writes an untargeted `display-message` marker ahead of every command whose
//! failure has to be attributed: an in-band `send-keys`, a flow-control resume,
//! and a capture. The reader recognises the marker in the block before the one
//! it describes. This module owns that vocabulary — the writer's side lives in
//! `queue_marker`.

use super::validate_tmux_id;

/// What an uncorrelated command block establishes about the block after it.
///
/// Every marker is ordinary `display-message` output, so the only thing that
/// separates them is their prefix; classifying once keeps the reader from
/// having to know which marker shapes exist.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum MarkerBlock {
    Input(String),
    Resume(String),
    Capture(Option<String>),
}

pub(super) fn classify_marker_block(pane_id: Option<String>, lines: &[Vec<u8>]) -> MarkerBlock {
    if let Some(pane_id) = lines.iter().find_map(|line| input_marker_pane(line)) {
        return MarkerBlock::Input(pane_id);
    }
    if let Some(pane_id) = lines.iter().find_map(|line| resume_marker_pane(line)) {
        return MarkerBlock::Resume(pane_id);
    }
    MarkerBlock::Capture(pane_id.or_else(|| lines.iter().find_map(|line| marker_pane(line))))
}

pub(super) fn marker_pane(line: &[u8]) -> Option<String> {
    marker_pane_with_prefix(line, b"__ADE_CAPTURE__:")
}

fn input_marker_pane(line: &[u8]) -> Option<String> {
    marker_pane_with_prefix(line, b"__ADE_INPUT__:")
}

fn resume_marker_pane(line: &[u8]) -> Option<String> {
    marker_pane_with_prefix(line, b"__ADE_RESUME__:")
}

/// Restores the `%` sigil `queue_marker` had to strip: tmux's display message
/// goes through `strftime`, which eats a literal `%0`.
fn marker_pane_with_prefix(line: &[u8], prefix: &[u8]) -> Option<String> {
    let digits = std::str::from_utf8(line.strip_prefix(prefix)?).ok()?;
    let pane = format!("%{digits}");
    validate_tmux_id(&pane, '%').ok()?;
    Some(pane)
}

/// How many of a failing block's output lines are carried into its event.
const MAX_ERROR_DETAIL_LINES: usize = 4;
/// And how much of each. tmux's own messages are one short sentence; anything
/// longer is not an explanation and does not belong in an event. Counted in
/// characters, not bytes, because the truncation has to land on a boundary the
/// text can be cut at.
const MAX_ERROR_DETAIL_LINE_CHARS: usize = 200;

/// Renders an `%error` as one readable line.
///
/// `header` is tmux's three-number command tag, which on its own says only that
/// *something* failed; `lines` is what tmux printed inside the block, which is
/// the actual reason. Only blocks that print nothing on success may pass their
/// lines here — a capture block's lines are the user's screen.
pub(super) fn error_reason(header: &str, lines: &[Vec<u8>]) -> String {
    let text: Vec<_> = lines
        .iter()
        .take(MAX_ERROR_DETAIL_LINES)
        .map(|line| {
            let text = String::from_utf8_lossy(line);
            let text = text.trim();
            match text.char_indices().nth(MAX_ERROR_DETAIL_LINE_CHARS) {
                Some((index, _)) => format!("{}…", &text[..index]),
                None => text.to_owned(),
            }
        })
        .filter(|line| !line.is_empty())
        .collect();
    if text.is_empty() {
        header.to_owned()
    } else {
        format!("{header}: {}", text.join("; "))
    }
}

/// The bound the reader applies while collecting a block's lines, so a
/// misrouted block cannot grow a log detail without limit.
pub(super) fn wants_error_line(collected: usize) -> bool {
    collected < MAX_ERROR_DETAIL_LINES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_marker_survives_the_strftime_pass_that_eats_a_literal_pane_sigil() {
        // `queue_marker` strips the sigil because tmux expands a display message
        // through strftime and drops `%0` as an unknown conversion; the reader
        // has to put it back or every marked block goes uncorrelated.
        assert_eq!(
            input_marker_pane(b"__ADE_INPUT__:12").as_deref(),
            Some("%12")
        );
        assert_eq!(marker_pane(b"__ADE_CAPTURE__:12").as_deref(), Some("%12"));
        assert_eq!(
            resume_marker_pane(b"__ADE_RESUME__:12").as_deref(),
            Some("%12")
        );
        assert_eq!(input_marker_pane(b"__ADE_INPUT__:%12"), None);
        assert_eq!(input_marker_pane(b"__ADE_INPUT__:"), None);
        assert_eq!(input_marker_pane(b"__ADE_INPUT__:1a"), None);
        assert_eq!(input_marker_pane(b"__ADE_CAPTURE__:12"), None);
        assert_eq!(marker_pane(b"__ADE_CAPTURE__:%12"), None);
    }

    #[test]
    fn each_marker_shape_classifies_to_the_block_that_follows_it() {
        assert_eq!(
            classify_marker_block(None, &[b"__ADE_INPUT__:2".to_vec()]),
            MarkerBlock::Input("%2".into())
        );
        assert_eq!(
            classify_marker_block(None, &[b"__ADE_CAPTURE__:2".to_vec()]),
            MarkerBlock::Capture(Some("%2".into()))
        );
        assert_eq!(
            classify_marker_block(None, &[b"__ADE_RESUME__:2".to_vec()]),
            MarkerBlock::Resume("%2".into())
        );
        assert_eq!(
            classify_marker_block(None, &[b"__ADE_MEMBERSHIP__".to_vec()]),
            MarkerBlock::Capture(None)
        );
    }

    /// The `%error` header's three numbers say only that something failed; the
    /// reason tmux printed is inside the block. Carrying it is what made
    /// P12-U001 visible in a log at all.
    #[test]
    fn an_error_detail_carries_the_reason_tmux_printed_and_stays_bounded() {
        assert_eq!(
            error_reason("1786682005 425 1", &[b"parse error: syntax error".to_vec()]),
            "1786682005 425 1: parse error: syntax error"
        );
        assert_eq!(error_reason("1786682005 425 1", &[]), "1786682005 425 1");
        let long = error_reason("1 2 1", &[vec![b'x'; 4096]]);
        assert_eq!(
            long.chars().count(),
            "1 2 1: ".len() + MAX_ERROR_DETAIL_LINE_CHARS + 1
        );
        assert!(long.ends_with('…'));
        assert!(!wants_error_line(MAX_ERROR_DETAIL_LINES));
        assert!(wants_error_line(0));
    }
}
