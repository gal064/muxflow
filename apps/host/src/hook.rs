use std::{
    fs::{self, OpenOptions},
    io::{BufReader, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, bail};
use prost::Message;
use serde::{
    Deserialize, Deserializer,
    de::{IgnoredAny, MapAccess, SeqAccess, Visitor},
};
use tmux_agent_protocol::{envelope, read_frame, v1, write_frame};
use tokio::{
    net::UnixStream,
    time::{Duration, timeout},
};

pub(crate) mod codex_transcript;

/// Vendor hook input can contain the complete tool result. In particular,
/// Claude's `PostToolUse(Read)` embeds image data in `tool_response`, so the
/// raw envelope can legitimately be much larger than the compact event sent
/// to the daemon. The parser below streams past fields Muxflow does not use;
/// this bound limits CPU/input consumption rather than retained payload size.
const MAX_VENDOR_HOOK_BYTES: usize = 64 * 1024 * 1024;
const MAX_LIFECYCLE_FIELD_BYTES: usize = 16 * 1024;
const MAX_NORMALIZED_HOOK_BYTES: usize = 256 * 1024;
/// How much of `last_assistant_message` a `Stop` forwards for voice mode.
/// Cut rather than rejected: a long reply must never cost the `Stop` itself.
const MAX_ASSISTANT_MESSAGE_BYTES: usize = 32 * 1024;

#[derive(Debug)]
struct VendorHookPayload {
    session_id: Option<String>,
    agent_id: Option<String>,
    event_id: Option<String>,
    hook_event_name: Option<String>,
    notification_type: Option<String>,
    tool_name: Option<String>,
    turn_id: Option<String>,
    transcript_path: Option<String>,
    has_running_subagent: bool,
    last_assistant_message: Option<TruncatedString>,
}

#[derive(Clone, Copy)]
enum HookField {
    SessionId,
    SessionIdCamel,
    AgentId,
    AgentIdCamel,
    EventId,
    EventIdCamel,
    HookEventId,
    HookEventName,
    HookEventNameCamel,
    Event,
    NotificationType,
    NotificationTypeCamel,
    ToolName,
    ToolNameCamel,
    TurnId,
    TurnIdCamel,
    TranscriptPath,
    TranscriptPathCamel,
    BackgroundTasks,
    LastAssistantMessage,
    LastAssistantMessageCamel,
    Unknown,
}

impl<'de> Deserialize<'de> for HookField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_identifier(HookFieldVisitor)
    }
}

struct HookFieldVisitor;

impl Visitor<'_> for HookFieldVisitor {
    type Value = HookField;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a hook payload field")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(match value {
            "session_id" => HookField::SessionId,
            "sessionId" => HookField::SessionIdCamel,
            "agent_id" => HookField::AgentId,
            "agentId" => HookField::AgentIdCamel,
            "event_id" => HookField::EventId,
            "eventId" => HookField::EventIdCamel,
            "hook_event_id" => HookField::HookEventId,
            "hook_event_name" => HookField::HookEventName,
            "hookEventName" => HookField::HookEventNameCamel,
            "event" => HookField::Event,
            "notification_type" => HookField::NotificationType,
            "notificationType" => HookField::NotificationTypeCamel,
            "tool_name" => HookField::ToolName,
            "toolName" => HookField::ToolNameCamel,
            "turn_id" => HookField::TurnId,
            "turnId" => HookField::TurnIdCamel,
            "transcript_path" => HookField::TranscriptPath,
            "transcriptPath" => HookField::TranscriptPathCamel,
            "background_tasks" => HookField::BackgroundTasks,
            "last_assistant_message" => HookField::LastAssistantMessage,
            "lastAssistantMessage" => HookField::LastAssistantMessageCamel,
            _ => HookField::Unknown,
        })
    }
}

struct BoundedString(Option<String>);

impl<'de> Deserialize<'de> for BoundedString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(BoundedStringVisitor)
    }
}

struct BoundedStringVisitor;

impl<'de> Visitor<'de> for BoundedStringVisitor {
    type Value = BoundedString;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a bounded lifecycle string or an ignored malformed value")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.len() > MAX_LIFECYCLE_FIELD_BYTES {
            return Err(E::custom(format_args!(
                "hook lifecycle field exceeds the {MAX_LIFECYCLE_FIELD_BYTES}-byte limit"
            )));
        }
        Ok(BoundedString(Some(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.len() > MAX_LIFECYCLE_FIELD_BYTES {
            return Err(E::custom(format_args!(
                "hook lifecycle field exceeds the {MAX_LIFECYCLE_FIELD_BYTES}-byte limit"
            )));
        }
        Ok(BoundedString(Some(value)))
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(BoundedString(None))
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(BoundedString(None))
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(BoundedString(None))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(BoundedString(None))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(BoundedString(None))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(BoundedString(None))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(BoundedString(None))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(BoundedString(None))
    }
}

/// A string kept up to [`MAX_ASSISTANT_MESSAGE_BYTES`], cut on a character
/// boundary with the cut recorded, instead of failing the whole hook the way
/// [`BoundedString`] does: the reply is a payload, not an identifier, and its
/// length says nothing about the event's validity.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TruncatedString {
    text: String,
    truncated: bool,
}

impl TruncatedString {
    fn cut(value: &str) -> Self {
        if value.len() <= MAX_ASSISTANT_MESSAGE_BYTES {
            return Self {
                text: value.to_owned(),
                truncated: false,
            };
        }
        let mut end = MAX_ASSISTANT_MESSAGE_BYTES;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        Self {
            text: value[..end].to_owned(),
            truncated: true,
        }
    }
}

struct OptionalTruncatedString(Option<TruncatedString>);

impl<'de> Deserialize<'de> for OptionalTruncatedString {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(TruncatedStringVisitor)
    }
}

struct TruncatedStringVisitor;

impl<'de> Visitor<'de> for TruncatedStringVisitor {
    type Value = OptionalTruncatedString;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("a message string, truncated past its bound, or an ignored malformed value")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(OptionalTruncatedString(Some(TruncatedString::cut(value))))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(OptionalTruncatedString(Some(TruncatedString::cut(&value))))
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(OptionalTruncatedString(None))
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(OptionalTruncatedString(None))
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(OptionalTruncatedString(None))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(OptionalTruncatedString(None))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(OptionalTruncatedString(None))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(OptionalTruncatedString(None))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(OptionalTruncatedString(None))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(OptionalTruncatedString(None))
    }
}

impl<'de> Deserialize<'de> for VendorHookPayload {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(VendorHookPayloadVisitor)
    }
}

struct VendorHookPayloadVisitor;

impl<'de> Visitor<'de> for VendorHookPayloadVisitor {
    type Value = VendorHookPayload;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a hook payload JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut session_id = None;
        let mut session_id_camel = None;
        let mut agent_id = None;
        let mut agent_id_camel = None;
        let mut event_id = None;
        let mut event_id_camel = None;
        let mut hook_event_id = None;
        let mut hook_event_name = None;
        let mut hook_event_name_camel = None;
        let mut event = None;
        let mut notification_type = None;
        let mut notification_type_camel = None;
        let mut tool_name = None;
        let mut tool_name_camel = None;
        let mut turn_id = None;
        let mut turn_id_camel = None;
        let mut transcript_path = None;
        let mut transcript_path_camel = None;
        let mut has_running_subagent = false;
        let mut last_assistant_message = None;
        let mut last_assistant_message_camel = None;

        while let Some(field) = map.next_key::<HookField>()? {
            match field {
                HookField::SessionId => session_id = map.next_value::<BoundedString>()?.0,
                HookField::SessionIdCamel => {
                    session_id_camel = map.next_value::<BoundedString>()?.0
                }
                HookField::AgentId => agent_id = map.next_value::<BoundedString>()?.0,
                HookField::AgentIdCamel => agent_id_camel = map.next_value::<BoundedString>()?.0,
                HookField::EventId => event_id = map.next_value::<BoundedString>()?.0,
                HookField::EventIdCamel => event_id_camel = map.next_value::<BoundedString>()?.0,
                HookField::HookEventId => hook_event_id = map.next_value::<BoundedString>()?.0,
                HookField::HookEventName => hook_event_name = map.next_value::<BoundedString>()?.0,
                HookField::HookEventNameCamel => {
                    hook_event_name_camel = map.next_value::<BoundedString>()?.0
                }
                HookField::Event => event = map.next_value::<BoundedString>()?.0,
                HookField::NotificationType => {
                    notification_type = map.next_value::<BoundedString>()?.0
                }
                HookField::NotificationTypeCamel => {
                    notification_type_camel = map.next_value::<BoundedString>()?.0
                }
                HookField::ToolName => tool_name = map.next_value::<BoundedString>()?.0,
                HookField::ToolNameCamel => tool_name_camel = map.next_value::<BoundedString>()?.0,
                HookField::TurnId => turn_id = map.next_value::<BoundedString>()?.0,
                HookField::TurnIdCamel => turn_id_camel = map.next_value::<BoundedString>()?.0,
                HookField::TranscriptPath => transcript_path = map.next_value::<BoundedString>()?.0,
                HookField::TranscriptPathCamel => {
                    transcript_path_camel = map.next_value::<BoundedString>()?.0
                }
                HookField::BackgroundTasks => {
                    has_running_subagent = map.next_value::<RunningSubagent>()?.0
                }
                HookField::LastAssistantMessage => {
                    last_assistant_message = map.next_value::<OptionalTruncatedString>()?.0
                }
                HookField::LastAssistantMessageCamel => {
                    last_assistant_message_camel = map.next_value::<OptionalTruncatedString>()?.0
                }
                HookField::Unknown => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }

        Ok(VendorHookPayload {
            session_id: session_id.or(session_id_camel),
            agent_id: agent_id.or(agent_id_camel),
            event_id: event_id.or(event_id_camel).or(hook_event_id),
            hook_event_name: hook_event_name.or(hook_event_name_camel).or(event),
            notification_type: notification_type.or(notification_type_camel),
            tool_name: tool_name.or(tool_name_camel),
            turn_id: turn_id.or(turn_id_camel),
            transcript_path: transcript_path.or(transcript_path_camel),
            has_running_subagent,
            last_assistant_message: last_assistant_message.or(last_assistant_message_camel),
        })
    }
}

struct RunningSubagent(bool);

impl<'de> Deserialize<'de> for RunningSubagent {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(RunningSubagentVisitor)
    }
}

struct RunningSubagentVisitor;

impl<'de> Visitor<'de> for RunningSubagentVisitor {
    type Value = RunningSubagent;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a background task list or an ignored malformed value")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut running = false;
        while let Some(task) = sequence.next_element::<BackgroundTaskMatch>()? {
            running |= task.0;
        }
        Ok(RunningSubagent(running))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(RunningSubagent(false))
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(RunningSubagent(false))
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(RunningSubagent(false))
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(RunningSubagent(false))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(RunningSubagent(false))
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(RunningSubagent(false))
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(RunningSubagent(false))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(RunningSubagent(false))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(RunningSubagent(false))
    }
}

#[derive(Clone, Copy)]
enum TaskField {
    Type,
    Status,
    Unknown,
}

impl<'de> Deserialize<'de> for TaskField {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_identifier(TaskFieldVisitor)
    }
}

struct TaskFieldVisitor;

impl Visitor<'_> for TaskFieldVisitor {
    type Value = TaskField;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a background task field")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(match value {
            "type" => TaskField::Type,
            "status" => TaskField::Status,
            _ => TaskField::Unknown,
        })
    }
}

struct BackgroundTaskMatch(bool);

impl<'de> Deserialize<'de> for BackgroundTaskMatch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(BackgroundTaskMatchVisitor)
    }
}

struct BackgroundTaskMatchVisitor;

impl<'de> Visitor<'de> for BackgroundTaskMatchVisitor {
    type Value = BackgroundTaskMatch;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a background task object or an ignored malformed value")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut task_type = TaskToken::Other;
        let mut status = TaskToken::Other;
        while let Some(field) = map.next_key::<TaskField>()? {
            match field {
                TaskField::Type => task_type = map.next_value()?,
                TaskField::Status => status = map.next_value()?,
                TaskField::Unknown => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(BackgroundTaskMatch(
            task_type == TaskToken::Subagent && status == TaskToken::Running,
        ))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(BackgroundTaskMatch(false))
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(BackgroundTaskMatch(false))
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(BackgroundTaskMatch(false))
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(BackgroundTaskMatch(false))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(BackgroundTaskMatch(false))
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(BackgroundTaskMatch(false))
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(BackgroundTaskMatch(false))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(BackgroundTaskMatch(false))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(BackgroundTaskMatch(false))
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskToken {
    Subagent,
    Running,
    Other,
}

impl<'de> Deserialize<'de> for TaskToken {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(TaskTokenVisitor)
    }
}

struct TaskTokenVisitor;

impl<'de> Visitor<'de> for TaskTokenVisitor {
    type Value = TaskToken;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a background task discriminator")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(match value {
            "subagent" => TaskToken::Subagent,
            "running" => TaskToken::Running,
            _ => TaskToken::Other,
        })
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_str(&value)
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(TaskToken::Other)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(TaskToken::Other)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(TaskToken::Other)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(TaskToken::Other)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(TaskToken::Other)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(TaskToken::Other)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<IgnoredAny>()?.is_some() {}
        Ok(TaskToken::Other)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(TaskToken::Other)
    }
}

pub(crate) async fn run(arguments: Vec<String>) -> anyhow::Result<()> {
    let adapter = parse_adapter(&arguments)?;
    // Parse before writing a fallback mailbox. Only the allowlisted lifecycle
    // fields are retained; raw vendor data never traverses a desktop/public
    // listener or reaches durable storage.
    let payload = read_vendor_hook(std::io::stdin().lock())?;
    let Some(pane_id) = pane_for_hook(std::env::var_os("TMUX_PANE"))? else {
        return Ok(());
    };
    let origin_server_identity =
        crate::service::snapshot::inherited_server_identity().unwrap_or_default();
    let now = now_millis();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let event = build_event_from_payload(
        adapter,
        &payload,
        &pane_id,
        &origin_server_identity,
        now,
        home.as_deref(),
    )?;
    deliver(
        &crate::paths::runtime_dir_candidates(),
        &event,
        home.as_deref(),
    )
    .await
}

fn read_vendor_hook(reader: impl Read) -> anyhow::Result<VendorHookPayload> {
    read_vendor_hook_bounded(reader, MAX_VENDOR_HOOK_BYTES)
}

fn read_vendor_hook_bounded(
    reader: impl Read,
    max_bytes: usize,
) -> anyhow::Result<VendorHookPayload> {
    let mut limited = reader.take((max_bytes + 1) as u64);
    let parsed = serde_json::from_reader(BufReader::new(&mut limited));
    if limited.limit() == 0 {
        bail!("hook payload exceeds the {max_bytes}-byte limit");
    }
    parsed.context("parse hook JSON")
}

/// Hand the event to whichever daemon is actually running.
///
/// The candidate list exists because "the runtime directory" is not a property
/// of the machine but of the environment a process happened to inherit, and a
/// hook inherits a different one from the daemon (see `paths::record_runtime_dir`).
/// Only a connection error moves on to the next candidate: a daemon that
/// answered and then rejected the event is the daemon, and retrying the same
/// event against another directory would either duplicate it or hide the
/// rejection.
async fn deliver(
    candidates: &[std::path::PathBuf],
    event: &v1::AgentHookEvent,
    home: Option<&Path>,
) -> anyhow::Result<()> {
    for runtime in candidates {
        let socket = runtime.join("host.sock");
        if !socket.exists() {
            continue;
        }
        match send(&socket, event).await {
            Ok(v1::HookIngestDisposition::Applied | v1::HookIngestDisposition::Discarded) => {
                return Ok(());
            }
            Ok(v1::HookIngestDisposition::Retryable | v1::HookIngestDisposition::Unspecified) => {
                // The daemon answered, so do not probe another candidate and
                // risk delivering twice. One rejection produces one durable
                // mailbox entry for the daemon to replay later.
                return persist_latest_fallback(runtime, &event_for_fallback(event, home)?);
            }
            Err(HookDeliveryFailure::PreDelivery(error)) => {
                drop(error);
                continue;
            }
            Err(HookDeliveryFailure::Ambiguous(error)) => {
                // The request may already have committed. Never probe another
                // daemon after that boundary; persist beside the one that may
                // have applied it and let source-ID dedupe reconcile replay.
                drop(error);
                return persist_latest_fallback(runtime, &event_for_fallback(event, home)?);
            }
        }
    }
    persist_latest_fallback(
        &crate::paths::fallback_runtime_dir(candidates),
        &event_for_fallback(event, home)?,
    )
}

#[derive(Debug)]
enum HookDeliveryFailure {
    /// No ingest request bytes were attempted; another runtime candidate is
    /// still safe to try.
    PreDelivery(anyhow::Error),
    /// Request delivery began, so absence of an acknowledgement is not proof
    /// that the daemon did not apply it.
    Ambiguous(anyhow::Error),
}

/// `hook status|install|uninstall` — the same merge-only installer the desktop
/// drives, reachable without a UI.
///
/// This exists so the installer can be exercised against a fixture copied from
/// a real machine's configuration: `--home` relocates every adapter's file and
/// `--settings-path` relocates one adapter's, which is what keeps QA off a real
/// `~/.claude` while still testing the real merge.
pub(crate) fn manage(verb: &str, arguments: Vec<String>) -> anyhow::Result<()> {
    let options = Options::parse(&arguments);
    let home = options.home.clone().map(std::path::PathBuf::from);
    let settings_path = options.settings_path.clone().map(std::path::PathBuf::from);
    let adapter_id = options.adapter.clone();
    let selected = adapter_id
        .as_deref()
        .map(|id| crate::service::agents::adapters::by_id(id).context("unsupported hook adapter"))
        .transpose()?;
    let config_override = match (settings_path, selected) {
        (Some(path), Some(adapter)) => Some((adapter.id(), path)),
        (Some(_), None) => {
            bail!("--settings-path names one adapter's configuration and requires --adapter")
        }
        (None, _) => None,
    };
    let manager = crate::service::agents::HookManager::with_overrides(home, config_override)?;
    let action = match verb {
        "status" => None,
        "install" => Some(v1::HookManagementAction::Install),
        "uninstall" => Some(v1::HookManagementAction::Uninstall),
        _ => bail!("usage: muxflow-host hook <ingest|status|install|uninstall>"),
    };
    // Consent, for the one caller that has no user interface to ask through.
    //
    // The desktop will not write a host's agent configuration without a
    // recorded answer for that host; this command had no notion of consent at
    // all, so anything that could run it — a script, an agent, a paste from a
    // README — rewrote the operator's real `~/.claude` and `~/.codex` silently.
    // `--yes` is what makes the answer explicit and, in a shell history, a
    // record.
    //
    // The question is which files this run would write, never which flags it
    // carries: `--home "$HOME"` and `--settings-path ~/.claude/settings.json`
    // are both redirections and both land on exactly the files an unredirected
    // run would. So the paths are compared against the ones this operator's own
    // environment resolves, which is also what leaves every fixture lane — all
    // of which redirect somewhere else — needing no answer.
    if action.is_some() && !options.confirmed {
        // `?`, never a default. A gate that permits when it cannot work out
        // what it is protecting is not a gate.
        let mine = crate::service::agents::HookManager::with_overrides(None, None)
            .context("resolve your own agent configuration to confirm this would not change it")?
            .wiring()
            .into_iter()
            .map(|(_, entry)| entry.config_path)
            .collect::<std::collections::HashSet<_>>();
        if let Some((_, entry)) = manager
            .wiring()
            .into_iter()
            .find(|(_, entry)| mine.contains(&entry.config_path))
        {
            bail!(
                "hook {verb} would change your own agent configuration at {}. \
                 Re-run with --yes to confirm, or point --home/--settings-path at a copy.",
                entry.config_path.display()
            );
        }
    }
    let mut report = Vec::new();
    let mut failed = false;
    if let Some(action) = action {
        for (adapter, entry) in manager.wiring() {
            // The two verbs ask opposite questions and had been sharing one
            // predicate. Install asks "would this act here" — `invites_setup`,
            // which excludes an agent that is not on the host, because
            // installing there creates a configuration directory and file for
            // a tool the user does not use. Uninstall asks "do we own anything
            // here", and a `Wired` adapter answers no to the first and yes to
            // the second: unscoped `hook uninstall` removed nothing at all and
            // reported that the agents were not installed.
            //
            // `--adapter` overrides absence, since an operator naming an
            // adapter has said something the probe cannot. Nothing overrides an
            // unreadable configuration: writing over what nobody could parse is
            // how unrelated hooks get lost.
            let explicit = adapter_id.as_deref() == Some(entry.adapter_id);
            let acts_here = match action {
                v1::HookManagementAction::Uninstall => {
                    entry.state == v1::AgentHookWiring::Wired
                        || entry.state == v1::AgentHookWiring::Partial
                }
                _ => entry.state.invites_setup(),
            };
            let skip = if adapter_id.is_some() && !explicit {
                Some("not selected")
            } else if entry.state == v1::AgentHookWiring::Unavailable {
                Some("configuration could not be read")
            } else if !acts_here && !explicit {
                Some(match action {
                    v1::HookManagementAction::Uninstall => "nothing of ours is installed here",
                    _ => "agent is not installed here",
                })
            } else {
                None
            };
            if let Some(reason) = skip {
                report.push(serde_json::json!({
                    "adapterId": entry.adapter_id,
                    "configPath": entry.config_path,
                    "skipped": reason,
                }));
                continue;
            }
            // Every adapter is reported even when an earlier one failed: a
            // merge-only installer whose pitch is "you can see exactly what
            // changed" must not exit silently having already written a file.
            match apply_one(&manager, adapter.legacy_kind(), action) {
                Ok(value) => report.push(value),
                Err(error) => {
                    failed = true;
                    report.push(serde_json::json!({
                        "adapterId": entry.adapter_id,
                        "configPath": entry.config_path,
                        "error": error.to_string(),
                    }));
                }
            }
        }
    }
    // Re-read: what the wiring is *after* whatever just happened is the
    // useful answer, and the observation above was only a plan.
    let wiring: Vec<_> = manager
        .wiring()
        .into_iter()
        .map(|(_, entry)| entry)
        .filter(|entry| {
            adapter_id
                .as_deref()
                .is_none_or(|id| id == entry.adapter_id)
        })
        .map(|entry| {
            serde_json::json!({
                "adapterId": entry.adapter_id,
                "configPath": entry.config_path,
                "wiring": entry.state.label(),
                "detail": entry.detail,
            })
        })
        .collect();
    println!(
        "{}",
        serde_json::json!({
            "action": verb,
            "applied": report,
            "adapters": wiring,
        })
    );
    if failed {
        bail!("one or more adapters could not be updated; see the reported result");
    }
    Ok(())
}

fn apply_one(
    manager: &crate::service::agents::HookManager,
    adapter: v1::AgentAdapterKind,
    action: v1::HookManagementAction,
) -> anyhow::Result<serde_json::Value> {
    let review = manager.review(adapter, action)?;
    // `already_current` is the installer's own idempotence answer, so a second
    // run reports "unchanged" rather than rewriting a file and claiming it did
    // something.
    let changed = !review.already_current;
    if changed {
        manager.apply(adapter, action, &review.confirmation_token)?;
    }
    Ok(serde_json::json!({
        "adapterId": review.adapter_id,
        "configPath": review.config_path,
        "backupPath": review.backup_path,
        "changed": changed,
    }))
}

/// Everything this command reads from its arguments, parsed once.
///
/// One traversal, because there were two and they disagreed. A `windows(2)`
/// search matches a flag anywhere, including where it is another flag's value;
/// a positional walk does not. With both models present, `--adapter --home /x`
/// meant different things to the code that decides *where* to write and the
/// code that decides *whether* it may — on the one command that writes the
/// user's configuration files.
#[derive(Default)]
struct Options {
    adapter: Option<String>,
    home: Option<String>,
    settings_path: Option<String>,
    confirmed: bool,
}

impl Options {
    fn parse(arguments: &[String]) -> Self {
        let mut options = Self::default();
        let mut rest = arguments.iter();
        while let Some(argument) = rest.next() {
            let field = match argument.as_str() {
                "--adapter" => &mut options.adapter,
                "--home" => &mut options.home,
                "--settings-path" => &mut options.settings_path,
                "--yes" => {
                    options.confirmed = true;
                    continue;
                }
                _ => continue,
            };
            // A flag whose value is missing stays unset rather than swallowing
            // the next flag, and the value is never re-read as one.
            *field = rest.next().cloned();
        }
        options
    }
}

fn build_event_from_payload(
    adapter: v1::AgentAdapterKind,
    payload: &VendorHookPayload,
    pane_id: &str,
    origin_server_identity: &str,
    now: i64,
    home: Option<&Path>,
) -> anyhow::Result<v1::AgentHookEvent> {
    let native_session_id = payload.session_id.clone().unwrap_or_default();
    // Neither currently supported adapter documents a stable hook sequence.
    // Wall time and incidental "generation" fields are not causal ordering.
    let source_generation = 0;
    let source_event_id = payload.event_id.clone().unwrap_or_default();
    let source_event_id = if source_event_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        source_event_id
    };
    let event_name = payload.hook_event_name.clone().unwrap_or_default();
    let codex_turn_start =
        adapter == v1::AgentAdapterKind::Codex && event_name == "UserPromptSubmit";
    let codex_permission =
        adapter == v1::AgentAdapterKind::Codex && event_name == "PermissionRequest";
    let codex_pre_tool = adapter == v1::AgentAdapterKind::Codex && event_name == "PreToolUse";
    let codex_child_event = adapter == v1::AgentAdapterKind::Codex && payload.agent_id.is_some();
    let claude_stop = adapter == v1::AgentAdapterKind::ClaudeCode && event_name == "Stop";
    // Both adapters send the final message on `Stop`; nothing else forwards
    // it, and `StopFailure` forwards nothing. The daemon hands it to voice mode
    // and keeps nothing (docs/mobile/voice-mode-plan.md §4.5).
    let stop = event_name == "Stop";
    let notification_type = payload.notification_type.clone().unwrap_or_default();
    let mut normalized = serde_json::Map::new();
    normalized.insert("hook_event_name".into(), event_name.into());
    if !native_session_id.is_empty() {
        normalized.insert("session_id".into(), native_session_id.clone().into());
    }
    if !notification_type.is_empty() {
        normalized.insert("notification_type".into(), notification_type.into());
    }
    if codex_child_event
        && let Some(agent_id) = payload.agent_id.as_ref().filter(|id| !id.is_empty())
    {
        normalized.insert(
            crate::service::agents::adapters::CODEX_SUBAGENT_ID_FIELD.into(),
            agent_id.clone().into(),
        );
    }
    if claude_stop {
        normalized.insert(
            crate::service::agents::adapters::CLAUDE_HAS_RUNNING_SUBAGENT_FIELD.into(),
            payload.has_running_subagent.into(),
        );
    }
    if stop
        && let Some(message) = payload
            .last_assistant_message
            .as_ref()
            .filter(|message| !message.text.trim().is_empty())
    {
        normalized.insert(
            crate::service::agents::adapters::LAST_ASSISTANT_MESSAGE_FIELD.into(),
            message.text.clone().into(),
        );
        if message.truncated {
            normalized.insert(
                crate::service::agents::adapters::LAST_ASSISTANT_MESSAGE_TRUNCATED_FIELD.into(),
                true.into(),
            );
        }
    }
    if codex_pre_tool {
        let tool_name = payload.tool_name.clone().unwrap_or_default();
        if !tool_name.is_empty() {
            normalized.insert("tool_name".into(), tool_name.into());
        }
    }
    if adapter == v1::AgentAdapterKind::Codex {
        let turn_id = payload.turn_id.clone().unwrap_or_default();
        if !turn_id.is_empty() {
            normalized.insert(
                crate::service::agents::adapters::CODEX_APPROVAL_TURN_ID_FIELD.into(),
                turn_id.into(),
            );
        }
    }
    if codex_turn_start
        && let Some(reviewer) = home.and_then(|home| {
            let transcript_payload = serde_json::json!({
                "turn_id": payload.turn_id.as_deref(),
                "transcript_path": payload.transcript_path.as_deref(),
            });
            codex_transcript::approval_reviewer(&transcript_payload, home)
        })
    {
        normalized.insert(
            crate::service::agents::adapters::CODEX_APPROVAL_REVIEWER_FIELD.into(),
            reviewer.as_str().into(),
        );
    }
    if codex_permission
        && let Some(path) = payload
            .transcript_path
            .as_ref()
            .filter(|path| !path.is_empty())
    {
        // This locator crosses only the private hook-to-daemon socket. The
        // daemon consults the exact-turn positive cache before opening it, and
        // the fallback path below resolves it to a reviewer and removes it
        // before writing a mailbox event.
        normalized.insert(
            crate::service::agents::adapters::CODEX_APPROVAL_TRANSCRIPT_PATH_FIELD.into(),
            path.clone().into(),
        );
    }
    let payload_json = serde_json::to_vec(&normalized)?;
    if payload_json.len() > MAX_NORMALIZED_HOOK_BYTES {
        bail!("normalized hook payload exceeds the {MAX_NORMALIZED_HOOK_BYTES}-byte limit");
    }
    Ok(v1::AgentHookEvent {
        adapter: adapter.into(),
        adapter_id: crate::service::agents::adapters::adapter(adapter)
            .map(|adapter| adapter.id().to_owned())
            .unwrap_or_default(),
        source_event_id,
        source_generation,
        native_session_id,
        pane_id: pane_id.into(),
        payload_json,
        occurred_at_unix_millis: now,
        source_sequence_authoritative: false,
        origin_server_identity: origin_server_identity.into(),
    })
}

fn event_for_fallback(
    event: &v1::AgentHookEvent,
    home: Option<&Path>,
) -> anyhow::Result<v1::AgentHookEvent> {
    let mut event = event.clone();
    if event.adapter_id != "codex" {
        return Ok(event);
    }
    let mut payload: serde_json::Value = serde_json::from_slice(&event.payload_json)?;
    if payload
        .get("hook_event_name")
        .and_then(serde_json::Value::as_str)
        != Some("PermissionRequest")
    {
        return Ok(event);
    }
    let turn_id = payload
        .get(crate::service::agents::adapters::CODEX_APPROVAL_TURN_ID_FIELD)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let transcript_path = payload
        .get(crate::service::agents::adapters::CODEX_APPROVAL_TRANSCRIPT_PATH_FIELD)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if payload
        .get(crate::service::agents::adapters::CODEX_APPROVAL_REVIEWER_FIELD)
        .is_none()
        && let (Some(home), Some(turn_id), Some(transcript_path)) =
            (home, turn_id.as_deref(), transcript_path.as_deref())
        && let Some(reviewer) = codex_transcript::approval_reviewer(
            &serde_json::json!({
                "turn_id": turn_id,
                "transcript_path": transcript_path,
            }),
            home,
        )
    {
        payload[crate::service::agents::adapters::CODEX_APPROVAL_REVIEWER_FIELD] =
            reviewer.as_str().into();
    }
    if let Some(object) = payload.as_object_mut() {
        object.remove(crate::service::agents::adapters::CODEX_APPROVAL_TRANSCRIPT_PATH_FIELD);
    }
    event.payload_json = serde_json::to_vec(&payload)?;
    if event.payload_json.len() > MAX_NORMALIZED_HOOK_BYTES {
        bail!("normalized hook payload exceeds the {MAX_NORMALIZED_HOOK_BYTES}-byte limit");
    }
    Ok(event)
}

#[cfg(test)]
fn build_event(
    adapter: v1::AgentAdapterKind,
    payload: Vec<u8>,
    pane_id: &str,
    origin_server_identity: &str,
    now: i64,
    home: Option<&Path>,
) -> anyhow::Result<v1::AgentHookEvent> {
    let payload = read_vendor_hook(payload.as_slice())?;
    build_event_from_payload(
        adapter,
        &payload,
        pane_id,
        origin_server_identity,
        now,
        home,
    )
}

async fn send(
    socket: &Path,
    event: &v1::AgentHookEvent,
) -> Result<v1::HookIngestDisposition, HookDeliveryFailure> {
    let mut stream = connect_and_handshake(socket)
        .await
        .map_err(HookDeliveryFailure::PreDelivery)?;
    send_request_and_wait(&mut stream, event)
        .await
        .map_err(HookDeliveryFailure::Ambiguous)
}

async fn connect_and_handshake(socket: &Path) -> anyhow::Result<UnixStream> {
    let mut stream = timeout(Duration::from_secs(1), UnixStream::connect(socket))
        .await
        .context("private daemon connection timed out")??;
    write_frame(
        &mut stream,
        &envelope(
            1,
            0,
            v1::envelope::Payload::ClientHello(v1::ClientHello {
                desktop_version: tmux_agent_protocol::HELPER_VERSION.into(),
                requested_capabilities: tmux_agent_protocol::CAP_AGENTS,
                expected_helper_version: tmux_agent_protocol::HELPER_VERSION.into(),
                bulk_connection: false,
                ..Default::default()
            }),
        ),
    )
    .await?;
    let hello = timeout(Duration::from_secs(1), read_frame(&mut stream))
        .await
        .context("private daemon handshake timed out")??
        .context("private daemon closed during handshake")?;
    let Some(v1::envelope::Payload::ServerHello(hello)) = hello.payload else {
        bail!("private daemon returned an invalid handshake");
    };
    if hello.read_only || hello.capabilities & tmux_agent_protocol::CAP_AGENTS == 0 {
        bail!("private daemon does not accept this hook protocol version");
    }
    Ok(stream)
}

async fn send_request_and_wait(
    stream: &mut UnixStream,
    event: &v1::AgentHookEvent,
) -> anyhow::Result<v1::HookIngestDisposition> {
    write_frame(
        stream,
        &envelope(
            2,
            0,
            v1::envelope::Payload::Request(v1::Request {
                operation: v1::Operation::AgentHookIngest.into(),
                agent: Some(v1::AgentRequest {
                    hook_event: Some(event.clone()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
        ),
    )
    .await?;
    loop {
        let frame = timeout(Duration::from_secs(2), read_frame(stream))
            .await
            .context("hook acknowledgement timed out")??
            .context("private daemon closed before hook acknowledgement")?;
        if frame.request_id != 2 {
            continue;
        }
        let Some(v1::envelope::Payload::Response(response)) = frame.payload else {
            bail!("private daemon returned an invalid hook acknowledgement");
        };
        let disposition = v1::HookIngestDisposition::try_from(response.hook_ingest_disposition)
            .unwrap_or_default();
        if disposition != v1::HookIngestDisposition::Unspecified {
            return Ok(disposition);
        }
        bail!("private daemon omitted the typed hook-ingest disposition");
    }
}

/// Events a stopped daemon could not be told about, kept in order.
///
/// This used to be one file per pane, overwritten by each event, and that
/// discarded exactly the sequence the daemon needs. A turn that starts and then
/// blocks while the daemon is down left only the block behind — and the
/// daemon's own rule that a late tool event may not revive a finished turn then
/// correctly ignored it, because the prompt that opened the new turn had been
/// overwritten. The user closed the app mid-turn, the agent asked for
/// permission, and the app came back showing the *previous* turn's result.
///
/// Bounded rather than unbounded: a mailbox that nothing ever drains must not
/// grow without limit, so the oldest events are dropped once a pane has this
/// many waiting. Dropping the oldest keeps the tail, which is the part that
/// describes where the agent ended up.
const MAX_FALLBACK_PER_PANE: usize = 32;

fn persist_latest_fallback(runtime: &Path, event: &v1::AgentHookEvent) -> anyhow::Result<()> {
    let _mailbox = crate::hook_mailbox::HookMailboxLock::acquire(runtime)?;
    let pane = event.pane_id.trim_start_matches('%');
    let adapter = crate::service::agents::adapters::adapter(
        v1::AgentAdapterKind::try_from(event.adapter).unwrap_or_default(),
    )
    .context("unsupported hook adapter")?
    .id();
    let prefix = format!("hook-fallback-{adapter}-{pane}-");
    prune_fallbacks(runtime, &prefix);
    // The mailbox lock serializes hook writers. Derive the next fixed-width
    // sequence from the pending files instead of wall time, which can move
    // backward and would then replay a permission before its turn start.
    let sequence = next_fallback_sequence(runtime, &prefix)?;
    let path = runtime.join(format!("{prefix}{sequence:020}.pb"));
    let temporary = runtime.join(format!(".hook-fallback-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&event.encode_to_vec())?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        fs::File::open(runtime)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn next_fallback_sequence(runtime: &Path, prefix: &str) -> anyhow::Result<u128> {
    let mut latest = None;
    for entry in fs::read_dir(runtime)? {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(sequence) = name
            .strip_prefix(prefix)
            .and_then(|suffix| suffix.strip_suffix(".pb"))
            .and_then(|sequence| sequence.parse::<u128>().ok())
        else {
            continue;
        };
        latest = Some(latest.map_or(sequence, |latest: u128| latest.max(sequence)));
    }
    latest
        .map_or(Some(0), |latest| latest.checked_add(1))
        .context("hook fallback sequence exhausted")
}

/// Drop the oldest waiting events for this pane once the mailbox is full.
///
/// Best effort by design: a mailbox that cannot be pruned is not a reason to
/// lose the event that is being written now.
fn prune_fallbacks(runtime: &Path, prefix: &str) {
    let Ok(entries) = fs::read_dir(runtime) else {
        return;
    };
    let mut existing: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            (name.starts_with(prefix) && name.ends_with(".pb")).then(|| (name, entry.path()))
        })
        .collect();
    if existing.len() < MAX_FALLBACK_PER_PANE {
        return;
    }
    existing.sort_by(|left, right| left.0.cmp(&right.0));
    for (_, path) in existing
        .iter()
        .take(existing.len() + 1 - MAX_FALLBACK_PER_PANE)
    {
        let _ = fs::remove_file(path);
    }
}

fn parse_adapter(arguments: &[String]) -> anyhow::Result<v1::AgentAdapterKind> {
    let value = Options::parse(arguments)
        .adapter
        .context("hook ingest requires --adapter codex|claude-code")?;
    crate::service::agents::adapters::by_id(&value)
        .map(|adapter| adapter.legacy_kind())
        .ok_or_else(|| anyhow::anyhow!("unsupported hook adapter"))
}

/// Which pane this hook belongs to, or `None` when it belongs to no pane.
///
/// The managed hook line is installed once, into the agent's own configuration,
/// and that configuration follows the user everywhere the agent runs — including
/// a plain terminal with no tmux server in sight. There is legitimately nothing
/// to ingest there, and treating it as an error surfaced
/// `UserPromptSubmit hook error … TMUX_PANE is unavailable` on *every* prompt of
/// every such session.
///
/// Absent is the no-tmux case and is silent. Present-but-malformed is not: a
/// value that exists and is not a pane id means something in the environment
/// claims to be tmux and is wrong, which the operator has to be told about. That
/// includes a value that is not UTF-8 at all, which is why the caller passes
/// `var_os` — `var` reports it as absent and would have swallowed exactly the
/// misconfiguration this distinction exists to catch.
fn pane_for_hook(value: Option<std::ffi::OsString>) -> anyhow::Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let pane_id = value
        .to_str()
        .context("TMUX_PANE is not valid UTF-8 and cannot be a tmux pane ID")?;
    validate_pane_id(pane_id)?;
    Ok(Some(pane_id.to_owned()))
}

fn validate_pane_id(value: &str) -> anyhow::Result<()> {
    if value.strip_prefix('%').is_some_and(|digits| {
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
    }) {
        Ok(())
    } else {
        bail!("TMUX_PANE must be an exact tmux % pane ID")
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rejecting_daemon(
        runtime: &Path,
        disposition: v1::HookIngestDisposition,
    ) -> tokio::task::JoinHandle<()> {
        let listener = tokio::net::UnixListener::bind(runtime.join("host.sock")).unwrap();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let hello = read_frame(&mut stream).await.unwrap().unwrap();
            assert!(matches!(
                hello.payload,
                Some(v1::envelope::Payload::ClientHello(_))
            ));
            write_frame(
                &mut stream,
                &envelope(
                    1,
                    0,
                    v1::envelope::Payload::ServerHello(v1::ServerHello {
                        capabilities: tmux_agent_protocol::CAP_AGENTS,
                        ..Default::default()
                    }),
                ),
            )
            .await
            .unwrap();
            let request = read_frame(&mut stream).await.unwrap().unwrap();
            assert_eq!(request.request_id, 2);
            write_frame(
                &mut stream,
                &envelope(
                    2,
                    0,
                    v1::envelope::Payload::Response(v1::Response {
                        ok: false,
                        display_message: "redacted injected rejection".into(),
                        hook_ingest_disposition: disposition.into(),
                        ..Default::default()
                    }),
                ),
            )
            .await
            .unwrap();
        })
    }

    fn fallback_count(runtime: &Path) -> usize {
        fs::read_dir(runtime)
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with("hook-fallback-") && name.ends_with(".pb"))
            })
            .count()
    }

    #[test]
    fn fallback_writers_assign_a_monotonic_sequence_under_the_mailbox_lock() {
        let runtime = tempfile::tempdir().unwrap();
        let prompt = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"UserPromptSubmit","turn_id":"turn-1"}"#.to_vec(),
            "%7",
            "server-a",
            7,
            None,
        )
        .unwrap();
        let permission = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"PermissionRequest","turn_id":"turn-1"}"#.to_vec(),
            "%7",
            "server-a",
            7,
            None,
        )
        .unwrap();

        persist_latest_fallback(runtime.path(), &prompt).unwrap();
        persist_latest_fallback(runtime.path(), &permission).unwrap();

        let mut paths: Vec<_> = fs::read_dir(runtime.path())
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "pb"))
            .collect();
        paths.sort();
        assert_eq!(
            paths
                .iter()
                .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            [
                "hook-fallback-codex-7-00000000000000000000.pb",
                "hook-fallback-codex-7-00000000000000000001.pb",
            ]
        );
        let event_names: Vec<_> = paths
            .iter()
            .map(|path| {
                let event = v1::AgentHookEvent::decode(fs::read(path).unwrap().as_slice()).unwrap();
                serde_json::from_slice::<serde_json::Value>(&event.payload_json)
                    .unwrap()["hook_event_name"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(event_names, ["UserPromptSubmit", "PermissionRequest"]);
    }

    #[test]
    fn adapters_and_pane_identity_are_strict() {
        assert_eq!(
            parse_adapter(&["--adapter".into(), "codex".into()]).unwrap(),
            v1::AgentAdapterKind::Codex
        );
        assert!(parse_adapter(&["--adapter".into(), "other".into()]).is_err());
        assert!(validate_pane_id("%12").is_ok());
        assert!(validate_pane_id("%12;bad").is_err());
    }

    /// Running the agent outside tmux is not a misconfiguration and must not
    /// look like one; running it with a broken `TMUX_PANE` still must.
    #[test]
    fn a_session_outside_tmux_has_no_pane_and_is_not_an_error() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        assert_eq!(pane_for_hook(None).unwrap(), None);
        assert_eq!(
            pane_for_hook(Some("%12".into())).unwrap(),
            Some("%12".to_owned())
        );
        for malformed in ["", "%", "12", "%12;bad", "%1 2"] {
            assert!(
                pane_for_hook(Some(malformed.into())).is_err(),
                "a present but malformed TMUX_PANE ({malformed:?}) must still fail loudly"
            );
        }
        assert!(pane_for_hook(Some(OsString::from_vec(vec![0xff, 0xfe]))).is_err());
    }

    #[test]
    fn vendor_payload_bound_accepts_its_boundary_and_rejects_the_next_byte() {
        let payload = br#"{"hook_event_name":"Stop"}"#;
        assert!(read_vendor_hook_bounded(payload.as_slice(), payload.len()).is_ok());

        let mut oversized = payload.to_vec();
        oversized.push(b' ');
        let error = read_vendor_hook_bounded(oversized.as_slice(), payload.len()).unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("hook payload exceeds the {}-byte limit", payload.len())
        );
    }

    #[test]
    fn large_tool_results_are_discarded_before_claude_or_codex_delivery() {
        let private_image = "private-image-data".repeat(32 * 1024);
        for adapter in [
            v1::AgentAdapterKind::ClaudeCode,
            v1::AgentAdapterKind::Codex,
        ] {
            let raw = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "PostToolUse",
                "session_id": "session-1",
                "tool_name": "Read",
                "tool_response": {
                    "content": [{
                        "type": "image",
                        "source": {"type": "base64", "data": private_image},
                    }],
                },
            }))
            .unwrap();
            assert!(raw.len() > 256 * 1024);

            let event = build_event(adapter, raw, "%12", "tmux:server-a", 7, None).unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&event.payload_json).unwrap(),
                serde_json::json!({
                    "hook_event_name": "PostToolUse",
                    "session_id": "session-1",
                })
            );
            assert!(event.payload_json.len() < 256 * 1024);
            assert!(!String::from_utf8_lossy(&event.payload_json).contains("private-image-data"));
        }
    }

    #[test]
    fn codex_subagent_hooks_forward_only_the_opaque_agent_id() {
        for event_name in ["SubagentStart", "SubagentStop"] {
            let raw = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": event_name,
                "session_id": "session-1",
                "agent_id": "agent-1",
                "agent_type": "private-role",
                "agent_transcript_path": "/private/transcript",
                "last_assistant_message": "private result",
            }))
            .unwrap();
            let event = build_event(
                v1::AgentAdapterKind::Codex,
                raw,
                "%12",
                "tmux:server-a",
                7,
                None,
            )
            .unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&event.payload_json).unwrap();
            assert_eq!(
                payload,
                serde_json::json!({
                    "hook_event_name": event_name,
                    "session_id": "session-1",
                    "agent_id": "agent-1",
                })
            );
            let serialized = payload.to_string();
            for private in ["private-role", "/private/transcript", "private result"] {
                assert!(!serialized.contains(private));
            }
        }
    }

    #[test]
    fn codex_child_activity_retains_its_id_but_not_its_private_payload() {
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"PreToolUse","session_id":"session-1","agent_id":"agent-1","agent_type":"private-role","tool_name":"Bash","tool_input":{"command":"private command"}}"#.to_vec(),
            "%12",
            "tmux:server-a",
            7,
            None,
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&event.payload_json).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({
                "hook_event_name": "PreToolUse",
                "session_id": "session-1",
                "agent_id": "agent-1",
                "tool_name": "Bash",
            })
        );
        let serialized = payload.to_string();
        assert!(!serialized.contains("private-role"));
        assert!(!serialized.contains("private command"));
    }

    #[test]
    fn codex_lifecycle_events_retain_the_exact_turn_for_pending_permission_cleanup() {
        for event_name in ["PreToolUse", "PostToolUse", "SubagentStop", "Stop"] {
            let event = build_event(
                v1::AgentAdapterKind::Codex,
                serde_json::to_vec(&serde_json::json!({
                    "hook_event_name": event_name,
                    "session_id": "session-1",
                    "agent_id": "agent-1",
                    "turn_id": "turn-1",
                    "tool_input": {"command": "private command"},
                }))
                .unwrap(),
                "%12",
                "tmux:server-a",
                7,
                None,
            )
            .unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&event.payload_json).unwrap();
            assert_eq!(
                payload[crate::service::agents::adapters::CODEX_APPROVAL_TURN_ID_FIELD],
                "turn-1"
            );
            assert!(!payload.to_string().contains("private command"));
        }
    }

    #[test]
    fn oversized_retained_fields_are_rejected_before_event_delivery() {
        for field in ["session_id", "agent_id", "event_id"] {
            let raw = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "Stop",
                (field): "x".repeat(MAX_LIFECYCLE_FIELD_BYTES + 1),
            }))
            .unwrap();
            let error = read_vendor_hook(raw.as_slice()).unwrap_err();
            let detail = format!("{error:#}");
            assert!(
                detail.contains(&format!(
                    "hook lifecycle field exceeds the {MAX_LIFECYCLE_FIELD_BYTES}-byte limit"
                )),
                "unexpected {field} error: {detail}"
            );
        }
    }

    #[test]
    fn malformed_and_duplicate_fields_keep_the_previous_tolerant_semantics() {
        let raw = br#"{
            "hook_event_name": 7,
            "hookEventName": "Stop",
            "session_id": "replaced",
            "session_id": null,
            "sessionId": "session-from-alias",
            "background_tasks": [
                null,
                7,
                ["not", "a", "task"],
                {"type": 7, "status": "running"},
                {"type": "subagent", "status": "running", "command": "private"}
            ]
        }"#;
        let event = build_event(
            v1::AgentAdapterKind::ClaudeCode,
            raw.to_vec(),
            "%12",
            "tmux:server-a",
            7,
            None,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&event.payload_json).unwrap(),
            serde_json::json!({
                "hook_event_name": "Stop",
                "session_id": "session-from-alias",
                "has_running_subagent": true,
            })
        );

        let null_tasks = build_event(
            v1::AgentAdapterKind::ClaudeCode,
            br#"{"hook_event_name":"Stop","background_tasks":null}"#.to_vec(),
            "%12",
            "tmux:server-a",
            7,
            None,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&null_tasks.payload_json).unwrap(),
            serde_json::json!({
                "hook_event_name": "Stop",
                "has_running_subagent": false,
            })
        );
    }

    #[test]
    fn hook_events_use_the_exact_pane_and_are_unsequenced() {
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"Stop","generation":99}"#.to_vec(),
            "%12",
            "tmux:server-a",
            7,
            None,
        )
        .unwrap();
        assert_eq!(event.pane_id, "%12");
        assert_eq!(event.origin_server_identity, "tmux:server-a");
        assert_eq!(event.source_generation, 0);
        assert!(!event.source_sequence_authoritative);
    }

    #[test]
    fn fallback_envelope_contains_only_normalized_lifecycle_fields() {
        let event = build_event(
            v1::AgentAdapterKind::ClaudeCode,
            br#"{"hook_event_name":"Stop","session_id":"s","prompt":"private prompt","api_token":"secret","background_tasks":[{"id":"agent-private","type":"subagent","status":"running","command":"private command","description":"private task"}]}"#.to_vec(),
            "%12",
            "tmux:server-a",
            7,
            None,
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&event.payload_json).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({
                "hook_event_name": "Stop",
                "session_id": "s",
                "has_running_subagent": true,
            })
        );
        let serialized = payload.to_string();
        for private in [
            "background_tasks",
            "agent-private",
            "private command",
            "private task",
            "secret",
            "prompt",
        ] {
            assert!(!serialized.contains(private));
        }
    }

    /// Voice mode's one hook change (docs/mobile/voice-mode-plan.md §4.7):
    /// the final message rides `Stop` for both adapters, is cut on a char
    /// boundary past 32 KiB without failing the hook, and rides nothing else.
    #[test]
    fn stop_forwards_the_last_assistant_message_for_both_adapters() {
        for adapter in [
            v1::AgentAdapterKind::ClaudeCode,
            v1::AgentAdapterKind::Codex,
        ] {
            let event = build_event(
                adapter,
                br#"{"hook_event_name":"Stop","session_id":"s","last_assistant_message":"All done.\n\nSee `x`."}"#.to_vec(),
                "%12",
                "tmux:server-a",
                7,
                None,
            )
            .unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&event.payload_json).unwrap();
            assert_eq!(
                payload[crate::service::agents::adapters::LAST_ASSISTANT_MESSAGE_FIELD],
                "All done.\n\nSee `x`."
            );
            assert!(
                payload
                    .get(crate::service::agents::adapters::LAST_ASSISTANT_MESSAGE_TRUNCATED_FIELD)
                    .is_none()
            );
        }
        let camel = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"Stop","lastAssistantMessage":"camel"}"#.to_vec(),
            "%12",
            "tmux:server-a",
            7,
            None,
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&camel.payload_json).unwrap();
        assert_eq!(
            payload[crate::service::agents::adapters::LAST_ASSISTANT_MESSAGE_FIELD],
            "camel"
        );
    }

    #[test]
    fn a_forty_kib_message_is_truncated_on_a_char_boundary_and_the_stop_survives() {
        // 'é' is two bytes; the 32 KiB bound falls inside one of them, and the
        // cut must land before it, not through it.
        let message = "é".repeat(20 * 1024);
        assert!(message.len() > 40 * 1024 - 1024);
        let raw = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "Stop",
            "session_id": "s",
            "last_assistant_message": message,
        }))
        .unwrap();
        let event = build_event(
            v1::AgentAdapterKind::ClaudeCode,
            raw,
            "%12",
            "tmux:server-a",
            7,
            None,
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&event.payload_json).unwrap();
        let forwarded = payload[crate::service::agents::adapters::LAST_ASSISTANT_MESSAGE_FIELD]
            .as_str()
            .unwrap();
        assert_eq!(forwarded.len(), MAX_ASSISTANT_MESSAGE_BYTES);
        assert!(forwarded.chars().all(|c| c == 'é'));
        assert_eq!(
            payload[crate::service::agents::adapters::LAST_ASSISTANT_MESSAGE_TRUNCATED_FIELD],
            true
        );
        assert!(event.payload_json.len() <= MAX_NORMALIZED_HOOK_BYTES);
    }

    #[test]
    fn the_message_rides_only_stop_and_a_blank_or_malformed_one_is_dropped() {
        for (event_name, adapter) in [
            ("StopFailure", v1::AgentAdapterKind::ClaudeCode),
            ("UserPromptSubmit", v1::AgentAdapterKind::Codex),
            ("PostToolUse", v1::AgentAdapterKind::ClaudeCode),
            ("SubagentStop", v1::AgentAdapterKind::Codex),
        ] {
            let raw = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": event_name,
                "session_id": "s",
                "last_assistant_message": "private reply",
            }))
            .unwrap();
            let event = build_event(adapter, raw, "%12", "tmux:server-a", 7, None).unwrap();
            assert!(
                !String::from_utf8_lossy(&event.payload_json).contains("private reply"),
                "{event_name} forwarded the message"
            );
        }
        for value in [
            serde_json::json!("   "),
            serde_json::json!(7),
            serde_json::json!(null),
            serde_json::json!({"text": "nested"}),
            serde_json::json!(["list"]),
        ] {
            let raw = serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "Stop",
                "last_assistant_message": value,
            }))
            .unwrap();
            let event = build_event(
                v1::AgentAdapterKind::ClaudeCode,
                raw,
                "%12",
                "tmux:server-a",
                7,
                None,
            )
            .unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&event.payload_json).unwrap();
            assert!(
                payload
                    .get(crate::service::agents::adapters::LAST_ASSISTANT_MESSAGE_FIELD)
                    .is_none(),
                "{value}"
            );
        }
    }

    #[test]
    fn claude_stop_ignores_non_subagent_background_work() {
        let event = build_event(
            v1::AgentAdapterKind::ClaudeCode,
            br#"{"hook_event_name":"Stop","session_id":"s","background_tasks":[{"id":"shell-private","type":"local_bash","status":"running","command":"sleep 300"}],"session_crons":[{"prompt":"private cron"}]}"#.to_vec(),
            "%12",
            "tmux:server-a",
            7,
            None,
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&event.payload_json).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({
                "hook_event_name": "Stop",
                "session_id": "s",
                "has_running_subagent": false,
            })
        );
    }

    #[test]
    fn codex_question_normalizes_only_the_tool_name_discriminator() {
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"PreToolUse","session_id":"session-1","tool_name":"request_user_input","tool_input":{"questions":[{"question":"private question","options":[{"label":"private choice"}]}]},"prompt":"private prompt","api_token":"secret"}"#.to_vec(),
            "%12",
            "tmux:server-a",
            7,
            None,
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&event.payload_json).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({
                "hook_event_name": "PreToolUse",
                "session_id": "session-1",
                "tool_name": "request_user_input",
            })
        );
    }

    #[test]
    fn codex_permission_defers_revalidation_and_fallback_materializes_it() {
        let home = tempfile::tempdir().unwrap();
        let sessions = home.path().join(".codex/sessions/2026/08/22");
        fs::create_dir_all(&sessions).unwrap();
        let transcript = sessions.join("rollout.jsonl");
        fs::write(&transcript, "").unwrap();
        let raw = serde_json::to_vec(&serde_json::json!({
            "hook_event_name": "UserPromptSubmit",
            "session_id": "session-1",
            "turn_id": "turn-1",
            "transcript_path": &transcript,
            "prompt": "private prompt",
            "api_token": "secret"
        }))
        .unwrap();
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            raw,
            "%12",
            "tmux:server-a",
            7,
            Some(home.path()),
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&event.payload_json).unwrap();
        assert!(
            payload
                .get(crate::service::agents::adapters::CODEX_APPROVAL_REVIEWER_FIELD)
                .is_none()
        );
        assert_eq!(
            payload.get(crate::service::agents::adapters::CODEX_APPROVAL_TURN_ID_FIELD),
            Some(&serde_json::Value::String("turn-1".into()))
        );
        let serialized = payload.to_string();
        for private in ["transcript_path", "private prompt", "secret"] {
            assert!(!serialized.contains(private));
        }
        assert!(payload.get("turn_id").is_none());

        fs::write(
            &transcript,
            serde_json::json!({
                "type": "turn_context",
                "payload": {
                    "turn_id": "turn-1",
                    "approval_policy": "on-request",
                    "approvals_reviewer": "auto_review"
                }
            })
            .to_string()
                + "\n",
        )
        .unwrap();

        let permission = build_event(
            v1::AgentAdapterKind::Codex,
            serde_json::to_vec(&serde_json::json!({
                "hook_event_name": "PermissionRequest",
                "session_id": "session-1",
                "turn_id": "turn-1",
                "transcript_path": &transcript,
            }))
            .unwrap(),
            "%12",
            "tmux:server-a",
            8,
            Some(home.path()),
        )
        .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&permission.payload_json).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({
                "hook_event_name": "PermissionRequest",
                "session_id": "session-1",
                crate::service::agents::adapters::CODEX_APPROVAL_TURN_ID_FIELD: "turn-1",
                crate::service::agents::adapters::CODEX_APPROVAL_TRANSCRIPT_PATH_FIELD: transcript,
            })
        );

        let fallback = event_for_fallback(&permission, Some(home.path())).unwrap();
        let fallback_payload: serde_json::Value =
            serde_json::from_slice(&fallback.payload_json).unwrap();
        assert_eq!(
            fallback_payload,
            serde_json::json!({
                "hook_event_name": "PermissionRequest",
                "session_id": "session-1",
                crate::service::agents::adapters::CODEX_APPROVAL_TURN_ID_FIELD: "turn-1",
                crate::service::agents::adapters::CODEX_APPROVAL_REVIEWER_FIELD: "auto_review",
            })
        );
        assert!(!String::from_utf8_lossy(&fallback.payload_json).contains("rollout.jsonl"));
    }

    #[tokio::test]
    async fn retryable_live_rejection_writes_exactly_one_fallback_event() {
        let runtime = std::env::temp_dir().join(format!(
            "ade-hr-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        ));
        fs::create_dir_all(&runtime).unwrap();
        let server = rejecting_daemon(&runtime, v1::HookIngestDisposition::Retryable);
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"PermissionRequest","event_id":"retry-once"}"#.to_vec(),
            "%7",
            "server-a",
            7,
            None,
        )
        .unwrap();

        deliver(std::slice::from_ref(&runtime), &event, None)
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(fallback_count(&runtime), 1);
        fs::remove_dir_all(runtime).unwrap();
    }

    #[tokio::test]
    async fn permanent_live_rejection_is_discarded_without_a_fallback_event() {
        let runtime = std::env::temp_dir().join(format!(
            "ade-hd-{}",
            &uuid::Uuid::new_v4().simple().to_string()[..12]
        ));
        fs::create_dir_all(&runtime).unwrap();
        let server = rejecting_daemon(&runtime, v1::HookIngestDisposition::Discarded);
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"PermissionRequest","event_id":"discard-once"}"#.to_vec(),
            "%7",
            "server-a",
            7,
            None,
        )
        .unwrap();

        deliver(std::slice::from_ref(&runtime), &event, None)
            .await
            .unwrap();
        server.await.unwrap();
        assert_eq!(fallback_count(&runtime), 0);
        fs::remove_dir_all(runtime).unwrap();
    }

    #[tokio::test]
    async fn ambiguous_ack_never_retries_a_second_daemon() {
        // A short root, not the default tempdir: the sockets below must stay
        // under the platform's 104/108-byte bind limit, and macOS puts the
        // default tempdir 50+ bytes deep under /var/folders.
        let root =
            std::path::PathBuf::from("/tmp").join(format!("ade-ha-{}", uuid::Uuid::new_v4()));
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        let first_listener = tokio::net::UnixListener::bind(first.join("host.sock")).unwrap();
        let second_listener = tokio::net::UnixListener::bind(second.join("host.sock")).unwrap();
        let first_server = tokio::spawn(async move {
            let (mut stream, _) = first_listener.accept().await.unwrap();
            let _hello = read_frame(&mut stream).await.unwrap().unwrap();
            write_frame(
                &mut stream,
                &envelope(
                    1,
                    0,
                    v1::envelope::Payload::ServerHello(v1::ServerHello {
                        capabilities: tmux_agent_protocol::CAP_AGENTS,
                        ..Default::default()
                    }),
                ),
            )
            .await
            .unwrap();
            let request = read_frame(&mut stream).await.unwrap().unwrap();
            assert_eq!(request.request_id, 2);
            // Applied but acknowledgement lost.
        });
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"Stop","event_id":"ambiguous"}"#.to_vec(),
            "%7",
            "server-a",
            7,
            None,
        )
        .unwrap();
        deliver(&[first.clone(), second.clone()], &event, None)
            .await
            .unwrap();
        first_server.await.unwrap();
        assert!(
            timeout(Duration::from_millis(100), second_listener.accept())
                .await
                .is_err()
        );
        assert_eq!(fallback_count(&first), 1);
        assert_eq!(fallback_count(&second), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_fallback_publishers_preserve_the_exact_per_pane_bound() {
        let runtime = std::env::temp_dir().join(format!("ade-hb-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&runtime).unwrap();
        let event = build_event(
            v1::AgentAdapterKind::Codex,
            br#"{"hook_event_name":"Stop","event_id":"bounded"}"#.to_vec(),
            "%7",
            "server-a",
            7,
            None,
        )
        .unwrap();
        let threads = (0..64)
            .map(|_| {
                let runtime = runtime.clone();
                let event = event.clone();
                std::thread::spawn(move || persist_latest_fallback(&runtime, &event).unwrap())
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(fallback_count(&runtime), MAX_FALLBACK_PER_PANE);
        fs::remove_dir_all(runtime).unwrap();
    }
}
