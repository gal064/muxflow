//! The right panel's AI tab: OpenAI-compatible chat with tools, proxied.
//!
//! The webview's content security policy lets it speak only to this process
//! (`connect-src ipc:`), so a request to a model server cannot be made from
//! the frontend — it happens here, the same lane `update_check` uses for the
//! release manifest. The endpoint is whatever the user configured in Settings
//! → Assistant; only the shape of the call is fixed: the OpenAI
//! `/chat/completions` contract, which local servers (Ollama, MLX, vLLM,
//! llama.cpp, LM Studio) and hosted APIs both answer.
//!
//! The reply streams: deltas are pushed to the frontend over a channel as
//! the server produces them, so an answer appears while it is still being
//! generated. Tool calls are the one thing this module does not pass through
//! as they arrive: the server streams them in fragments, and a half-parsed
//! action is worse than none, so the fragments are accumulated and one
//! complete `toolCall` event is sent per call when the answer ends. Whether
//! any of it is *executed* is entirely the panel's decision — this module
//! only reports what the model asked for. `ai_cancel` stops a generation the
//! user has walked away from; a closed channel ends the read loop on its own,
//! because a send that cannot land means nobody is listening anymore.

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

/// A chat turn as the panel assembled it. Roles are the OpenAI set, tools
/// included: an assistant turn may carry `tool_calls` instead of text, and a
/// `tool` turn answers one call by id.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    role: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_call_id: Option<String>,
    /// The OpenAI-shaped call echo (`{id, function: {name, arguments}}`),
    /// passed through untouched: the panel built it from a call this module
    /// reported, and the server must see its own shape back.
    #[serde(default)]
    tool_calls: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiChatRequest {
    request_id: u64,
    base_url: String,
    #[serde(default)]
    api_key: String,
    model: String,
    messages: Vec<ChatMessage>,
    /// The tool table, in the OpenAI shape, passed through untouched; the
    /// panel owns what the assistant may ask for.
    #[serde(default)]
    tools: Option<Vec<serde_json::Value>>,
}

/// One streamed event: text, a finished tool call, the end, or a refusal.
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AiEvent {
    Delta {
        text: String,
    },
    /// A complete tool call the model asked for: `arguments` is the raw
    /// argument JSON the server streamed, for the panel to parse and run.
    ToolCall {
        id: String,
        name: String,
        arguments: String,
    },
    Done,
    Error {
        message: String,
    },
}

/// In-flight generations by request id, so `ai_cancel` can reach one.
#[derive(Clone, Default)]
pub struct AiRequests(Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>);

/// The longest the panel's system prompt or a pasted question may be. The
/// context snapshot is a few kilobytes; anything far past that is a mistake
/// or an attempt to make the app exfiltrate a document through a model.
const MAX_MESSAGE_BYTES: usize = 256 * 1024;
/// The panel keeps a short conversation; a longer history is not a chat.
const MAX_MESSAGES: usize = 64;
const MAX_MODEL_BYTES: usize = 256;
const MAX_URL_BYTES: usize = 2_048;
const MAX_TOOL_CALLS: usize = 8;
const MAX_ID_BYTES: usize = 128;
const MAX_NAME_BYTES: usize = 64;
/// Models one server may report before the list stops being a list. Past this
/// the answer is a mistake or an attack on the Settings dialog's patience.
const MAX_MODELS: usize = 512;
/// A model list is a table of ids; anything near this size is not one.
const MAX_LIST_BYTES: u64 = 4 * 1024 * 1024;
/// A non-streaming server's whole answer, read in one piece.
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Time to wait for the response *headers*; the body may stream far longer.
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(120);

/// Ask the configured model, streaming the answer into `on_event`. The
/// command itself answers as soon as the generation ends; the panel renders
/// from the events and treats this result as belt-and-braces.
#[tauri::command]
pub async fn ai_chat(
    request: AiChatRequest,
    on_event: tauri::ipc::Channel<AiEvent>,
    requests: State<'_, AiRequests>,
) -> Result<(), String> {
    let prepared = prepare(request)?;
    let request_id = prepared.request_id;
    let cancel = Arc::new(AtomicBool::new(false));
    let registry = requests.0.clone();
    registry
        .lock()
        .map_err(|_| "the request registry is gone".to_owned())?
        .insert(request_id, cancel.clone());
    tauri::async_runtime::spawn_blocking(move || {
        let result = run(prepared, &on_event, &cancel);
        if let Ok(mut live) = registry.lock() {
            live.remove(&request_id);
        }
        // The read loop ends without a terminal event; the panel's own `done`
        // promise needs one whichever way the answer ended.
        if result.is_ok() {
            let _ = on_event.send(AiEvent::Done);
        }
        result
    })
    .await
    .map_err(|error| format!("the assistant call could not run: {error}"))?
}

/// The endpoint and key to ask for its model list. The same pair the panel
/// chats with: the list exists to fill the model field, so a second source of
/// endpoints would only let the two disagree.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiModelsRequest {
    base_url: String,
    #[serde(default)]
    api_key: String,
}

/// Ask the configured server which models it serves, for Settings → Assistant
/// to offer instead of a hand-typed id. The OpenAI `GET /models` contract,
/// which the local servers answer as readily as the chat endpoint.
#[tauri::command]
pub async fn ai_models(request: AiModelsRequest) -> Result<Vec<String>, String> {
    if request.base_url.len() > MAX_URL_BYTES {
        return Err("the assistant endpoint is implausibly long".into());
    }
    let url = models_url(&request.base_url)?;
    let api_key = request.api_key;
    tauri::async_runtime::spawn_blocking(move || list_models(&url, api_key.trim()))
        .await
        .map_err(|error| format!("the model list could not be fetched: {error}"))?
}

/// Stop the generation behind one request id. Unknown ids are ignored: the
/// panel may cancel after the answer already landed.
#[tauri::command]
pub fn ai_cancel(request_id: u64, requests: State<'_, AiRequests>) {
    if let Ok(live) = requests.0.lock()
        && let Some(cancel) = live.get(&request_id)
    {
        cancel.store(true, Ordering::Relaxed);
    }
}

struct Prepared {
    request_id: u64,
    url: String,
    body: serde_json::Value,
    api_key: String,
}

fn prepare(request: AiChatRequest) -> Result<Prepared, String> {
    if request.messages.is_empty() || request.messages.len() > MAX_MESSAGES {
        return Err(format!("between 1 and {MAX_MESSAGES} messages are allowed"));
    }
    for message in &request.messages {
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
    if request.model.trim().is_empty() || request.model.len() > MAX_MODEL_BYTES {
        return Err("set a model in Settings → Assistant".into());
    }
    if request.base_url.len() > MAX_URL_BYTES {
        return Err("the assistant endpoint is implausibly long".into());
    }
    let tools = match &request.tools {
        Some(tools) if !tools.is_empty() => {
            if tools.len() > 16 {
                return Err("at most 16 tools may be offered".into());
            }
            let size = serde_json::to_string(tools).map_err(|_| "unreadable tools".to_owned())?;
            if size.len() > MAX_MESSAGE_BYTES {
                return Err("the tool table is implausibly large".into());
            }
            Some(tools.clone())
        }
        _ => None,
    };
    let url = completions_url(&request.base_url)?;
    let mut body = serde_json::json!({
        "model": request.model.trim(),
        "messages": request.messages.iter().map(wire_message).collect::<Vec<_>>(),
        "stream": true,
    });
    if let Some(tools) = tools {
        body["tools"] = serde_json::Value::Array(tools);
    }
    Ok(Prepared {
        request_id: request.request_id,
        url,
        body,
        api_key: request.api_key,
    })
}

/// One message in the shape the *server* expects — snake_case, with absent
/// fields absent rather than null, which is what lets an assistant turn carry
/// tool calls and no content.
fn wire_message(message: &ChatMessage) -> serde_json::Value {
    let mut value = serde_json::json!({ "role": message.role });
    if let Some(content) = &message.content {
        value["content"] = serde_json::Value::String(content.clone());
    }
    if let Some(tool_call_id) = &message.tool_call_id {
        value["tool_call_id"] = serde_json::Value::String(tool_call_id.clone());
    }
    if let Some(calls) = &message.tool_calls {
        value["tool_calls"] = serde_json::Value::Array(calls.clone());
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
/// A base pasted as the full chat endpoint is rewritten to the equivalent
/// list endpoint, so the one field a user fills answers both calls.
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
fn list_models(url: &str, api_key: &str) -> Result<Vec<String>, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(Duration::from_secs(20)))
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
/// staring at a "no models" line cannot tell an empty server from a wrong
/// port serving HTML.
fn model_ids(body: &str) -> Result<Vec<String>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "the model server's reply was not a model list")?;
    let entries = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or("the model server's reply carried no model list")?;
    let mut ids: Vec<String> = Vec::new();
    for entry in entries {
        if ids.len() >= MAX_MODELS {
            break;
        }
        let Some(id) = entry.get("id").and_then(serde_json::Value::as_str) else {
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
    ids.sort_by(|left, right| left.to_lowercase().cmp(&right.to_lowercase()));
    Ok(ids)
}

fn run(
    prepared: Prepared,
    on_event: &tauri::ipc::Channel<AiEvent>,
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
    let mut request = agent.post(&prepared.url);
    if !prepared.api_key.trim().is_empty() {
        request = request.header(
            "authorization",
            format!("Bearer {}", prepared.api_key.trim()),
        );
    }
    // The body is sent as text with the JSON content type rather than through
    // `send_json`, which ureq keeps behind its `json` feature; the body is
    // already a `serde_json::Value`'s string.
    let mut response = match request
        .header("content-type", "application/json")
        .send(prepared.body.to_string())
    {
        Ok(response) => response,
        Err(error) => {
            let message = format!("the model server could not be reached: {error}");
            let _ = on_event.send(AiEvent::Error {
                message: message.clone(),
            });
            return Err(message);
        }
    };
    if !response.status().is_success() {
        let body = response
            .body_mut()
            .with_config()
            .limit(16 * 1024)
            .read_to_string()
            .unwrap_or_default();
        let message = server_refusal(response.status().as_u16(), &body);
        let _ = on_event.send(AiEvent::Error {
            message: message.clone(),
        });
        return Err(message);
    }
    let streaming = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"));
    if streaming {
        let mut reader = BufReader::new(response.body_mut().as_reader());
        return stream_reply(&mut reader, on_event, cancel);
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
            if on_event
                .send(AiEvent::Delta { text })
                .map_err(|_| "the panel closed mid-answer")
                .is_err()
            {
                return Err("the panel closed mid-answer".into());
            }
            Ok(())
        }
        WholeReply::Refusal(message) => {
            let _ = on_event.send(AiEvent::Error {
                message: message.clone(),
            });
            Err(message)
        }
    }
}

/// Read the body as SSE and forward each delta; accumulate tool-call
/// fragments and report the complete calls only once the stream ends.
fn stream_reply(
    reader: &mut impl BufRead,
    on_event: &tauri::ipc::Channel<AiEvent>,
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut saw_content = false;
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
                let _ = on_event.send(AiEvent::Error {
                    message: message.clone(),
                });
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
            let _ = on_event.send(AiEvent::Error {
                message: message.clone(),
            });
            return Err(message);
        }
        for choice in chunk.choices {
            if let Some(text) = choice
                .delta
                .as_ref()
                .and_then(|delta| delta.content.as_ref())
                .filter(|text| !text.is_empty())
            {
                saw_content = true;
                if on_event
                    .send(AiEvent::Delta { text: text.clone() })
                    .is_err()
                {
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
                    let _ = on_event.send(AiEvent::Error {
                        message: message.clone(),
                    });
                    return Err(message);
                }
            }
        }
    }
    let complete = calls.finish();
    if !complete.is_empty() {
        // The calls are reported after the text they followed, in the order
        // the model asked for them.
        for call in complete {
            if on_event
                .send(AiEvent::ToolCall {
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
    let _ = on_event.send(AiEvent::Error {
        message: message.clone(),
    });
    Err(message)
}

#[derive(Deserialize)]
struct StreamChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    error: Option<ServerError>,
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
        tool_calls: Vec<serde_json::Value>,
    }
    let completion: Result<Completion, _> = serde_json::from_str(body);
    match completion {
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

#[derive(Deserialize)]
struct WrappedOnly {
    error: ServerError,
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

    #[test]
    fn a_versioned_base_becomes_the_chat_completions_endpoint() {
        assert_eq!(
            completions_url("http://127.0.0.1:11434/v1").unwrap(),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(
            completions_url("https://api.openai.com/v1/").unwrap(),
            "https://api.openai.com/v1/chat/completions"
        );
    }

    #[test]
    fn a_full_endpoint_passes_through_untouched() {
        assert_eq!(
            completions_url("https://proxy.example/v1/chat/completions").unwrap(),
            "https://proxy.example/v1/chat/completions"
        );
    }

    #[test]
    fn only_http_and_https_reach_a_server() {
        for base in ["ftp://host/v1", "host:11434/v1", "", "/", "https://"] {
            assert!(completions_url(base).is_err(), "{base} must be refused");
        }
    }

    #[test]
    fn the_same_base_serves_the_chat_and_the_list_endpoints() {
        assert_eq!(
            models_url("http://127.0.0.1:11434/v1").unwrap(),
            "http://127.0.0.1:11434/v1/models"
        );
        // A base pasted as the full chat endpoint still finds the list beside it.
        assert_eq!(
            models_url("https://api.openai.com/v1/chat/completions").unwrap(),
            "https://api.openai.com/v1/models"
        );
        for base in ["ftp://host/v1", "host:11434/v1", ""] {
            assert!(models_url(base).is_err(), "{base} must be refused");
        }
    }

    #[test]
    fn a_model_list_yields_its_ids_and_survives_one_odd_row() {
        let ids = model_ids(
            r#"{"object":"list","data":[
                {"id":"llama3","object":"model"},
                {"id":"ZEPHYR","object":"model"},
                {"id":"llama3","object":"model"},
                {"id":"","object":"model"},
                {"id":42,"object":"model"},
                {"object":"model"}
            ]}"#,
        )
        .unwrap();
        // Duplicates and unusable rows are gone; the rest sort for scanning.
        assert_eq!(ids, ["llama3", "ZEPHYR"]);
    }

    #[test]
    fn an_empty_list_is_answered_and_an_unlisted_body_refused() {
        assert_eq!(
            model_ids(r#"{"object":"list","data":[]}"#).unwrap(),
            Vec::<String>::new()
        );
        // HTML from the wrong port must not read as "the server has no models".
        assert!(
            model_ids("<html>")
                .unwrap_err()
                .contains("not a model list")
        );
        assert!(
            model_ids(r#"{"error":"nope"}"#)
                .unwrap_err()
                .contains("carried no model list")
        );
    }

    #[test]
    fn the_wire_shape_is_the_servers_snake_case() {
        let call =
            serde_json::json!({"id": "c1", "function": {"name": "list_panes", "arguments": "{}"}});
        let assistant = wire_message(&ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_call_id: None,
            tool_calls: Some(vec![call.clone()]),
        });
        assert_eq!(assistant["tool_calls"][0]["function"]["name"], "list_panes");
        assert!(assistant.get("content").is_none());
        let answer = wire_message(&ChatMessage {
            role: "tool".into(),
            content: Some("two panes".into()),
            tool_call_id: Some("c1".into()),
            tool_calls: None,
        });
        assert_eq!(answer["tool_call_id"], "c1");
        assert_eq!(answer["content"], "two panes");
    }

    #[test]
    fn a_stream_chunk_yields_its_delta_and_ignores_the_rest() {
        let chunk: StreamChunk = serde_json::from_str(
            r#"{"choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}],"usage":null}"#,
        )
        .unwrap();
        assert_eq!(
            chunk.choices[0].delta.as_ref().unwrap().content.as_deref(),
            Some("hi")
        );
        let error: StreamChunk =
            serde_json::from_str(r#"{"error":{"message":"model not loaded"}}"#).unwrap();
        assert_eq!(error.error.unwrap().message.unwrap(), "model not loaded");
    }

    #[test]
    fn tool_call_fragments_rejoin_in_index_order() {
        let mut calls = ToolCalls::default();
        calls
            .apply(serde_json::from_str(
                r#"{"index":0,"id":"call_1","function":{"name":"send_to_pane","arguments":"{\"te"}}"#,
            )
            .unwrap())
            .unwrap();
        calls
            .apply(
                serde_json::from_str(
                    r#"{"index":0,"function":{"arguments":"xt\":\"git status\"}"}}"#,
                )
                .unwrap(),
            )
            .unwrap();
        calls
            .apply(
                serde_json::from_str(
                    r#"{"index":1,"id":"call_2","function":{"name":"list_panes","arguments":""}}"#,
                )
                .unwrap(),
            )
            .unwrap();
        let done = calls.finish();
        assert_eq!(done.len(), 2);
        assert_eq!(done[0].id, "call_1");
        assert_eq!(done[0].name, "send_to_pane");
        assert_eq!(done[0].arguments, r#"{"text":"git status"}"#);
        assert_eq!(done[1].name, "list_panes");
        // An empty argument list is the OpenAI spelling of "no arguments".
        assert_eq!(done[1].arguments, "{}");
    }

    #[test]
    fn fragments_without_an_index_rejoin_in_arrival_order() {
        let mut calls = ToolCalls::default();
        for line in [
            r#"{"id":"a","function":{"name":"list_panes","arguments":"{}"}}"#,
            r#"{"function":{"arguments":""}}"#,
        ] {
            calls.apply(serde_json::from_str(line).unwrap()).unwrap();
        }
        let done = calls.finish();
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].id, "a");
    }

    #[test]
    fn a_minted_id_survives_a_server_that_sends_none() {
        let mut calls = ToolCalls::default();
        calls
            .apply(
                serde_json::from_str(
                    r#"{"index":0,"function":{"name":"list_panes","arguments":"{}"}}"#,
                )
                .unwrap(),
            )
            .unwrap();
        let done = calls.finish();
        assert_eq!(done.len(), 1);
        assert!(!done[0].id.is_empty());
    }

    #[test]
    fn too_many_tool_calls_at_once_is_refused() {
        let mut calls = ToolCalls::default();
        for index in 0..(MAX_TOOL_CALLS + 1) {
            let fragment: DeltaToolCall = serde_json::from_str(&format!(
                r#"{{"index":{index},"function":{{"name":"x","arguments":"{{}}"}}}}"#
            ))
            .unwrap();
            let applied = calls.apply(fragment);
            if index >= MAX_TOOL_CALLS {
                assert!(applied.is_err(), "call {} must be refused", index);
            } else {
                applied.unwrap();
            }
        }
    }

    #[test]
    fn a_whole_answer_reads_like_a_stream() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"two files"}}]}"#;
        assert!(matches!(whole_reply(body), WholeReply::Text(text) if text == "two files"));
        assert!(matches!(whole_reply("not json"), WholeReply::Refusal(_)));
        assert!(matches!(
            whole_reply(r#"{"choices":[{"message":{"content":null,"tool_calls":[{"id":"c"}]}}]}"#),
            WholeReply::Refusal(message) if message.contains("streaming")
        ));
        assert_eq!(
            body_error(r#"{"error":{"message":"no key"}}"#).as_deref(),
            Some("no key")
        );
    }

    #[test]
    fn a_request_over_the_limits_is_refused_before_the_network() {
        let base = AiChatRequest {
            request_id: 1,
            base_url: "http://127.0.0.1:11434/v1".into(),
            api_key: String::new(),
            model: "qwen".into(),
            messages: vec![message("user", "x")],
            tools: None,
        };
        assert!(prepare(base.clone()).is_ok());
        assert!(
            prepare(AiChatRequest {
                messages: vec![],
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            prepare(AiChatRequest {
                messages: vec![message("tool", "x")],
                ..base.clone()
            })
            .is_err()
        ); // a tool answer with no call id is not a coherent history
        assert!(
            prepare(AiChatRequest {
                messages: vec![ChatMessage {
                    role: "tool".into(),
                    content: Some("done".into()),
                    tool_call_id: Some("c1".into()),
                    tool_calls: None,
                }],
                ..base.clone()
            })
            .is_ok()
        );
        assert!(
            prepare(AiChatRequest {
                messages: vec![message("tool", "x").clone()],
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            prepare(AiChatRequest {
                model: "".into(),
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            prepare(AiChatRequest {
                messages: vec![message("user", &"x".repeat(MAX_MESSAGE_BYTES + 1))],
                ..base.clone()
            })
            .is_err()
        );
        assert!(
            prepare(AiChatRequest {
                tools: Some(vec![serde_json::json!({"type": "function"})]),
                ..base
            })
            .is_ok()
        );
    }
}
