//! The bridge's binary frames, decoded for the terminal path. Port of the pane
//! half of `decodeTerminalEvent` in `features/terminal/api.ts`. Every other
//! frame keeps only its sequence, which is all the hub reads from it.

use alacritty_terminal::grid::Dimensions;
use serde::Deserialize;

/// A pane's character grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub columns: usize,
    pub rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneGrid {
    pub pane_id: String,
    pub size: GridSize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneResourceState {
    Unspecified,
    Visible,
    HiddenBuffered,
    Released,
}

impl PaneResourceState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Unspecified => "unspecified",
            Self::Visible => "visible",
            Self::HiddenBuffered => "hiddenBuffered",
            Self::Released => "released",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaneResource {
    pub pane_id: String,
    pub state: PaneResourceState,
    pub requires_seed: bool,
    pub resume_from_renderer: bool,
    pub recovery_reason: String,
    pub generation: u64,
    pub snapshot_generation: u64,
    pub tail_through_generation: u64,
    pub raw_tail: Vec<u8>,
    pub sequence: u64,
}

/// What the hub routes to one pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaneEvent {
    Seed {
        pane_id: String,
        generation: u64,
        data: Vec<u8>,
        sequence: u64,
    },
    Output {
        pane_id: String,
        generation: u64,
        data: Vec<u8>,
        sequence: u64,
    },
    Resource(PaneResource),
    SeedDiagnostic {
        pane_id: String,
        message: String,
        sequence: u64,
    },
    History {
        pane_id: String,
        data: Vec<u8>,
        history_size: Option<u32>,
        sequence: u64,
    },
}

impl PaneEvent {
    pub fn pane_id(&self) -> &str {
        match self {
            Self::Seed { pane_id, .. }
            | Self::Output { pane_id, .. }
            | Self::SeedDiagnostic { pane_id, .. }
            | Self::History { pane_id, .. } => pane_id,
            Self::Resource(resource) => &resource.pane_id,
        }
    }

    pub fn sequence(&self) -> u64 {
        match self {
            Self::Seed { sequence, .. }
            | Self::Output { sequence, .. }
            | Self::SeedDiagnostic { sequence, .. }
            | Self::History { sequence, .. } => *sequence,
            Self::Resource(resource) => resource.sequence,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    GenerationEpoch {
        epoch: u64,
        sequence: u64,
    },
    Pane(PaneEvent),
    TopologyDirty {
        grids: Option<Vec<PaneGrid>>,
        sequence: u64,
    },
    Snapshot {
        grids: Option<Vec<PaneGrid>>,
        authoritative: bool,
        sequence: u64,
    },
    FlowStalled {
        pane_id: String,
        sequence: u64,
    },
    Other {
        sequence: u64,
    },
}

impl TerminalEvent {
    pub fn sequence(&self) -> u64 {
        match self {
            Self::GenerationEpoch { sequence, .. }
            | Self::TopologyDirty { sequence, .. }
            | Self::Snapshot { sequence, .. }
            | Self::FlowStalled { sequence, .. }
            | Self::Other { sequence } => *sequence,
            Self::Pane(event) => event.sequence(),
        }
    }
}

const COMMON_HEADER_BYTES: usize = 1 + 2 + 8;
const PANE_RESOURCE_HEADER_BYTES: usize = 34;
const TERMINAL_HISTORY_HEADER_BYTES: usize = 5;

/// Decodes one frame. An error is a frame to drop; the hub's sequence watermark
/// is what notices the hole, and its credit is spent either way.
pub fn decode_frame(frame: &[u8]) -> Result<TerminalEvent, String> {
    if frame.len() < COMMON_HEADER_BYTES {
        return Err("terminal frame is shorter than its common header".into());
    }
    let label_len = usize::from(u16::from_be_bytes([frame[1], frame[2]]));
    let payload_at = COMMON_HEADER_BYTES + label_len;
    if frame.len() < payload_at {
        return Err("terminal frame label or sequence is truncated".into());
    }
    let label = text(&frame[3..3 + label_len], "terminal frame label")?;
    let sequence = be_u64(&frame[3 + label_len..payload_at]);
    let data = &frame[payload_at..];
    let host = |what: &str| {
        if sequence == 0 {
            Err(format!("{what} frame must use a nonzero host sequence"))
        } else {
            Ok(())
        }
    };
    match frame[0] {
        kind @ (1 | 2) => {
            let what = if kind == 1 { "seed" } else { "output" };
            host(what)?;
            require_pane_id(&label, what)?;
            if data.len() < 8 {
                return Err(format!("{what} frame omitted terminal generation"));
            }
            let pane_id = label;
            let generation = be_u64(&data[..8]);
            let data = data[8..].to_vec();
            Ok(TerminalEvent::Pane(if kind == 1 {
                PaneEvent::Seed {
                    pane_id,
                    generation,
                    data,
                    sequence,
                }
            } else {
                PaneEvent::Output {
                    pane_id,
                    generation,
                    data,
                    sequence,
                }
            }))
        }
        3 => {
            host("topology dirty")?;
            if !data.is_empty() {
                return Err("topology dirty frame has an unexpected payload".into());
            }
            Ok(TerminalEvent::TopologyDirty {
                grids: decode_layout_grids(&label)?,
                sequence,
            })
        }
        7 => decode_snapshot(&label, sequence, data),
        9 => decode_pane_resource(label, sequence, data),
        10 => {
            if sequence != 0 {
                return Err("terminal generation epoch frame must use local sequence zero".into());
            }
            if label != "terminal" {
                return Err("terminal generation epoch has an invalid label".into());
            }
            if data.len() != 8 {
                return Err("terminal generation epoch must be an 8-byte u64".into());
            }
            match be_u64(data) {
                0 => Err("terminal generation epoch must be nonzero".into()),
                epoch => Ok(TerminalEvent::GenerationEpoch { epoch, sequence }),
            }
        }
        11 => {
            host("terminal seed diagnostic")?;
            require_pane_id(&label, "terminal seed diagnostic")?;
            Ok(TerminalEvent::Pane(PaneEvent::SeedDiagnostic {
                pane_id: label,
                message: text(data, "terminal seed diagnostic payload")?,
                sequence,
            }))
        }
        15 => {
            host("terminal flow stall")?;
            require_pane_id(&label, "terminal flow stall")?;
            text(data, "terminal flow stall payload")?;
            Ok(TerminalEvent::FlowStalled {
                pane_id: label,
                sequence,
            })
        }
        18 => {
            host("terminal history")?;
            require_pane_id(&label, "terminal history")?;
            if data.len() < TERMINAL_HISTORY_HEADER_BYTES {
                return Err("terminal history frame is truncated".into());
            }
            let known = data[0] == 1;
            let size = u32::from_be_bytes([data[1], data[2], data[3], data[4]]);
            Ok(TerminalEvent::Pane(PaneEvent::History {
                pane_id: label,
                data: data[TERMINAL_HISTORY_HEADER_BYTES..].to_vec(),
                history_size: known.then_some(size),
                sequence,
            }))
        }
        8 => {
            host("protocol progress")?;
            if label != "protocol" || !data.is_empty() {
                return Err("protocol progress frame is malformed".into());
            }
            Ok(TerminalEvent::Other { sequence })
        }
        4..=6 | 12..=14 | 16 | 17 | 19 => Ok(TerminalEvent::Other { sequence }),
        kind => Err(format!("unknown terminal frame kind {kind}")),
    }
}

fn decode_layout_grids(label: &str) -> Result<Option<Vec<PaneGrid>>, String> {
    if !label.starts_with("layout-change") {
        return Ok(None);
    }
    let Some(payload) = label.strip_prefix("layout-change ") else {
        return Err("layout notification omitted pane grids".into());
    };
    let entries: Vec<(String, usize, usize)> =
        serde_json::from_str(payload).map_err(|_| "invalid pane grid".to_owned())?;
    let mut grids: Vec<PaneGrid> = Vec::with_capacity(entries.len());
    for (pane_id, columns, rows) in entries {
        let in_range = (1..=65_535).contains(&columns) && (1..=65_535).contains(&rows);
        if !is_pane_id(&pane_id) || !in_range || grids.iter().any(|g| g.pane_id == pane_id) {
            return Err("invalid pane grid".into());
        }
        grids.push(PaneGrid {
            pane_id,
            size: GridSize { columns, rows },
        });
    }
    Ok(Some(grids))
}

#[derive(Deserialize)]
struct SnapshotPayload {
    #[serde(default)]
    snapshot: Option<SnapshotTree>,
    sequence: u64,
    authoritative: bool,
}

#[derive(Deserialize)]
struct SnapshotTree {
    #[serde(default)]
    panes: Vec<SnapshotPane>,
}

#[derive(Deserialize)]
struct SnapshotPane {
    id: String,
    width: usize,
    height: usize,
}

fn decode_snapshot(label: &str, sequence: u64, data: &[u8]) -> Result<TerminalEvent, String> {
    if label != "snapshot" {
        return Err("topology snapshot has an invalid label".into());
    }
    let parsed: SnapshotPayload = serde_json::from_slice(data)
        .map_err(|_| "topology snapshot payload is not valid JSON".to_owned())?;
    if parsed.sequence != sequence {
        return Err("snapshot sequence conflicts with its common frame header".into());
    }
    if !parsed.authoritative && sequence == 0 {
        return Err("ordered topology snapshot frame must use a nonzero host sequence".into());
    }
    Ok(TerminalEvent::Snapshot {
        grids: parsed.snapshot.map(|tree| {
            tree.panes
                .into_iter()
                .map(|pane| PaneGrid {
                    pane_id: pane.id,
                    size: GridSize {
                        columns: pane.width,
                        rows: pane.height,
                    },
                })
                .collect()
        }),
        authoritative: parsed.authoritative,
        sequence,
    })
}

fn decode_pane_resource(
    pane_id: String,
    sequence: u64,
    payload: &[u8],
) -> Result<TerminalEvent, String> {
    if sequence == 0 {
        return Err("pane resource frame must use a nonzero host sequence".into());
    }
    require_pane_id(&pane_id, "pane resource")?;
    if payload.len() < PANE_RESOURCE_HEADER_BYTES {
        return Err("pane resource payload is truncated".into());
    }
    let state = match payload[0] {
        0 => PaneResourceState::Unspecified,
        1 => PaneResourceState::Visible,
        2 => PaneResourceState::HiddenBuffered,
        3 => PaneResourceState::Released,
        _ => return Err("invalid pane resource state".into()),
    };
    let flags = payload[1];
    if flags & !3 != 0 {
        return Err("pane resource payload has unknown flags".into());
    }
    let generation = be_u64(&payload[2..10]);
    let snapshot_generation = be_u64(&payload[10..18]);
    let tail_through_generation = be_u64(&payload[18..26]);
    if snapshot_generation > tail_through_generation || tail_through_generation > generation {
        return Err("pane resource generation metadata is inconsistent".into());
    }
    let reason_len =
        u32::from_be_bytes([payload[26], payload[27], payload[28], payload[29]]) as usize;
    let tail_len =
        u32::from_be_bytes([payload[30], payload[31], payload[32], payload[33]]) as usize;
    if PANE_RESOURCE_HEADER_BYTES + reason_len + tail_len != payload.len() {
        return Err("pane resource length fields do not match its payload".into());
    }
    let reason_end = PANE_RESOURCE_HEADER_BYTES + reason_len;
    Ok(TerminalEvent::Pane(PaneEvent::Resource(PaneResource {
        pane_id,
        state,
        requires_seed: flags & 1 != 0,
        resume_from_renderer: flags & 2 != 0,
        recovery_reason: text(
            &payload[PANE_RESOURCE_HEADER_BYTES..reason_end],
            "pane resource recovery reason",
        )?,
        generation,
        snapshot_generation,
        tail_through_generation,
        raw_tail: payload[reason_end..].to_vec(),
        sequence,
    })))
}

fn be_u64(bytes: &[u8]) -> u64 {
    let mut word = [0; 8];
    word.copy_from_slice(bytes);
    u64::from_be_bytes(word)
}

fn text(data: &[u8], what: &str) -> Result<String, String> {
    String::from_utf8(data.to_vec()).map_err(|_| format!("{what} is not valid UTF-8"))
}

fn is_pane_id(value: &str) -> bool {
    value
        .strip_prefix('%')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn require_pane_id(value: &str, what: &str) -> Result<(), String> {
    if is_pane_id(value) {
        Ok(())
    } else {
        Err(format!("{what} has an invalid pane label"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(kind: u8, label: &str, sequence: u64, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![kind];
        bytes.extend_from_slice(&(label.len() as u16).to_be_bytes());
        bytes.extend_from_slice(label.as_bytes());
        bytes.extend_from_slice(&sequence.to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn decodes_output_with_its_generation() {
        let mut payload = 7_u64.to_be_bytes().to_vec();
        payload.extend_from_slice(&[0, 255, 27]);
        assert_eq!(
            decode_frame(&frame(2, "%3", 11, &payload)).unwrap(),
            TerminalEvent::Pane(PaneEvent::Output {
                pane_id: "%3".into(),
                generation: 7,
                data: vec![0, 255, 27],
                sequence: 11,
            })
        );
    }

    #[test]
    fn decodes_a_layout_notification_into_ordered_grids() {
        let event = decode_frame(&frame(3, r#"layout-change [["%1",40,12]]"#, 2, &[])).unwrap();
        assert_eq!(
            event,
            TerminalEvent::TopologyDirty {
                grids: Some(vec![PaneGrid {
                    pane_id: "%1".into(),
                    size: GridSize {
                        columns: 40,
                        rows: 12
                    },
                }]),
                sequence: 2,
            }
        );
        assert_eq!(
            decode_frame(&frame(3, "window-add", 3, &[])).unwrap(),
            TerminalEvent::TopologyDirty {
                grids: None,
                sequence: 3
            }
        );
    }

    #[test]
    fn refuses_a_layout_notification_with_no_grids_or_a_bad_one() {
        assert!(decode_frame(&frame(3, "layout-change", 2, &[])).is_err());
        assert!(decode_frame(&frame(3, r#"layout-change [["%1",0,12]]"#, 2, &[])).is_err());
        assert!(decode_frame(&frame(3, r#"layout-change [["x",4,12]]"#, 2, &[])).is_err());
    }

    #[test]
    fn decodes_a_pane_resource_and_its_flags() {
        let mut payload = vec![1, 2];
        payload.extend_from_slice(&9_u64.to_be_bytes());
        payload.extend_from_slice(&4_u64.to_be_bytes());
        payload.extend_from_slice(&7_u64.to_be_bytes());
        payload.extend_from_slice(&3_u32.to_be_bytes());
        payload.extend_from_slice(&2_u32.to_be_bytes());
        payload.extend_from_slice(b"why");
        payload.extend_from_slice(&[0xaa, 0xbb]);
        assert_eq!(
            decode_frame(&frame(9, "%1", 5, &payload)).unwrap(),
            TerminalEvent::Pane(PaneEvent::Resource(PaneResource {
                pane_id: "%1".into(),
                state: PaneResourceState::Visible,
                requires_seed: false,
                resume_from_renderer: true,
                recovery_reason: "why".into(),
                generation: 9,
                snapshot_generation: 4,
                tail_through_generation: 7,
                raw_tail: vec![0xaa, 0xbb],
                sequence: 5,
            }))
        );
    }

    #[test]
    fn refuses_inconsistent_pane_resource_generations() {
        let mut payload = vec![1, 0];
        payload.extend_from_slice(&5_u64.to_be_bytes());
        payload.extend_from_slice(&6_u64.to_be_bytes());
        payload.extend_from_slice(&5_u64.to_be_bytes());
        payload.extend_from_slice(&[0; 8]);
        assert!(decode_frame(&frame(9, "%1", 5, &payload)).is_err());
    }

    #[test]
    fn reads_history_size_presence_from_its_own_byte() {
        let answered = [&[1_u8, 0, 0, 7, 208][..], b"rows"].concat();
        assert_eq!(
            decode_frame(&frame(18, "%1", 2, &answered)).unwrap(),
            TerminalEvent::Pane(PaneEvent::History {
                pane_id: "%1".into(),
                data: b"rows".to_vec(),
                history_size: Some(2000),
                sequence: 2,
            })
        );
        let unanswered = [&[0_u8, 0, 0, 0, 0][..], b"rows"].concat();
        assert!(matches!(
            decode_frame(&frame(18, "%1", 2, &unanswered)).unwrap(),
            TerminalEvent::Pane(PaneEvent::History {
                history_size: None,
                ..
            })
        ));
    }

    #[test]
    fn enforces_local_and_host_sequence_rules() {
        assert!(decode_frame(&frame(2, "%1", 0, &[0; 8])).is_err());
        assert!(decode_frame(&frame(10, "terminal", 1, &1_u64.to_be_bytes())).is_err());
        assert!(decode_frame(&frame(10, "terminal", 0, &0_u64.to_be_bytes())).is_err());
        assert_eq!(
            decode_frame(&frame(10, "terminal", 0, &7_u64.to_be_bytes())).unwrap(),
            TerminalEvent::GenerationEpoch {
                epoch: 7,
                sequence: 0
            }
        );
    }

    #[test]
    fn keeps_only_the_sequence_of_frames_the_hub_does_not_read() {
        assert_eq!(
            decode_frame(&frame(14, "agent:1", 9, b"{}")).unwrap(),
            TerminalEvent::Other { sequence: 9 }
        );
    }

    #[test]
    fn rejects_unknown_kinds_and_truncation() {
        assert!(decode_frame(&frame(99, "x", 1, &[])).is_err());
        assert!(decode_frame(&[1, 0]).is_err());
        assert!(decode_frame(&[1, 0, 9, b'x']).is_err());
    }

    fn resource_payload(state: u8, flags: u8, reason: &[u8], tail: &[u8]) -> Vec<u8> {
        let mut payload = vec![state, flags];
        for generation in [9_u64, 4, 7] {
            payload.extend_from_slice(&generation.to_be_bytes());
        }
        payload.extend_from_slice(&(reason.len() as u32).to_be_bytes());
        payload.extend_from_slice(&(tail.len() as u32).to_be_bytes());
        payload.extend_from_slice(reason);
        payload.extend_from_slice(tail);
        payload
    }

    #[test]
    fn refuses_a_malformed_pane_resource() {
        let decode = |payload: &[u8]| decode_frame(&frame(9, "%1", 5, payload));
        assert!(decode(&resource_payload(1, 0, b"", b"")).is_ok());
        let full = resource_payload(1, 0, b"why", b"ab");
        assert!(decode(&full[..33]).is_err(), "truncated header");
        assert!(
            decode(&resource_payload(1, 4, b"", b"")).is_err(),
            "unknown flags"
        );
        assert!(
            decode(&resource_payload(4, 0, b"", b"")).is_err(),
            "unknown state"
        );
        assert!(
            decode(&resource_payload(1, 0, &[0xff], b"")).is_err(),
            "reason not UTF-8"
        );
        assert!(decode(&full[..full.len() - 1]).is_err(), "length fields");
        assert!(
            decode(&[full.as_slice(), b"x"].concat()).is_err(),
            "length fields"
        );
        assert!(
            decode_frame(&frame(9, "pane", 5, &full)).is_err(),
            "pane label"
        );
        assert!(
            decode_frame(&frame(9, "%1", 0, &full)).is_err(),
            "host sequence"
        );
    }

    #[test]
    fn decodes_a_seed_diagnostic_and_refuses_a_bad_one() {
        assert_eq!(
            decode_frame(&frame(11, "%2", 4, b"no mouse state")).unwrap(),
            TerminalEvent::Pane(PaneEvent::SeedDiagnostic {
                pane_id: "%2".into(),
                message: "no mouse state".into(),
                sequence: 4,
            })
        );
        assert!(decode_frame(&frame(11, "two", 4, b"x")).is_err());
        assert!(decode_frame(&frame(11, "%2", 4, &[0xff])).is_err());
        assert!(decode_frame(&frame(11, "%2", 0, b"x")).is_err());
    }

    #[test]
    fn a_history_of_zero_lines_is_known_and_a_bad_history_frame_is_refused() {
        assert!(matches!(
            decode_frame(&frame(18, "%1", 2, &[1, 0, 0, 0, 0])).unwrap(),
            TerminalEvent::Pane(PaneEvent::History {
                history_size: Some(0),
                ..
            })
        ));
        assert!(decode_frame(&frame(18, "%1", 2, &[1, 0, 0, 0])).is_err());
        assert!(decode_frame(&frame(18, "%1", 0, &[0; 5])).is_err());
        assert!(decode_frame(&frame(18, "x", 2, &[0; 5])).is_err());
    }

    #[test]
    fn refuses_pane_bytes_without_their_generation() {
        assert!(decode_frame(&frame(1, "%1", 3, &[0; 7])).is_err());
        assert!(decode_frame(&frame(2, "%1", 3, &[0; 7])).is_err());
        assert!(decode_frame(&frame(1, "%1", 3, &[0; 8])).is_ok());
    }

    #[test]
    fn protocol_progress_is_an_empty_frame_with_its_own_label() {
        assert_eq!(
            decode_frame(&frame(8, "protocol", 6, &[])).unwrap(),
            TerminalEvent::Other { sequence: 6 }
        );
        assert!(decode_frame(&frame(8, "protocol", 6, b"x")).is_err());
        assert!(decode_frame(&frame(8, "terminal", 6, &[])).is_err());
        assert!(decode_frame(&frame(8, "protocol", 0, &[])).is_err());
    }

    #[test]
    fn carries_snapshot_grids_only_when_it_has_a_tree() {
        let tree = br#"{"sequence":5,"authoritative":true,"snapshot":{"panes":[{"id":"%1","width":80,"height":24}]}}"#;
        assert_eq!(
            decode_frame(&frame(7, "snapshot", 5, tree)).unwrap(),
            TerminalEvent::Snapshot {
                grids: Some(vec![PaneGrid {
                    pane_id: "%1".into(),
                    size: GridSize {
                        columns: 80,
                        rows: 24
                    },
                }]),
                authoritative: true,
                sequence: 5,
            }
        );
        assert_eq!(
            decode_frame(&frame(
                7,
                "snapshot",
                5,
                br#"{"sequence":5,"authoritative":false}"#
            ))
            .unwrap(),
            TerminalEvent::Snapshot {
                grids: None,
                authoritative: false,
                sequence: 5,
            }
        );
    }
}
