//! The assistant's way to a model: the OpenAI `/chat/completions` contract,
//! streamed. Port of the Tauri app's `src-tauri/src/ai.rs`.
//!
//! The endpoint is whatever the user configured in Settings → Assistant; only
//! the shape of the call is fixed, because local servers (Ollama, MLX, vLLM,
//! llama.cpp, LM Studio) and hosted APIs both answer it. The request leaves
//! this computer directly; it never travels through a connected host.
//!
//! The reply streams: deltas come out of [`ChatStream::next_event`] as the
//! server produces them. Tool calls are the one thing not passed through as
//! they arrive: the server streams them in fragments, and a half-parsed action
//! is worse than none, so the fragments are accumulated and complete calls are
//! reported once the answer ends. Whether any of them is *executed* is the
//! panel's decision; this module only reports what the model asked for.
//! [`ChatStream::cancel`] stops a generation the user walked away from, and a
//! dropped stream ends the read loop on its own.

use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt as _;
use futures::channel::mpsc;
use serde::Deserialize;
use serde_json::{Value, json};

use desktop_core::platform;

/// One message of the call, in the shape the panel assembles it. Roles are the
/// OpenAI set, tools included: an assistant turn may carry `tool_calls`
/// instead of text, and a `tool` turn answers one call by id.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub content: Option<String>,
    /// Set on a `tool` answer: the call it answers.
    pub tool_call_id: Option<String>,
    /// The OpenAI-shaped call echo (`{id, function: {name, arguments}}`),
    /// built by the panel from a call this module reported; the server must
    /// see its own shape back.
    pub tool_calls: Option<Vec<Value>>,
}

impl ChatMessage {
    pub fn text(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.to_owned(),
            content: Some(content.into()),
            tool_call_id: None,
            tool_calls: None,
        }
    }
}

/// The endpoint and key to chat with. [`Self::configured`] is the panel's gate.
#[derive(Debug, Clone, Default)]
pub struct ChatSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: String,
}

impl ChatSettings {
    pub fn configured(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.model.trim().is_empty()
    }
}

/// One streamed event: text, a finished tool call, the reported cost of the
/// round, the end, or a refusal.
#[derive(Debug, Clone)]
pub enum AiEvent {
    Delta(String),
    /// A complete tool call the model asked for: `arguments` is the raw
    /// argument JSON the server streamed, for the panel to parse and run.
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    /// The tokens the server says this round's answer cost, when it says.
    /// Not every server reports usage; the panel shows a count only when one
    /// did.
    Usage {
        completion_tokens: u64,
    },
    Done,
    Error(String),
}

/// The longest the panel's system prompt or a pasted question may be. The
/// context snapshot is a few kilobytes; anything far past that is a mistake or
/// an attempt to make the app exfiltrate a document through a model.
const MAX_MESSAGE_BYTES: usize = 256 * 1024;
/// The panel keeps a short conversation; a longer history is not a chat.
const MAX_MESSAGES: usize = 64;
const MAX_MODEL_BYTES: usize = 256;
const MAX_URL_BYTES: usize = 2_048;
const MAX_TOOL_CALLS: usize = 8;
const MAX_ID_BYTES: usize = 128;
const MAX_NAME_BYTES: usize = 64;
const MAX_TOOLS: usize = 16;
/// Models one server may report before the list stops being a list. Past this
/// the answer is a mistake or an attack on the Settings dialog's patience.
const MAX_MODELS: usize = 512;
/// A model list is a table of ids; anything near this size is not one.
const MAX_LIST_BYTES: u64 = 4 * 1024 * 1024;
/// A non-streaming server's whole answer, read in one piece.
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// Time to wait for the response *headers*; the body may stream far longer.
const FIRST_BYTE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// A generation in flight. Events arrive from the read thread; [`Self::cancel`]
/// asks it to stop, and dropping the stream ends it too: the read loop notices
/// its channel is closed and returns without waiting for the server.
pub struct ChatStream {
    cancel: Arc<AtomicBool>,
    events: mpsc::UnboundedReceiver<AiEvent>,
    finished: bool,
}

impl ChatStream {
    /// The next event, `None` once the stream is over. A terminal `Done` or
    /// `Error` is always the last event, so a loop can end on it and never
    /// hangs on a server that kept the connection open.
    pub async fn next_event(&mut self) -> Option<AiEvent> {
        if self.finished {
            return None;
        }
        let event = self.events.next().await;
        if matches!(event, Some(AiEvent::Done) | Some(AiEvent::Error(_)) | None) {
            self.finished = true;
        }
        event
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The flag itself, for whoever parks it on a conversation so a Stop
    /// button can reach the read thread.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
}

/// Ask the configured model, streaming the answer into the returned stream.
/// Validation that cannot become a request (bad endpoint, oversized body) is
/// the `Err` here, before any socket opens.
pub fn chat(
    settings: ChatSettings,
    messages: Vec<ChatMessage>,
    tools: Option<Vec<Value>>,
) -> Result<ChatStream, String> {
    let prepared = prepare(&settings, &messages, tools.as_ref())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (sender, receiver) = mpsc::unbounded::<AiEvent>();
    let running = cancel.clone();
    platform::spawn_blocking(move || {
        let result = run(&prepared, &sender, &running);
        // The read loop ends without a terminal event; the panel's loop needs
        // one whichever way the answer ended.
        if result.is_ok() {
            let _ = sender.unbounded_send(AiEvent::Done);
        }
    });
    Ok(ChatStream {
        cancel,
        events: receiver,
        finished: false,
    })
}

/// Ask the configured server which models it serves, for Settings → Assistant
/// to offer instead of a hand-typed id. The OpenAI `GET /models` contract,
/// which the local servers answer as readily as the chat endpoint.
pub async fn list_models(settings: ChatSettings) -> Result<Vec<String>, String> {
    if settings.base_url.len() > MAX_URL_BYTES {
        return Err("the assistant endpoint is implausibly long".into());
    }
    let url = models_url(&settings.base_url)?;
    let api_key = settings.api_key;
    platform::spawn_blocking(move || read_models(&url, api_key.trim()))
        .await
        .map_err(|error| format!("the model list could not be fetched: {error}"))?
}

struct Prepared {
    url: String,
    body: Value,
    api_key: String,
}

fn prepare(
    settings: &ChatSettings,
    messages: &[ChatMessage],
    tools: Option<&Vec<Value>>,
) -> Result<Prepared, String> {
    if messages.is_empty() || messages.len() > MAX_MESSAGES {
        return Err(format!("between 1 and {MAX_MESSAGES} messages are allowed"));
    }
    for message in messages {
        if !matches!(
            message.role.as_str(),
            "system" | "user" | "assistant" | "tool"
        ) {
            return Err("only system, user, assistant and tool roles are allowed".into());
        }
        if let Some(content) = &message.content
            && content.len() > MAX_MESSAGE_BYTES
        {
            return Err(format!(
                "a message exceeds {} KiB",
                MAX_MESSAGE_BYTES / 1024
            ));
        }
        if message.role == "tool" && message.tool_call_id.as_deref().unwrap_or("").is_empty() {
            return Err("a tool answer must name the call it answers".into());
        }
        if let Some(calls) = &message.tool_calls {
            if calls.len() > MAX_TOOL_CALLS {
                return Err(format!("at most {MAX_TOOL_CALLS} tool calls per message"));
            }
            let size =
                serde_json::to_string(calls).map_err(|_| "unreadable tool calls".to_owned())?;
            if size.len() > MAX_MESSAGE_BYTES {
                return Err("the echoed tool calls are implausibly large".into());
            }
        }
    }
    if settings.model.trim().is_empty() || settings.model.len() > MAX_MODEL_BYTES {
        return Err("set a model in Settings → Assistant".into());
    }
    if settings.base_url.len() > MAX_URL_BYTES {
        return Err("the assistant endpoint is implausibly long".into());
    }
    let tools = match tools {
        Some(tools) if !tools.is_empty() => {
            if tools.len() > MAX_TOOLS {
                return Err(format!("at most {MAX_TOOLS} tools may be offered"));
            }
            let size = serde_json::to_string(tools).map_err(|_| "unreadable tools".to_owned())?;
            if size.len() > MAX_MESSAGE_BYTES {
                return Err("the tool table is implausibly large".into());
            }
            Some(tools.clone())
        }
        _ => None,
    };
    let url = completions_url(&settings.base_url)?;
    let mut body = json!({
        "model": settings.model.trim(),
        "messages": messages.iter().map(wire_message).collect::<Vec<_>>(),
        "stream": true,
    });
    // Ask servers that honor it to end the stream with a usage line; the panel
    // shows the answer's token count when there is one to show. A server that
    // chokes on the field gets the request again without it.
    body["stream_options"] = json!({ "include_usage": true });
    if let Some(tools) = tools {
        body["tools"] = Value::Array(tools);
    }
    Ok(Prepared {
        url,
        body,
        api_key: settings.api_key.clone(),
    })
}

/// One message in the shape the *server* expects — snake_case, with absent
/// fields absent rather than null, which is what lets an assistant turn carry
/// tool calls and no content.
fn wire_message(message: &ChatMessage) -> Value {
    let mut value = json!({ "role": message.role });
    if let Some(content) = &message.content {
        value["content"] = Value::String(content.clone());
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        value["tool_call_id"] = Value::String(tool_call_id.clone());
    }
    if let Some(calls) = &message.tool_calls {
        value["tool_calls"] = Value::Array(calls.clone());
    }
    value
}

/// The endpoint from the configured base: `…/v1` becomes `…/v1/chat/completions`,
/// and a full endpoint passes through. Only http and https reach a server.
fn completions_url(base_url: &str) -> Result<String, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .ok_or("the assistant endpoint must start with http:// or https://")?;
    if rest.is_empty() {
        return Err("the assistant endpoint names no server".into());
    }
    if trimmed.ends_with("/chat/completions") {
        return Ok(trimmed.to_owned());
    }
    Ok(format!("{trimmed}/chat/completions"))
}

/// The list endpoint from the configured base: `…/v1` becomes `…/v1/models`.
/// A base pasted as the full chat endpoint is rewritten to the equivalent list
/// endpoint, so the one field a user fills answers both calls.
fn models_url(base_url: &str) -> Result<String, String> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"))
        .ok_or("the assistant endpoint must start with http:// or https://")?;
    if rest.is_empty() {
        return Err("the assistant endpoint names no server".into());
    }
    if let Some(root) = trimmed.strip_suffix("/chat/completions") {
        return Ok(format!("{root}/models"));
    }
    Ok(format!("{trimmed}/models"))
}

/// One `GET /models`, answered in a page of ids or refused with the server's
/// own words. It is a list, not a generation: no cancel registry, and a
/// response timeout a person will actually wait on rather than the twelve
/// minutes a thinking model is allowed its first token.
fn read_models(url: &str, api_key: &str) -> Result<Vec<String>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(std::time::Duration::from_secs(20)))
        .http_status_as_error(false)
        .build()
        .into();
    let mut request = agent.get(url);
    if !api_key.is_empty() {
        request = request.header("authorization", format!("Bearer {api_key}"));
    }
    let mut response = match request.call() {
        Ok(response) => response,
        Err(error) => return Err(format!("the model server could not be reached: {error}")),
    };
    if !response.status().is_success() {
        let body = response
            .body_mut()
            .with_config()
            .limit(16 * 1024)
            .read_to_string()
            .unwrap_or_default();
        return Err(server_refusal(response.status().as_u16(), &body));
    }
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_LIST_BYTES)
        .read_to_string()
        .map_err(|error| format!("the model list could not be read: {error}"))?;
    model_ids(&body)
}

/// The ids from an OpenAI `{"data": [{"id": …}]}` body, in the server's own
/// order with repeats dropped. An entry with no usable id is skipped rather
/// than fatal: a server that lists one odd extra row is still worth hearing
/// from. A body that is not a model list at all says so, because the user
/// staring at a "no models" line cannot tell an empty server from a wrong port
/// serving HTML.
fn model_ids(body: &str) -> Result<Vec<String>, String> {
    let value: Value =
        serde_json::from_str(body).map_err(|_| "the model server's reply was not a model list")?;
    let entries = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or("the model server's reply carried no model list")?;
    let mut ids: Vec<String> = Vec::new();
    for entry in entries {
        if ids.len() >= MAX_MODELS {
            break;
        }
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        let id = id.trim();
        if id.is_empty() || ids.iter().any(|seen| seen == id) {
            continue;
        }
        ids.push(id.to_owned());
    }
    // Settings shows the list to a person scanning for one name; the servers'
    // own orders (load order, arrival order) do not help with that.
    ids.sort_by_key(|id| id.to_lowercase());
    Ok(ids)
}

fn run(
    prepared: &Prepared,
    sink: &mpsc::UnboundedSender<AiEvent>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(FIRST_BYTE_TIMEOUT))
        // A 4xx/5xx arrives as a response, not as an error: the server's own
        // `{"error": …}` body is the message the user needs to see.
        .http_status_as_error(false)
        .build()
        .into();
    // The body is sent as text with the JSON content type rather than through
    // `send_json`, which ureq keeps behind its `json` feature; the body is
    // already a `serde_json::Value`'s string.
    let mut body = prepared.body.clone();
    let send = |body: &Value| {
        let mut request = agent.post(&prepared.url);
        if !prepared.api_key.trim().is_empty() {
            request = request.header(
                "authorization",
                format!("Bearer {}", prepared.api_key.trim()),
            );
        }
        request
            .header("content-type", "application/json")
            .send(body.to_string())
    };
    let mut response = match send(&body) {
        Ok(response) => response,
        Err(error) => {
            let message = format!("the model server could not be reached: {error}");
            let _ = sink.unbounded_send(AiEvent::Error(message.clone()));
            return Err(message);
        }
    };
    if response.status().as_u16() == 400
        && body
            .as_object()
            .is_some_and(|map| map.contains_key("stream_options"))
    {
        // A 400 with `stream_options` in the body reads as "I do not know that
        // field": ask again without it. The usage line was always a nicety;
        // the answer is the point.
        if let Some(map) = body.as_object_mut() {
            map.remove("stream_options");
        }
        response = match send(&body) {
            Ok(response) => response,
            Err(error) => {
                let message = format!("the model server could not be reached: {error}");
                let _ = sink.unbounded_send(AiEvent::Error(message.clone()));
                return Err(message);
            }
        };
    }
    if !response.status().is_success() {
        let body = response
            .body_mut()
            .with_config()
            .limit(16 * 1024)
            .read_to_string()
            .unwrap_or_default();
        let message = server_refusal(response.status().as_u16(), &body);
        let _ = sink.unbounded_send(AiEvent::Error(message.clone()));
        return Err(message);
    }
    let streaming = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"));
    if streaming {
        let mut reader = BufReader::new(response.body_mut().as_reader());
        return stream_reply(&mut reader, sink, cancel);
    }
    // A server that ignored `stream` answered in one piece; forward the whole
    // answer as a single delta so the panel sees one shape either way. A
    // non-streaming server that answered with tool calls is not supported —
    // the panel would have no call stream to accumulate — and says so.
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE_BYTES)
        .read_to_string()
        .map_err(|error| format!("the answer could not be read: {error}"))?;
    match whole_reply(&body) {
        WholeReply::Text(text) => {
            if sink.unbounded_send(AiEvent::Delta(text)).is_err() {
                return Err("the panel closed mid-answer".into());
            }
            if let Some(tokens) = reported_tokens(&body) {
                let _ = sink.unbounded_send(AiEvent::Usage {
                    completion_tokens: tokens,
                });
            }
            Ok(())
        }
        WholeReply::Refusal(message) => {
            let _ = sink.unbounded_send(AiEvent::Error(message.clone()));
            Err(message)
        }
    }
}

/// Read the body as SSE and forward each delta; accumulate tool-call fragments
/// and report the complete calls only once the stream ends.
fn stream_reply(
    reader: &mut impl BufRead,
    sink: &mpsc::UnboundedSender<AiEvent>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut saw_content = false;
    let mut usage: Option<u64> = None;
    let mut calls = ToolCalls::default();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error) => {
                let message = format!("the answer was cut short: {error}");
                let _ = sink.unbounded_send(AiEvent::Error(message.clone()));
                return Err(message);
            }
        }
        let Some(data) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }
        let chunk: StreamChunk = match serde_json::from_str(data) {
            Ok(chunk) => chunk,
            // Keep-alives and comments are not JSON; a real chunk always is.
            Err(_) => continue,
        };
        if let Some(error) = chunk.error.and_then(|error| error.message) {
            let message = format!("the model server refused: {error}");
            let _ = sink.unbounded_send(AiEvent::Error(message.clone()));
            return Err(message);
        }
        // Servers that honor `include_usage` end the stream with a chunk of no
        // choices and the round's counts; the last one seen is the truth.
        if let Some(tokens) = chunk.usage.and_then(|reported| reported.completion_tokens) {
            usage = Some(tokens);
        }
        for choice in chunk.choices {
            if let Some(text) = choice
                .delta
                .as_ref()
                .and_then(|delta| delta.content.as_ref())
                .filter(|text| !text.is_empty())
            {
                saw_content = true;
                if sink.unbounded_send(AiEvent::Delta(text.clone())).is_err() {
                    // Nobody is listening; stop generating.
                    return Ok(());
                }
            }
            for fragment in choice
                .delta
                .unwrap_or_default()
                .tool_calls
                .unwrap_or_default()
            {
                saw_content = true;
                if let Err(message) = calls.apply(fragment) {
                    let _ = sink.unbounded_send(AiEvent::Error(message.clone()));
                    return Err(message);
                }
            }
        }
    }
    let complete = calls.finish();
    if let Some(tokens) = usage {
        let _ = sink.unbounded_send(AiEvent::Usage {
            completion_tokens: tokens,
        });
    }
    if !complete.is_empty() {
        // The calls are reported after the text they followed, in the order
        // the model asked for them.
        for call in complete {
            if sink
                .unbounded_send(AiEvent::ToolCall {
                    id: call.id,
                    name: call.name,
                    arguments: call.arguments,
                })
                .is_err()
            {
                return Ok(());
            }
        }
        return Ok(());
    }
    if saw_content || cancel.load(Ordering::Relaxed) {
        return Ok(());
    }
    let message = "the model server answered without content".to_owned();
    let _ = sink.unbounded_send(AiEvent::Error(message.clone()));
    Err(message)
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    error: Option<ServerError>,
    #[serde(default)]
    usage: Option<Usage>,
}

/// The usage line a server may append: only the answer's cost is wanted.
#[derive(Deserialize)]
struct Usage {
    completion_tokens: Option<u64>,
}

/// The answer's token count from a whole (non-streaming) response body, when
/// the server reported one.
fn reported_tokens(body: &str) -> Option<u64> {
    #[derive(Deserialize)]
    struct WithUsage {
        #[serde(default)]
        usage: Option<Usage>,
    }
    serde_json::from_str::<WithUsage>(body)
        .ok()?
        .usage?
        .completion_tokens
}

#[derive(Deserialize, Default)]
struct ChunkChoice {
    delta: Option<ChunkDelta>,
}

#[derive(Deserialize, Default)]
struct ChunkDelta {
    content: Option<String>,
    tool_calls: Option<Vec<DeltaToolCall>>,
}

/// One fragment of one tool call. Servers stream the id and name with the
/// first fragment and the arguments across many; `index` ties them together.
#[derive(Deserialize)]
struct DeltaToolCall {
    index: Option<u32>,
    id: Option<String>,
    function: Option<DeltaFunction>,
}

#[derive(Deserialize)]
struct DeltaFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ServerError {
    message: Option<String>,
}

/// A tool call the server has finished streaming.
struct FinishedCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
    /// Some servers omit `index`; fragments then arrive in order, and the
    /// first new id seen opens the next slot.
    open: bool,
}

#[derive(Default)]
struct ToolCalls {
    by_index: Vec<PartialCall>,
}

impl ToolCalls {
    fn apply(&mut self, fragment: DeltaToolCall) -> Result<(), String> {
        let slot = match fragment.index {
            Some(index) => {
                let index = index as usize;
                if index >= MAX_TOOL_CALLS {
                    return Err(format!("at most {MAX_TOOL_CALLS} tool calls per answer"));
                }
                while self.by_index.len() <= index {
                    self.by_index.push(PartialCall::default());
                }
                &mut self.by_index[index]
            }
            None => {
                let last_open = self
                    .by_index
                    .last()
                    .is_some_and(|last| last.open && fragment.id.is_none());
                if last_open {
                    self.by_index.last_mut().expect("checked above")
                } else {
                    if self.by_index.len() >= MAX_TOOL_CALLS {
                        return Err(format!("at most {MAX_TOOL_CALLS} tool calls per answer"));
                    }
                    self.by_index.push(PartialCall::default());
                    self.by_index.last_mut().expect("pushed above")
                }
            }
        };
        if let Some(id) = fragment.id {
            if id.len() > MAX_ID_BYTES {
                return Err("a tool call id is implausibly long".into());
            }
            slot.id = id;
            slot.open = true;
        }
        let Some(function) = fragment.function else {
            return Ok(());
        };
        if let Some(name) = function.name {
            if name.len() > MAX_NAME_BYTES {
                return Err("a tool name is implausibly long".into());
            }
            slot.name = name;
        }
        if let Some(arguments) = function.arguments {
            if slot.arguments.len() + arguments.len() > MAX_MESSAGE_BYTES {
                return Err("the arguments of one tool call are implausibly large".into());
            }
            slot.arguments.push_str(&arguments);
        }
        Ok(())
    }

    fn finish(self) -> Vec<FinishedCall> {
        self.by_index
            .into_iter()
            .filter(|call| !call.name.is_empty())
            .map(|call| FinishedCall {
                id: if call.id.is_empty() {
                    // Some local servers omit ids; the panel's own result must
                    // still name the call it answers, so mint a stable one.
                    format!("call-{}", uuid::Uuid::new_v4())
                } else {
                    call.id
                },
                name: call.name,
                arguments: if call.arguments.trim().is_empty() {
                    "{}".into()
                } else {
                    call.arguments
                },
            })
            .collect()
    }
}

/// What a whole (non-streaming) answer turned out to be.
enum WholeReply {
    Text(String),
    Refusal(String),
}

fn whole_reply(body: &str) -> WholeReply {
    #[derive(Deserialize)]
    struct Completion {
        choices: Vec<WholeChoice>,
    }
    #[derive(Deserialize)]
    struct WholeChoice {
        message: WholeMessage,
    }
    #[derive(Deserialize)]
    struct WholeMessage {
        content: Option<String>,
        #[serde(default)]
        tool_calls: Vec<Value>,
    }
    match serde_json::from_str::<Completion>(body) {
        Ok(completion) => match completion.choices.into_iter().next() {
            Some(choice) if !choice.message.tool_calls.is_empty() => WholeReply::Refusal(
                "the model asked for a tool call, but its server answered without streaming; \
                 the assistant needs a streaming endpoint"
                    .into(),
            ),
            Some(choice) => choice
                .message
                .content
                .map(WholeReply::Text)
                .unwrap_or_else(|| {
                    WholeReply::Refusal("the model server answered without content".into())
                }),
            None => WholeReply::Refusal("the model server answered without content".into()),
        },
        Err(_) => WholeReply::Refusal(
            body_error(body).unwrap_or_else(|| "the model server answered without content".into()),
        ),
    }
}

fn body_error(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Wrapped {
        error: ServerError,
    }
    serde_json::from_str::<Wrapped>(body).ok()?.error.message
}

/// The server's own words for a 4xx/5xx, from its OpenAI-shaped error body
/// when it has one and from its raw text otherwise.
fn server_refusal(status: u16, body: &str) -> String {
    #[derive(Deserialize)]
    struct WrappedOnly {
        error: ServerError,
    }
    let detail = serde_json::from_str::<WrappedOnly>(body)
        .ok()
        .and_then(|wrapped| wrapped.error.message)
        .filter(|message: &String| !message.trim().is_empty());
    match detail {
        Some(message) => format!("the model server said {status}: {message}"),
        None if body.trim().is_empty() => format!("the model server answered {status}"),
        None => format!(
            "the model server said {status}: {}",
            body.trim().chars().take(400).collect::<String>()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: Some(content.into()),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    fn settings(base: &str) -> ChatSettings {
        ChatSettings {
            base_url: base.into(),
            model: "test-model".into(),
            api_key: String::new(),
        }
    }

    #[test]
    fn a_base_url_becomes_the_two_endpoints_it_implies() {
        assert_eq!(
            completions_url("http://x/v1").unwrap(),
            "http://x/v1/chat/completions"
        );
        assert_eq!(
            completions_url("http://x/v1/").unwrap(),
            "http://x/v1/chat/completions"
        );
        assert_eq!(
            completions_url("http://x/v1/chat/completions").unwrap(),
            "http://x/v1/chat/completions"
        );
        assert_eq!(models_url("http://x/v1").unwrap(), "http://x/v1/models");
        assert_eq!(
            models_url("http://x/v1/chat/completions").unwrap(),
            "http://x/v1/models"
        );
        assert!(completions_url("ftp://x/v1").is_err());
        assert!(completions_url("http://").is_err());
    }

    #[test]
    fn prepare_refuses_before_a_socket_what_could_never_be_a_request() {
        assert!(prepare(&settings("http://x/v1"), &[], None).is_err());
        let long = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert!(prepare(&settings("http://x/v1"), &[message("user", &long)], None).is_err());
        assert!(
            prepare(
                &settings("http://x/v1"),
                &[ChatMessage {
                    role: "tool".into(),
                    content: Some("ok".into()),
                    tool_call_id: None,
                    tool_calls: None
                }],
                None
            )
            .is_err()
        );
        let empty_model = ChatSettings {
            model: "  ".into(),
            ..settings("http://x/v1")
        };
        assert!(prepare(&empty_model, &[message("user", "hi")], None).is_err());
        // A good request prepares, and asks for the usage line by default.
        let prepared = prepare(&settings("http://x/v1"), &[message("user", "hi")], None).unwrap();
        assert_eq!(prepared.url, "http://x/v1/chat/completions");
        assert_eq!(
            prepared.body["stream_options"]["include_usage"],
            json!(true)
        );
    }

    #[test]
    fn a_wire_message_omits_what_it_does_not_carry() {
        let with_calls = ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_call_id: None,
            tool_calls: Some(vec![
                json!({"id": "c1", "function": {"name": "list_panes", "arguments": "{}"}}),
            ]),
        };
        assert_eq!(
            wire_message(&with_calls),
            json!({"role": "assistant", "tool_calls": [{"id": "c1", "function": {"name": "list_panes", "arguments": "{}"}}]})
        );
        assert_eq!(
            wire_message(&message("user", "hi")),
            json!({"role": "user", "content": "hi"})
        );
    }

    #[test]
    fn tool_call_fragments_reassemble_in_index_order_and_mint_missing_ids() {
        let fragment = |index: Option<u32>,
                        id: Option<&str>,
                        name: Option<&str>,
                        arguments: &str| DeltaToolCall {
            index,
            id: id.map(str::to_owned),
            function: Some(DeltaFunction {
                name: name.map(str::to_owned),
                arguments: Some(arguments.to_owned()),
            }),
        };
        let mut calls = ToolCalls::default();
        calls
            .apply(fragment(Some(1), None, Some("read_pane"), ""))
            .unwrap();
        calls
            .apply(fragment(Some(0), Some("a"), Some("send_to_pane"), "{\"te"))
            .unwrap();
        calls
            .apply(fragment(Some(0), None, None, "xt\":\"ls\"}"))
            .unwrap();
        calls.apply(fragment(Some(1), None, None, "{}")).unwrap();
        let finished = calls.finish();
        assert_eq!(finished.len(), 2);
        assert_eq!(finished[0].name, "send_to_pane");
        assert_eq!(finished[0].id, "a");
        assert_eq!(finished[0].arguments, "{\"text\":\"ls\"}");
        // A slot with a name but no id is still a call; its id gets minted.
        assert_eq!(finished[1].name, "read_pane");
        assert!(finished[1].id.starts_with("call-"));
        // An unnamed slot is noise, not a call. Fragments without an index
        // join the last slot only once an id has opened it; a missing id gets
        // minted.
        let mut named = ToolCalls::default();
        named
            .apply(fragment(None, Some("q"), Some("list_panes"), ""))
            .unwrap();
        named.apply(fragment(None, None, None, "more")).unwrap();
        let finished = named.finish();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].id, "q");
        assert_eq!(finished[0].arguments, "more");
        // Empty arguments echo back as an empty object, which parses.
        let mut empty = ToolCalls::default();
        empty
            .apply(fragment(Some(0), Some("z"), Some("list_panes"), "  "))
            .unwrap();
        assert_eq!(empty.finish()[0].arguments, "{}");
    }

    #[test]
    fn a_stream_is_read_into_deltas_usage_and_whole_calls() {
        let (sender, mut receiver) = mpsc::unbounded::<AiEvent>();
        let cancel = AtomicBool::new(false);
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n",
            ": keep-alive comment\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c\",\"function\":{\"name\":\"list_panes\",\"arguments\":\"{}\"}}]}}]}\n",
            "data: {\"choices\":[],\"usage\":{\"completion_tokens\":7}}\n",
            "data: [DONE]\n",
        );
        let mut body_reader = body.as_bytes();
        stream_reply(&mut body_reader, &sender, &cancel).unwrap();
        drop(sender);
        let mut events = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            events.push(event);
        }
        assert!(matches!(&events[0], AiEvent::Delta(text) if text == "hel"));
        assert!(matches!(&events[1], AiEvent::Delta(text) if text == "lo"));
        assert!(matches!(
            &events[2],
            AiEvent::Usage {
                completion_tokens: 7
            }
        ));
        assert!(
            matches!(&events[3], AiEvent::ToolCall { id, name, arguments } if id == "c" && name == "list_panes" && arguments == "{}")
        );
        assert_eq!(events.len(), 4);
    }

    #[test]
    fn a_stream_that_ends_with_neither_text_nor_calls_refuses() {
        let (sender, mut receiver) = mpsc::unbounded::<AiEvent>();
        let cancel = AtomicBool::new(false);
        let mut empty_stream = b"data: {\"choices\":[]}\n".as_slice();
        let error = stream_reply(&mut empty_stream, &sender, &cancel).unwrap_err();
        assert!(error.contains("without content"));
        assert!(matches!(receiver.try_recv(), Ok(AiEvent::Error(_))));
        // A refusal inside the stream is the server's own words.
        let cancel = AtomicBool::new(false);
        let mut refusal_stream =
            b"data: {\"error\":{\"message\":\"model not loaded\"}}\n".as_slice();
        let error = stream_reply(&mut refusal_stream, &sender, &cancel).unwrap_err();
        assert_eq!(error, "the model server refused: model not loaded");
    }

    #[test]
    fn a_cancelled_read_stops_quietly() {
        let (sender, _receiver) = mpsc::unbounded::<AiEvent>();
        let cancel = AtomicBool::new(true);
        let mut nothing = b"".as_slice();
        assert!(stream_reply(&mut nothing, &sender, &cancel).is_ok());
    }

    #[test]
    fn a_model_list_keeps_usable_ids_sorted_and_drops_repeats() {
        let body = r#"{"data":[{"id":"zeta"},{"id":"Alpha"},{"id":"zeta"},{"id":""},{"nope":1}]}"#;
        assert_eq!(model_ids(body).unwrap(), ["Alpha", "zeta"]);
        assert!(model_ids("<html>port 3000</html>").is_err());
        assert!(model_ids(r#"{"error":{"message":"no route"}}"#).is_err());
    }

    #[test]
    fn a_refusal_carries_the_servers_own_words() {
        assert_eq!(
            server_refusal(401, r#"{"error":{"message":"invalid key"}}"#),
            "the model server said 401: invalid key"
        );
        assert_eq!(server_refusal(502, ""), "the model server answered 502");
        assert_eq!(
            server_refusal(500, "bad gateway"),
            "the model server said 500: bad gateway"
        );
    }

    #[test]
    fn a_whole_answer_forwards_text_and_names_tool_calls_unsupported() {
        assert!(matches!(
            whole_reply(r#"{"choices":[{"message":{"content":"hi"}}],"usage":{"completion_tokens":3}}"#),
            WholeReply::Text(text) if text == "hi"
        ));
        assert_eq!(
            reported_tokens(r#"{"choices":[],"usage":{"completion_tokens":3}}"#),
            Some(3)
        );
        assert!(matches!(
            whole_reply(r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"c"}]}}]}"#),
            WholeReply::Refusal(message) if message.contains("without streaming")
        ));
    }
}
