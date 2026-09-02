//! Versioned, bounded binary messages shared by desktop and host processes.

// Prost generates the wire-compatible oneof as an enum. Boxing a variant
// would leak a generator-specific ownership change through every protocol
// consumer without changing the bounded frame contract.
#![allow(clippy::large_enum_variant)]

use std::io::{Read, Write};

use prost::Message;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/tmux_agent.protocol.v1.rs"));

    impl AgentHookWiring {
        /// The name this state travels under across the WebView boundary.
        ///
        /// Here, beside the enum, because it was three copies: one in the
        /// helper's CLI, a byte-identical one in the desktop's Tauri bridge,
        /// and a third as a TypeScript union. Adding a state meant editing all
        /// three, and missing one degraded silently to "unspecified" with no
        /// compile error in either language.
        pub fn label(self) -> &'static str {
            match self {
                Self::Wired => "wired",
                Self::Partial => "partial",
                Self::NotWired => "notWired",
                Self::Absent => "absent",
                Self::Unavailable => "unavailable",
                Self::Unspecified => "unspecified",
            }
        }

        /// Whether this state is one an install would act on.
        ///
        /// The policy, once. It was written twice — the helper's CLI decided
        /// which adapters to install into, and the desktop decided which to
        /// offer — in two languages, and the two were already diverging.
        /// `Absent` is not here: an agent that is not on the host has nothing
        /// to wire, and installing there writes configuration for a tool the
        /// user does not use. Neither is `Unavailable`: writing over what
        /// nobody could parse is how unrelated hooks get lost.
        pub fn invites_setup(self) -> bool {
            matches!(self, Self::NotWired | Self::Partial)
        }
    }
}

// Phase 9 removes obsolete topology and agent-route fields. The major bump
// deliberately makes older helpers read-only until the user accepts the
// existing explicit helper-upgrade flow.
pub const PROTOCOL_MAJOR: u32 = 2;
pub const PROTOCOL_MINOR: u32 = 0;
pub const HELPER_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Outcome of a descriptor-anchored filesystem publication. This Rust-level
/// contract is shared by the desktop downloader and host uploader so callers
/// never infer whether rename happened from an error string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationOutcome {
    Published,
    NotPublished,
    Unknown,
}

#[derive(Debug)]
pub struct Published<T> {
    pub value: T,
    pub cleanup_error: Option<String>,
}

#[derive(Debug)]
pub struct PublishFailure {
    pub outcome: PublicationOutcome,
    pub message: String,
}

impl std::fmt::Display for PublishFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let prefix = match self.outcome {
            PublicationOutcome::Published => "published",
            PublicationOutcome::NotPublished => "not_published",
            PublicationOutcome::Unknown => "outcome_unknown",
        };
        write!(formatter, "{prefix}: {}", self.message)
    }
}

impl std::error::Error for PublishFailure {}

pub type PublishResult<T> = Result<Published<T>, PublishFailure>;

pub const CAP_SNAPSHOTS: u64 = 1 << 0;
pub const CAP_ORDERED_EVENTS: u64 = 1 << 1;
pub const CAP_CANCELLATION: u64 = 1 << 2;
pub const CAP_TERMINAL_STREAM: u64 = 1 << 3;
pub const CAP_RESYNC: u64 = 1 << 4;
pub const CAP_TMUX_ACTIONS: u64 = 1 << 5;
pub const CAP_TERMINAL_RESOURCES: u64 = 1 << 6;
pub const CAP_ACTIVE_ROOT: u64 = 1 << 7;
pub const CAP_FILE_SERVICE: u64 = 1 << 8;
pub const CAP_TEXT_EDITOR: u64 = 1 << 9;
pub const CAP_BULK_DOWNLOAD: u64 = 1 << 10;
pub const CAP_GIT: u64 = 1 << 11;
pub const CAP_AGENTS: u64 = 1 << 12;
pub const CAP_TERMINAL_UPLOAD: u64 = 1 << 13;
pub const CAP_TERMINAL_OUTPUT_CREDIT: u64 = 1 << 14;
/// One descriptor-bound editor open per request, replacing the metadata +
/// preflight + per-chunk staircase.
///
/// A *required* capability, not an optional one: it is part of
/// [`HOST_CAPABILITIES`], which the desktop demands in full, so a helper
/// without it is refused at the handshake with the missing bit named. There is
/// deliberately no fallback to the staircase — the daemon already has to match
/// the desktop's helper version, and a second code path for opening files that
/// nobody exercises is how the one people do use goes quietly wrong.
///
/// The bit exists so that refusal says *what* is missing. Without it, a helper
/// that passed version checks but predated this operation would connect
/// cleanly and then fail every file open with an unknown-operation error.
pub const CAP_FILE_STREAM: u64 = 1 << 15;
/// Resolves explicit terminal-output paths against the authoritative pane cwd.
/// Required so a desktop cannot offer operation 46 to a helper that predates it.
pub const CAP_TERMINAL_FILE_RESOLUTION: u64 = 1 << 16;
/// The helper resolves tmux independently of an interactive shell `PATH`.
///
/// Required even though it adds no request type: a daemon survives desktop
/// upgrades, and a same-version daemon predating this behavior would otherwise
/// remain "compatible" while every local tmux operation still failed from a
/// Dock-launched app. The required bit makes the bridge retire that daemon and
/// start the helper shipped with the desktop.
pub const CAP_TMUX_EXECUTABLE_RESOLUTION: u64 = 1 << 17;
/// The helper serves the five `OPERATION_VOICE_*` operations and pushes
/// `VOICE_REPLY` events (docs/mobile/voice-mode-plan.md).
///
/// Required, like every other bit: the phone demands the full set, so a
/// helper without it is refused at the handshake with "voice" named rather
/// than failing the first utterance with an unknown-operation error. Whether
/// voice is *usable* on the host (uv installed, model provisioned) is a
/// runtime answer from `OPERATION_VOICE_STATUS`, not a capability.
pub const CAP_VOICE: u64 = 1 << 18;
pub const HOST_CAPABILITIES: u64 = CAP_SNAPSHOTS
    | CAP_ORDERED_EVENTS
    | CAP_CANCELLATION
    | CAP_TERMINAL_STREAM
    | CAP_RESYNC
    | CAP_TMUX_ACTIONS
    | CAP_TERMINAL_RESOURCES
    | CAP_ACTIVE_ROOT
    | CAP_FILE_SERVICE
    | CAP_TEXT_EDITOR
    | CAP_BULK_DOWNLOAD
    | CAP_GIT
    | CAP_AGENTS
    | CAP_TERMINAL_UPLOAD
    | CAP_TERMINAL_OUTPUT_CREDIT
    | CAP_FILE_STREAM
    | CAP_TERMINAL_FILE_RESOLUTION
    | CAP_TMUX_EXECUTABLE_RESOLUTION
    | CAP_VOICE;

/// Every required capability, with the name a refusal reports it by.
const CAPABILITY_NAMES: [(u64, &str); 19] = [
    (CAP_SNAPSHOTS, "snapshots"),
    (CAP_ORDERED_EVENTS, "orderedEvents"),
    (CAP_CANCELLATION, "cancellation"),
    (CAP_TERMINAL_STREAM, "terminalStream"),
    (CAP_RESYNC, "resync"),
    (CAP_TMUX_ACTIONS, "tmuxActions"),
    (CAP_TERMINAL_RESOURCES, "terminalResources"),
    (CAP_ACTIVE_ROOT, "activeRoot"),
    (CAP_FILE_SERVICE, "fileService"),
    (CAP_TEXT_EDITOR, "textEditor"),
    (CAP_BULK_DOWNLOAD, "bulkDownload"),
    (CAP_GIT, "git"),
    (CAP_AGENTS, "agents"),
    (CAP_TERMINAL_UPLOAD, "terminalUpload"),
    (CAP_TERMINAL_OUTPUT_CREDIT, "terminalOutputCredit"),
    (CAP_FILE_STREAM, "fileStream"),
    (CAP_TERMINAL_FILE_RESOLUTION, "terminalFileResolution"),
    (CAP_TMUX_EXECUTABLE_RESOLUTION, "tmuxExecutableResolution"),
    (CAP_VOICE, "voice"),
];

/// Which required capabilities `advertised` does not carry.
///
/// The admission rule itself, in one place. Restating it — even in a comment
/// beside a test — is how a helper that cannot serve an operation ends up
/// admitted by one copy of the rule and refused by another.
pub fn missing_host_capabilities(advertised: u64) -> u64 {
    HOST_CAPABILITIES & !advertised
}

/// Names the capabilities in `mask`, so a refusal can say what is missing.
///
/// A bare hex mask names the *bit*, which is not a fact anyone outside this
/// file can act on.
pub fn capability_names(mask: u64) -> Vec<&'static str> {
    let mut named: Vec<&'static str> = CAPABILITY_NAMES
        .iter()
        .filter(|(bit, _)| mask & bit != 0)
        .map(|(_, name)| *name)
        .collect();
    if mask & !CAPABILITY_NAMES.iter().fold(0, |all, (bit, _)| all | bit) != 0 {
        named.push("unknown");
    }
    named
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostContractError {
    ProtocolMajor { advertised: u32, required: u32 },
    ReadOnly(String),
    MissingCapabilities(u64),
}

impl std::fmt::Display for HostContractError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ProtocolMajor {
                advertised,
                required,
            } => {
                write!(
                    formatter,
                    "host protocol major {advertised} does not match required {required}"
                )
            }
            Self::ReadOnly(reason) if reason.is_empty() => {
                formatter.write_str("host bridge is read-only")
            }
            Self::ReadOnly(reason) => write!(formatter, "host bridge is read-only: {reason}"),
            Self::MissingCapabilities(mask) => write!(
                formatter,
                "host helper is missing required capabilities: {}",
                capability_names(*mask).join(", "),
            ),
        }
    }
}

impl std::error::Error for HostContractError {}

/** The common writable-host admission contract for control and bulk lanes. */
pub fn validate_host_contract(
    envelope_major: u32,
    hello: &v1::ServerHello,
) -> Result<(), HostContractError> {
    if envelope_major != PROTOCOL_MAJOR {
        return Err(HostContractError::ProtocolMajor {
            advertised: envelope_major,
            required: PROTOCOL_MAJOR,
        });
    }
    if hello.read_only {
        return Err(HostContractError::ReadOnly(hello.incompatibility.clone()));
    }
    let missing = missing_host_capabilities(hello.capabilities);
    if missing != 0 {
        return Err(HostContractError::MissingCapabilities(missing));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("frame I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame length {0} exceeds the {MAX_FRAME_BYTES}-byte limit")]
    TooLarge(usize),
    #[error("invalid protobuf frame: {0}")]
    Decode(#[from] prost::DecodeError),
}

pub fn encode_frame(envelope: &v1::Envelope) -> Result<Vec<u8>, FrameError> {
    let body = envelope.encode_to_vec();
    if body.len() > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(body.len()));
    }
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

pub fn read_frame_sync(reader: &mut impl Read) -> Result<Option<v1::Envelope>, FrameError> {
    let mut length = [0_u8; 4];
    let first = reader.read(&mut length[..1])?;
    if first == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut length[1..])?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(length));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    Ok(Some(v1::Envelope::decode(body.as_slice())?))
}

/// Incremental decoder used by cancellable synchronous IPC clients.
///
/// The bound applies to each advertised frame body. A read may legitimately
/// contain a complete maximum-sized frame followed by part or all of later
/// frames, so the aggregate buffered byte count is not itself a frame length.
#[derive(Default)]
pub struct FrameAccumulator {
    bytes: Vec<u8>,
}

impl FrameAccumulator {
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), FrameError> {
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    pub fn next_frame(&mut self) -> Result<Option<v1::Envelope>, FrameError> {
        if self.bytes.len() < 4 {
            return Ok(None);
        }
        let length =
            u32::from_be_bytes(self.bytes[..4].try_into().expect("four-byte prefix")) as usize;
        if length > MAX_FRAME_BYTES {
            return Err(FrameError::TooLarge(length));
        }
        if self.bytes.len() < length + 4 {
            return Ok(None);
        }
        let envelope = v1::Envelope::decode(&self.bytes[4..length + 4])?;
        self.bytes.drain(..length + 4);
        Ok(Some(envelope))
    }
}

pub fn write_frame_sync(
    writer: &mut impl Write,
    envelope: &v1::Envelope,
) -> Result<(), FrameError> {
    writer.write_all(&encode_frame(envelope)?)?;
    writer.flush()?;
    Ok(())
}

pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<v1::Envelope>, FrameError> {
    let mut length = [0_u8; 4];
    let first = reader.read(&mut length[..1]).await?;
    if first == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut length[1..]).await?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge(length));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    Ok(Some(v1::Envelope::decode(body.as_slice())?))
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    envelope: &v1::Envelope,
) -> Result<(), FrameError> {
    writer.write_all(&encode_frame(envelope)?).await?;
    writer.flush().await?;
    Ok(())
}

pub fn envelope(request_id: u64, sequence: u64, payload: v1::envelope::Payload) -> v1::Envelope {
    v1::Envelope {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        request_id,
        sequence,
        stream_id: 0,
        priority: v1::Priority::Control.into(),
        payload: Some(payload),
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;

    #[test]
    fn transactional_publish_failures_have_typed_outcomes() {
        let not_published = PublishFailure {
            outcome: PublicationOutcome::NotPublished,
            message: "rolled back".into(),
        };
        assert_eq!(not_published.to_string(), "not_published: rolled back");
        let unknown = PublishFailure {
            outcome: PublicationOutcome::Unknown,
            message: "replacement preserved".into(),
        };
        assert_eq!(
            unknown.to_string(),
            "outcome_unknown: replacement preserved"
        );
    }

    #[test]
    fn incremental_decoder_accepts_fragmented_frames_and_rejects_oversize() {
        let expected = envelope(
            77,
            0,
            v1::envelope::Payload::Cancel(v1::Cancel {
                target_request_id: 42,
            }),
        );
        let encoded = encode_frame(&expected).unwrap();
        let mut decoder = FrameAccumulator::default();
        for byte in &encoded[..encoded.len() - 1] {
            decoder.push(std::slice::from_ref(byte)).unwrap();
            assert!(decoder.next_frame().unwrap().is_none());
        }
        decoder.push(&encoded[encoded.len() - 1..]).unwrap();
        let decoded = decoder.next_frame().unwrap().unwrap();
        assert_eq!(decoded.request_id, 77);
        assert!(matches!(
            decoded.payload,
            Some(v1::envelope::Payload::Cancel(_))
        ));

        let mut oversized = FrameAccumulator::default();
        let length = (MAX_FRAME_BYTES as u32 + 1).to_be_bytes();
        oversized.push(&length).unwrap();
        assert!(matches!(
            oversized.next_frame(),
            Err(FrameError::TooLarge(_))
        ));
    }

    #[test]
    fn incremental_decoder_accepts_a_maximum_frame_followed_by_more_frames() {
        let mut maximum = envelope(
            1,
            0,
            v1::envelope::Payload::Request(v1::Request {
                operation: v1::Operation::TerminalInput.into(),
                data: vec![0; MAX_FRAME_BYTES - 64],
                ..Default::default()
            }),
        );
        let current = maximum.encoded_len();
        let v1::envelope::Payload::Request(request) = maximum.payload.as_mut().unwrap() else {
            unreachable!()
        };
        request
            .data
            .resize(request.data.len() + (MAX_FRAME_BYTES - current), 0);
        assert_eq!(maximum.encoded_len(), MAX_FRAME_BYTES);

        let second = envelope(
            2,
            0,
            v1::envelope::Payload::Cancel(v1::Cancel {
                target_request_id: 1,
            }),
        );
        let third = envelope(
            3,
            0,
            v1::envelope::Payload::Cancel(v1::Cancel {
                target_request_id: 2,
            }),
        );
        let second_frame = encode_frame(&second).unwrap();
        let split = 1;
        let mut first_read = encode_frame(&maximum).unwrap();
        first_read.extend_from_slice(&second_frame[..split]);

        let mut decoder = FrameAccumulator::default();
        decoder.push(&first_read).unwrap();
        assert_eq!(decoder.next_frame().unwrap().unwrap().request_id, 1);
        assert!(decoder.next_frame().unwrap().is_none());

        let mut next_read = second_frame[split..].to_vec();
        next_read.extend_from_slice(&encode_frame(&third).unwrap());
        decoder.push(&next_read).unwrap();
        assert_eq!(decoder.next_frame().unwrap().unwrap().request_id, 2);
        assert_eq!(decoder.next_frame().unwrap().unwrap().request_id, 3);
        assert!(decoder.next_frame().unwrap().is_none());
    }
}
