use std::collections::VecDeque;

use thiserror::Error;

const DEFAULT_MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommandTag {
    pub timestamp: u64,
    pub number: u64,
    pub flags: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlRecord {
    Output { pane_id: String, data: Vec<u8> },
    Begin { tag: CommandTag, arguments: String },
    End { tag: CommandTag, arguments: String },
    Error { tag: CommandTag, arguments: String },
    Exit { reason: String },
    Notification { name: String, arguments: String },
    CommandOutput(Vec<u8>),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ControlParseError {
    #[error("tmux control line exceeded {limit} bytes")]
    LineTooLarge { limit: usize },
    #[error("invalid %{record} record")]
    InvalidRecord { record: &'static str },
    #[error("malformed tmux octal escape at byte {offset}")]
    MalformedEscape { offset: usize },
    #[error("tmux control stream ended with {bytes} bytes of an incomplete record")]
    TruncatedLine { bytes: usize },
}

/// Incremental, byte-preserving tmux control-mode parser.
///
/// Once a line exceeds the configured bound, its entire remainder is discarded
/// through the next newline. This prevents a suffix of an oversized record from
/// being mistaken for a fresh command or terminal output record.
#[derive(Debug)]
pub struct ControlParser {
    buffered: Vec<u8>,
    ready: VecDeque<Result<ControlRecord, ControlParseError>>,
    max_line_bytes: usize,
    discarding_oversized_line: bool,
    /// The tag of the `%begin` that is currently open, if any. Holding the tag
    /// rather than a flag is what lets the parser tell tmux's own `%end`/
    /// `%error` from a captured screen that happens to contain one.
    open_command_block: Option<CommandTag>,
}

impl Default for ControlParser {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_LINE_BYTES)
    }
}

impl ControlParser {
    pub fn new(max_line_bytes: usize) -> Self {
        assert!(max_line_bytes > 0, "control line limit must be non-zero");
        Self {
            buffered: Vec::new(),
            ready: VecDeque::new(),
            max_line_bytes,
            discarding_oversized_line: false,
            open_command_block: None,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.discarding_oversized_line {
                if byte == b'\n' {
                    self.discarding_oversized_line = false;
                }
                continue;
            }
            if byte == b'\n' {
                if self.buffered.last() == Some(&b'\r') {
                    self.buffered.pop();
                }
                let line = std::mem::take(&mut self.buffered);
                let record = parse_line(&line, self.open_command_block);
                match &record {
                    Ok(ControlRecord::Begin { tag, .. }) => self.open_command_block = Some(*tag),
                    Ok(ControlRecord::End { .. } | ControlRecord::Error { .. }) => {
                        self.open_command_block = None;
                    }
                    _ => {}
                }
                self.ready.push_back(record);
            } else if self.buffered.len() == self.max_line_bytes {
                self.buffered.clear();
                self.discarding_oversized_line = true;
                self.ready.push_back(Err(ControlParseError::LineTooLarge {
                    limit: self.max_line_bytes,
                }));
            } else {
                self.buffered.push(byte);
            }
        }
    }

    /// Marks the byte stream as closed and reports a partial final record.
    pub fn finish(&mut self) {
        if self.discarding_oversized_line {
            self.discarding_oversized_line = false;
            return;
        }
        if !self.buffered.is_empty() {
            let bytes = self.buffered.len();
            self.buffered.clear();
            self.ready
                .push_back(Err(ControlParseError::TruncatedLine { bytes }));
        }
    }

    pub fn next_record(&mut self) -> Option<Result<ControlRecord, ControlParseError>> {
        self.ready.pop_front()
    }
}

/// tmux's own asynchronous notification names.
///
/// Inside a command block, a line beginning with `%` is ambiguous: it is either
/// a notification tmux interleaved into the block (the man page says this cannot
/// happen; it does) or a line of the command's own output. `capture-pane` of a
/// pane showing `%50 complete` or a zsh prompt is the common case, and treating
/// that content as protocol both loses the line and — for `%begin`-shaped
/// content — reports a parse failure that resnapshots the whole connection.
/// Restricting in-block notifications to names tmux actually emits keeps the
/// real ones and returns everything else as output. The residual ambiguity
/// (pane content byte-identical to a real notification line) is unavoidable:
/// the protocol does not escape command output.
fn known_notification_name(name: &str) -> bool {
    matches!(
        name,
        "exit"
            | "output"
            | "extended-output"
            | "client-detached"
            | "client-session-changed"
            | "config-error"
            | "continue"
            | "layout-change"
            | "message"
            | "pane-mode-changed"
            | "pause"
            | "paste-buffer-changed"
            | "paste-buffer-deleted"
            | "session-changed"
            | "session-renamed"
            | "session-window-changed"
            | "sessions-changed"
            | "subscription-changed"
            | "unlinked-window-add"
            | "unlinked-window-close"
            | "unlinked-window-renamed"
            | "window-add"
            | "window-close"
            | "window-pane-changed"
            | "window-renamed"
    )
}

fn parse_line(
    line: &[u8],
    open_command_block: Option<CommandTag>,
) -> Result<ControlRecord, ControlParseError> {
    if !line.starts_with(b"%") {
        return Ok(ControlRecord::CommandOutput(line.to_vec()));
    }

    let separator = line.iter().position(|byte| *byte == b' ');
    let (name, arguments) = match separator {
        Some(index) => (&line[1..index], &line[index + 1..]),
        None => (&line[1..], &[][..]),
    };
    let name = String::from_utf8_lossy(name).into_owned();
    let output = || Ok(ControlRecord::CommandOutput(line.to_vec()));
    if let Some(open) = open_command_block {
        match name.as_str() {
            // tmux does not nest command blocks, so a `%begin` inside one is
            // always the captured screen. Taking it as protocol would abandon
            // the capture in progress and resnapshot the connection.
            "begin" => return output(),
            // A block ends on its own tag and no other. Screen content that
            // happens to read `%end 1 2 1` closes nothing.
            "end" | "error" => {
                if parse_command_tag(arguments, "end") != Ok(open) {
                    return output();
                }
            }
            other if !known_notification_name(other) => return output(),
            _ => {}
        }
    }

    let record = match name.as_str() {
        "output" => parse_output(arguments),
        "extended-output" => parse_extended_output(arguments),
        "begin" => parse_command_tag(arguments, "begin").map(|tag| ControlRecord::Begin {
            tag,
            arguments: String::from_utf8_lossy(arguments).into_owned(),
        }),
        "end" => parse_command_tag(arguments, "end").map(|tag| ControlRecord::End {
            tag,
            arguments: String::from_utf8_lossy(arguments).into_owned(),
        }),
        "error" => parse_command_tag(arguments, "error").map(|tag| ControlRecord::Error {
            tag,
            arguments: String::from_utf8_lossy(arguments).into_owned(),
        }),
        "exit" => Ok(ControlRecord::Exit {
            reason: String::from_utf8_lossy(arguments).into_owned(),
        }),
        _ => Ok(ControlRecord::Notification {
            name,
            arguments: String::from_utf8_lossy(arguments).into_owned(),
        }),
    };
    // A record header that does not parse inside a block is command output that
    // happens to look like protocol — `%output not-a-pane` in a captured
    // screen, say. Reporting it as a parse failure costs a connection-wide
    // resnapshot for what is only text. An escape that does not decode is left
    // as a failure: its header did parse, so it is far more likely to be a real
    // desync.
    match record {
        Err(ControlParseError::InvalidRecord { .. }) if open_command_block.is_some() => output(),
        record => record,
    }
}

fn parse_command_tag(
    arguments: &[u8],
    record: &'static str,
) -> Result<CommandTag, ControlParseError> {
    let mut fields = arguments.split(|byte| *byte == b' ');
    let parse = |value: Option<&[u8]>| std::str::from_utf8(value?).ok()?.parse::<u64>().ok();
    Ok(CommandTag {
        timestamp: parse(fields.next()).ok_or(ControlParseError::InvalidRecord { record })?,
        number: parse(fields.next()).ok_or(ControlParseError::InvalidRecord { record })?,
        flags: parse(fields.next()).ok_or(ControlParseError::InvalidRecord { record })?,
    })
}

fn parse_output(arguments: &[u8]) -> Result<ControlRecord, ControlParseError> {
    let separator = arguments
        .iter()
        .position(|byte| *byte == b' ')
        .ok_or(ControlParseError::InvalidRecord { record: "output" })?;
    output_record(
        &arguments[..separator],
        &arguments[separator.saturating_add(1)..],
        "output",
    )
}

fn parse_extended_output(arguments: &[u8]) -> Result<ControlRecord, ControlParseError> {
    let pane_end = arguments.iter().position(|byte| *byte == b' ').ok_or(
        ControlParseError::InvalidRecord {
            record: "extended-output",
        },
    )?;
    let value_start = arguments
        .windows(3)
        .position(|window| window == b" : ")
        .map(|position| position + 3)
        .ok_or(ControlParseError::InvalidRecord {
            record: "extended-output",
        })?;
    output_record(
        &arguments[..pane_end],
        &arguments[value_start..],
        "extended-output",
    )
}

fn output_record(
    pane: &[u8],
    escaped: &[u8],
    record: &'static str,
) -> Result<ControlRecord, ControlParseError> {
    let pane_id = String::from_utf8(pane.to_vec())
        .map_err(|_| ControlParseError::InvalidRecord { record })?;
    if !valid_id(&pane_id, '%') {
        return Err(ControlParseError::InvalidRecord { record });
    }
    Ok(ControlRecord::Output {
        pane_id,
        data: unescape_output(escaped)?,
    })
}

fn valid_id(value: &str, prefix: char) -> bool {
    value.strip_prefix(prefix).is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
}

pub fn unescape_output(input: &[u8]) -> Result<Vec<u8>, ControlParseError> {
    let mut output = Vec::with_capacity(input.len());
    let mut index = 0;
    while index < input.len() {
        if input[index] != b'\\' {
            output.push(input[index]);
            index += 1;
            continue;
        }
        if index + 3 >= input.len()
            || !input[index + 1..=index + 3]
                .iter()
                .all(|byte| (b'0'..=b'7').contains(byte))
        {
            return Err(ControlParseError::MalformedEscape { offset: index });
        }
        let value = u16::from(input[index + 1] - b'0') * 64
            + u16::from(input[index + 2] - b'0') * 8
            + u16::from(input[index + 3] - b'0');
        let value = u8::try_from(value)
            .map_err(|_| ControlParseError::MalformedEscape { offset: index })?;
        output.push(value);
        index += 4;
    }
    Ok(output)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchEvent {
    Output {
        pane_id: String,
        data: Vec<u8>,
    },
    Notification {
        name: String,
        arguments: String,
    },
    CommandCompleted {
        tag: CommandTag,
        output: Vec<Vec<u8>>,
        error: Option<String>,
    },
    Exit {
        reason: String,
    },
    ResnapshotRequired {
        reason: String,
    },
}

#[derive(Debug)]
struct ActiveCommand {
    tag: CommandTag,
    output: Vec<Vec<u8>>,
}

/// Correlates command blocks while allowing asynchronous pane output and
/// notifications to pass through immediately.
#[derive(Debug, Default)]
pub struct ControlDispatcher {
    active: Option<ActiveCommand>,
}

impl ControlDispatcher {
    pub fn handle(
        &mut self,
        record: Result<ControlRecord, ControlParseError>,
    ) -> Vec<DispatchEvent> {
        let mut events = Vec::new();
        match record {
            Err(error) => events.push(DispatchEvent::ResnapshotRequired {
                reason: error.to_string(),
            }),
            Ok(ControlRecord::Output { pane_id, data }) => {
                events.push(DispatchEvent::Output { pane_id, data })
            }
            Ok(ControlRecord::Notification { name, arguments }) => {
                events.push(DispatchEvent::Notification { name, arguments })
            }
            Ok(ControlRecord::Exit { reason }) => events.push(DispatchEvent::Exit { reason }),
            Ok(ControlRecord::Begin { tag, .. }) => {
                if let Some(abandoned) = self.active.replace(ActiveCommand {
                    tag,
                    output: Vec::new(),
                }) {
                    events.push(DispatchEvent::ResnapshotRequired {
                        reason: format!(
                            "command {} was interrupted by command {}",
                            abandoned.tag.number, tag.number
                        ),
                    });
                }
            }
            Ok(ControlRecord::CommandOutput(line)) => {
                if let Some(active) = &mut self.active {
                    active.output.push(line);
                } else {
                    events.push(DispatchEvent::ResnapshotRequired {
                        reason: "command output arrived outside a command block".into(),
                    });
                }
            }
            Ok(ControlRecord::End { tag, .. }) => {
                self.finish(tag, None, &mut events);
            }
            Ok(ControlRecord::Error { tag, arguments }) => {
                self.finish(tag, Some(arguments), &mut events);
            }
        }
        events
    }

    fn finish(&mut self, tag: CommandTag, error: Option<String>, events: &mut Vec<DispatchEvent>) {
        match self.active.take() {
            Some(active) if active.tag == tag => events.push(DispatchEvent::CommandCompleted {
                tag,
                output: active.output,
                error,
            }),
            Some(active) => events.push(DispatchEvent::ResnapshotRequired {
                reason: format!(
                    "command block ended with tag {} while {} was active",
                    tag.number, active.tag.number
                ),
            }),
            None => events.push(DispatchEvent::ResnapshotRequired {
                reason: format!("command block {} ended without a begin", tag.number),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_partial_records_and_unescapes_output_bytes() {
        let mut parser = ControlParser::default();
        parser.push(b"%output %7 hello\\033[31");
        assert!(parser.next_record().is_none());
        parser.push(b"m\\377\n%layout-change @2 deadbeef\n");
        assert_eq!(
            parser.next_record().unwrap().unwrap(),
            ControlRecord::Output {
                pane_id: "%7".into(),
                data: b"hello\x1b[31m\xff".to_vec(),
            }
        );
        assert!(matches!(
            parser.next_record().unwrap().unwrap(),
            ControlRecord::Notification { name, .. } if name == "layout-change"
        ));
    }

    #[test]
    fn parses_extended_output_without_treating_age_as_terminal_data() {
        let mut parser = ControlParser::default();
        parser.push(b"%extended-output %2 153 : a\\033b\n");
        assert_eq!(
            parser.next_record().unwrap().unwrap(),
            ControlRecord::Output {
                pane_id: "%2".into(),
                data: b"a\x1bb".to_vec(),
            }
        );
    }

    /// tmux interleaves notifications into command blocks (the man page says it
    /// cannot; a raw `-C` probe says otherwise), and it does not escape command
    /// output, so a captured screen can contain lines that look exactly like
    /// records. Both halves have to work: the notification is recognised, and
    /// pane content is returned as output instead of being lost — or, for
    /// `%begin`-shaped content, reported as a parse failure that resnapshots
    /// every pane on the connection.
    #[test]
    fn in_block_lines_are_records_only_for_names_tmux_actually_emits() {
        let mut parser = ControlParser::default();
        parser.push(b"%begin 1 2 1\n");
        parser.push(b"%pause %3\n");
        parser.push(b"%50 percent line\n");
        parser.push(b"%begin fake\n");
        // A captured screen showing a well-formed `%begin`, and one showing an
        // `%end` for a different command, are the dangerous shapes: taken as
        // protocol the first abandons the capture in progress and the second
        // closes the block early, both of which resnapshot the connection.
        parser.push(b"%begin 9 9 1\n");
        parser.push(b"%end 9 9 1\n");
        parser.push(b"%output %3 live\n");
        parser.push(b"plain capture line\n");
        parser.push(b"%end 1 2 1\n");
        let records: Vec<_> = std::iter::from_fn(|| parser.next_record())
            .map(|record| record.expect("no in-block line may be a parse failure"))
            .collect();
        assert_eq!(
            records,
            vec![
                ControlRecord::Begin {
                    tag: CommandTag {
                        timestamp: 1,
                        number: 2,
                        flags: 1
                    },
                    arguments: "1 2 1".into()
                },
                ControlRecord::Notification {
                    name: "pause".into(),
                    arguments: "%3".into()
                },
                ControlRecord::CommandOutput(b"%50 percent line".to_vec()),
                ControlRecord::CommandOutput(b"%begin fake".to_vec()),
                ControlRecord::CommandOutput(b"%begin 9 9 1".to_vec()),
                ControlRecord::CommandOutput(b"%end 9 9 1".to_vec()),
                ControlRecord::Output {
                    pane_id: "%3".into(),
                    data: b"live".to_vec()
                },
                ControlRecord::CommandOutput(b"plain capture line".to_vec()),
                ControlRecord::End {
                    tag: CommandTag {
                        timestamp: 1,
                        number: 2,
                        flags: 1
                    },
                    arguments: "1 2 1".into()
                },
            ]
        );
        // Outside a block the same line can only have come from tmux, so a
        // malformed record there is still a real desync.
        parser.push(b"%begin oops\n");
        assert_eq!(
            parser.next_record().unwrap().unwrap_err(),
            ControlParseError::InvalidRecord { record: "begin" }
        );
    }

    #[test]
    fn malformed_escape_and_disconnect_have_deterministic_errors() {
        let mut parser = ControlParser::default();
        parser.push(b"%output %1 bad\\x\npartial");
        parser.finish();
        assert_eq!(
            parser.next_record().unwrap().unwrap_err(),
            ControlParseError::MalformedEscape { offset: 3 }
        );
        assert_eq!(
            parser.next_record().unwrap().unwrap_err(),
            ControlParseError::TruncatedLine { bytes: 7 }
        );
    }

    #[test]
    fn rejects_out_of_byte_range_octal_without_overflow() {
        for value in 0o400..=0o777 {
            let encoded = format!("\\{value:03o}");
            assert_eq!(
                unescape_output(encoded.as_bytes()),
                Err(ControlParseError::MalformedEscape { offset: 0 })
            );
        }
        assert_eq!(unescape_output(b"\\377").unwrap(), vec![0xff]);
    }

    #[test]
    fn oversized_line_is_discarded_through_its_newline() {
        let mut parser = ControlParser::new(12);
        parser.push(b"1234567890123suffix\n%exit clean\n");
        assert_eq!(
            parser.next_record().unwrap().unwrap_err(),
            ControlParseError::LineTooLarge { limit: 12 }
        );
        assert_eq!(
            parser.next_record().unwrap().unwrap(),
            ControlRecord::Exit {
                reason: "clean".into()
            }
        );
    }

    #[test]
    fn dispatcher_passes_async_records_during_correlated_command() {
        let tag = CommandTag {
            timestamp: 10,
            number: 4,
            flags: 1,
        };
        let mut dispatcher = ControlDispatcher::default();
        assert!(
            dispatcher
                .handle(Ok(ControlRecord::Begin {
                    tag,
                    arguments: "10 4 1".into()
                }))
                .is_empty()
        );
        assert!(matches!(
            dispatcher.handle(Ok(ControlRecord::Output {
                pane_id: "%1".into(),
                data: vec![0xff]
            }))[0],
            DispatchEvent::Output { .. }
        ));
        assert!(matches!(
            dispatcher.handle(Ok(ControlRecord::Notification {
                name: "layout-change".into(),
                arguments: "@1 deadbeef".into()
            }))[0],
            DispatchEvent::Notification { .. }
        ));
        dispatcher.handle(Ok(ControlRecord::CommandOutput(b"answer".to_vec())));
        assert!(matches!(
            dispatcher.handle(Ok(ControlRecord::End {
                tag,
                arguments: "10 4 1".into()
            }))[0],
            DispatchEvent::CommandCompleted { ref output, .. } if output == &[b"answer".to_vec()]
        ));
    }

    #[test]
    fn recorded_interleaving_fixture_preserves_output_and_command_correlation() {
        let fixture = include_bytes!("../../../tests/fixtures/tmux-control/interleaved.control");
        let mut parser = ControlParser::default();
        let mut dispatcher = ControlDispatcher::default();
        let mut events = Vec::new();
        for chunk in fixture.chunks(7) {
            parser.push(chunk);
            while let Some(record) = parser.next_record() {
                events.extend(dispatcher.handle(record));
            }
        }
        parser.finish();
        while let Some(record) = parser.next_record() {
            events.extend(dispatcher.handle(record));
        }
        assert!(events.iter().any(|event| matches!(
            event,
            DispatchEvent::Output { data, .. } if data == b"live\x1b[31moutput"
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, DispatchEvent::CommandCompleted { .. }))
                .count(),
            2
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, DispatchEvent::ResnapshotRequired { .. }))
        );
    }

    #[test]
    fn malformed_fixture_has_one_recovery_event_per_bad_record() {
        let fixture = include_bytes!("../../../tests/fixtures/tmux-control/malformed.control");
        let mut parser = ControlParser::default();
        let mut dispatcher = ControlDispatcher::default();
        parser.push(fixture);
        parser.finish();
        let mut recoveries = 0;
        while let Some(record) = parser.next_record() {
            recoveries += dispatcher
                .handle(record)
                .into_iter()
                .filter(|event| matches!(event, DispatchEvent::ResnapshotRequired { .. }))
                .count();
        }
        assert_eq!(recoveries, 4);
    }

    #[test]
    fn tmux_33_and_37_capture_corpus_survives_every_split_boundary() {
        for fixture in [
            include_bytes!("../../../tests/fixtures/tmux-control/tmux-3.3a-capture.control")
                .as_slice(),
            include_bytes!("../../../tests/fixtures/tmux-control/tmux-3.7-capture.control")
                .as_slice(),
        ] {
            for split in 0..=fixture.len() {
                let mut parser = ControlParser::default();
                parser.push(&fixture[..split]);
                parser.push(&fixture[split..]);
                parser.finish();
                let records = std::iter::from_fn(|| parser.next_record()).collect::<Vec<_>>();
                assert!(records.iter().all(Result::is_ok), "failed at split {split}");
                assert!(
                    records.iter().any(|record| matches!(
                        record,
                        Ok(ControlRecord::CommandOutput(line))
                            if line.starts_with(b"__ADE_META__")
                    )),
                    "metadata missing at split {split}"
                );
            }
        }
    }
}
