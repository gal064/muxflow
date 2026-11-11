//! A document's reads and writes on the host's bulk lane: the editor half of
//! `features/files/api.ts` (`openFile`, `writeText` and the file-I/O frame
//! reader behind them).
//!
//! `desktop-core` reports from its own threads, so a [`Reply`] is filled there
//! and read by a task that polls it on a timer, never woken from that thread.

use std::sync::{Arc, Mutex};

use desktop_core::platform::{EventBody, EventSink};
use serde::Deserialize;

use super::autosave::{LineEnding, SaveRequest};
use crate::connection_ui::backend::TerminalBackend;

/// The editor's limit on a text file, as the host's.
pub const TEXT_FILE_LIMIT_BYTES: u64 = 10 * 1024 * 1024;
/// The largest file whose bytes are fetched for an image preview.
pub const IMAGE_PREVIEW_LIMIT_BYTES: u64 = 25 * 1024 * 1024;

/// The live connection a document reads and writes through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileScope {
    pub client_id: String,
    pub profile_id: String,
    pub server_identity: String,
    pub terminal_epoch: u64,
}

/// The host-issued root a document was opened under. A terminal-linked file
/// outside the pane's root carries a capability for exactly that file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub path: String,
    pub token: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextFile {
    pub content: String,
    pub generation: String,
    pub size_bytes: u64,
    pub line_ending: LineEnding,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BinaryFile {
    pub path: String,
    pub generation: String,
    pub size_bytes: u64,
    pub mime: String,
    pub image: bool,
    /// The bytes, when the host streamed them for a preview.
    pub preview: Option<Arc<[u8]>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Opened {
    Text(TextFile),
    Binary(BinaryFile),
}

/// One editor transfer for the backend to start.
pub struct FileIoRequest {
    pub scope: FileScope,
    pub root: Root,
    pub path: String,
    /// `None` reads; otherwise the bytes to write, on top of what.
    pub write: Option<FileWrite>,
}

pub struct FileWrite {
    pub operation_id: String,
    pub base_generation: String,
    pub content: Vec<u8>,
}

/// Where an answer lands, filled in by the thread that has it.
pub type Slot<T> = Arc<Mutex<Option<Result<T, String>>>>;

/// An answer another thread fills in. Dropping it before the answer cancels
/// the transfer on the host.
pub struct Reply<T> {
    slot: Slot<T>,
    cancel: Option<Box<dyn FnOnce()>>,
}

impl<T> Reply<T> {
    pub fn ready(result: Result<T, String>) -> Self {
        Self {
            slot: Arc::new(Mutex::new(Some(result))),
            cancel: None,
        }
    }

    /// An answer a test fills in through the returned slot, and whether the
    /// transfer was cancelled.
    #[cfg(test)]
    pub fn pending() -> (Self, Slot<T>, std::rc::Rc<std::cell::Cell<bool>>) {
        let slot = Arc::new(Mutex::new(None));
        let cancelled = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = cancelled.clone();
        (
            Self {
                slot: slot.clone(),
                cancel: Some(Box::new(move || flag.set(true))),
            },
            slot,
            cancelled,
        )
    }

    /// The answer, once it has arrived.
    pub fn take(&mut self) -> Option<Result<T, String>> {
        let answer = self.slot.lock().ok()?.take();
        if answer.is_some() {
            self.cancel = None;
        }
        answer
    }
}

impl<T> Drop for Reply<T> {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel();
        }
    }
}

/// Where a document's file I/O goes. A seam, so tests run without a host.
pub trait FileIo {
    fn read(&self, scope: &FileScope, root: &Root, path: &str) -> Reply<Opened>;
    /// Answers with the generation the write produced.
    fn write(
        &self,
        scope: &FileScope,
        root: &Root,
        path: &str,
        save: &SaveRequest,
    ) -> Reply<String>;
}

/// The real lane, through the connection's backend.
pub struct BackendIo(pub Arc<dyn TerminalBackend>);

impl BackendIo {
    fn start<T: Send + 'static>(
        &self,
        request: FileIoRequest,
        finish: fn(Completed, Vec<u8>) -> Result<T, String>,
    ) -> Reply<T> {
        let slot = Arc::new(Mutex::new(None));
        let first_content = request
            .write
            .is_none()
            .then(|| crate::perf::PaintTicket::start(&["file.timeToFirstContent"]))
            .flatten();
        let sink = Arc::new(FrameReader {
            state: Mutex::new(Frames {
                first_content,
                ..Frames::default()
            }),
            slot: slot.clone(),
            finish,
        });
        match self.0.start_file_io(request, sink) {
            Ok(transfer_id) => {
                let backend = self.0.clone();
                Reply {
                    slot,
                    cancel: Some(Box::new(move || backend.cancel_file_io(transfer_id))),
                }
            }
            Err(error) => Reply::ready(Err(error)),
        }
    }
}

impl FileIo for BackendIo {
    fn read(&self, scope: &FileScope, root: &Root, path: &str) -> Reply<Opened> {
        self.start(
            FileIoRequest {
                scope: scope.clone(),
                root: root.clone(),
                path: path.to_owned(),
                write: None,
            },
            opened,
        )
    }

    fn write(
        &self,
        scope: &FileScope,
        root: &Root,
        path: &str,
        save: &SaveRequest,
    ) -> Reply<String> {
        match write_request(scope, root, path, save) {
            Ok(request) => self.start(request, written),
            Err(error) => Reply::ready(Err(error)),
        }
    }
}

/// The write the host is asked for: the buffer in the file's own line
/// endings, on top of the generation it was read at. One over the editor's
/// limit is refused before it leaves.
fn write_request(
    scope: &FileScope,
    root: &Root,
    path: &str,
    save: &SaveRequest,
) -> Result<FileIoRequest, String> {
    let content = save
        .line_ending
        .apply(&save.content.to_string())
        .into_bytes();
    if content.len() as u64 > TEXT_FILE_LIMIT_BYTES {
        return Err("Text files larger than 10 MiB cannot be edited.".to_owned());
    }
    Ok(FileIoRequest {
        scope: scope.clone(),
        root: root.clone(),
        path: path.to_owned(),
        write: Some(FileWrite {
            operation_id: save.operation_id.clone(),
            base_generation: save.generation.clone(),
            content,
        }),
    })
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub path: String,
    #[serde(default)]
    pub size: String,
    #[serde(default)]
    pub generation: String,
    #[serde(default)]
    pub mime: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FrameEvent {
    #[serde(default)]
    state: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    metadata: Option<Metadata>,
    #[serde(default)]
    content_kind: Option<String>,
    #[serde(default)]
    total_bytes: Option<String>,
    #[serde(default)]
    metadata_only: bool,
    #[serde(default)]
    generation: Option<String>,
}

/// What a transfer's completion said, with what earlier frames carried.
pub struct Completed {
    metadata: Option<Metadata>,
    content_kind: Option<String>,
    total_bytes: Option<String>,
    metadata_only: bool,
    generation: Option<String>,
    received: u64,
}

#[derive(Default)]
struct Frames {
    done: bool,
    bytes: Vec<u8>,
    metadata: Option<Metadata>,
    content_kind: Option<String>,
    /// A read's `file.timeToFirstContent`, ended by its first frame: the
    /// first chunk, or the completion of a file with none.
    first_content: Option<crate::perf::PaintTicket>,
}

struct FrameReader<T> {
    state: Mutex<Frames>,
    slot: Slot<T>,
    finish: fn(Completed, Vec<u8>) -> Result<T, String>,
}

impl<T: Send> FrameReader<T> {
    fn settle(&self, frames: &mut Frames, result: Result<T, String>) {
        frames.done = true;
        if let (Ok(_), Some(first)) = (&result, frames.first_content.take()) {
            first.painted();
        }
        frames.bytes = Vec::new();
        if let Ok(mut slot) = self.slot.lock() {
            *slot = Some(result);
        }
    }

    fn frame(
        &self,
        frames: &mut Frames,
        frame: &[u8],
    ) -> Result<Option<Result<T, String>>, String> {
        let (&kind, body) = frame
            .split_first()
            .ok_or("Host emitted an empty file-I/O frame.")?;
        if kind == 2 {
            let (offset, chunk) = body
                .split_first_chunk::<8>()
                .ok_or("Host emitted an invalid file chunk.")?;
            if u64::from_be_bytes(*offset) != frames.bytes.len() as u64 {
                return Err("Bulk file chunks arrived out of sequence.".into());
            }
            if (frames.bytes.len() + chunk.len()) as u64 > IMAGE_PREVIEW_LIMIT_BYTES {
                return Err("Bulk file content exceeded the 25 MiB limit.".into());
            }
            frames.bytes.extend_from_slice(chunk);
            return Ok(None);
        }
        if !(1..=4).contains(&kind) {
            return Err("Host emitted an unknown file-I/O frame.".into());
        }
        let event: FrameEvent = serde_json::from_slice(body)
            .map_err(|error| format!("Host emitted a malformed file-I/O frame: {error}"))?;
        if event.metadata.is_some() {
            frames.metadata = event.metadata.clone();
        }
        if event.content_kind.is_some() {
            frames.content_kind = event.content_kind.clone();
        }
        if kind == 4 || event.state == "error" || event.state == "cancelled" {
            let fallback = if event.state == "cancelled" {
                "File transfer cancelled."
            } else {
                "File transfer failed."
            };
            return Ok(Some(Err(event
                .error
                .filter(|error| !error.is_empty())
                .unwrap_or_else(|| fallback.to_owned()))));
        }
        if kind != 3 {
            return Ok(None);
        }
        let completed = Completed {
            metadata: frames.metadata.take(),
            content_kind: frames.content_kind.take(),
            total_bytes: event.total_bytes,
            metadata_only: event.metadata_only,
            generation: event.generation,
            received: frames.bytes.len() as u64,
        };
        Ok(Some((self.finish)(
            completed,
            std::mem::take(&mut frames.bytes),
        )))
    }
}

impl<T: Send> EventSink for FrameReader<T> {
    fn send(&self, body: EventBody) -> Result<(), String> {
        let mut frames = self.state.lock().map_err(|_| "file reader poisoned")?;
        if frames.done {
            // Settled already: tell the core nobody is reading any more.
            return Err("file transfer already settled".into());
        }
        let raw = match &body {
            EventBody::Raw(raw) => raw.as_slice(),
            EventBody::Json(json) => json.as_bytes(),
        };
        // Queued, running and the metadata header come first; content is the
        // first chunk, or the answer itself for a file with none.
        if raw.first() == Some(&2)
            && let Some(first) = frames.first_content.take()
        {
            first.painted();
        }
        match self.frame(&mut frames, raw) {
            Ok(None) => Ok(()),
            Ok(Some(result)) => {
                self.settle(&mut frames, result);
                Ok(())
            }
            Err(error) => {
                self.settle(&mut frames, Err(error.clone()));
                Err(error)
            }
        }
    }
}

fn opened(completed: Completed, bytes: Vec<u8>) -> Result<Opened, String> {
    let metadata = completed
        .metadata
        .ok_or("Host omitted file content metadata.")?;
    let kind = completed
        .content_kind
        .ok_or("Host omitted file content metadata.")?;
    let total = match &completed.total_bytes {
        Some(total) => total
            .parse::<u64>()
            .map_err(|_| "Host returned a malformed byte count.")?,
        None => completed.received,
    };
    if !completed.metadata_only && total != completed.received {
        return Err("Bulk file byte count verification failed.".into());
    }
    let generation = completed
        .generation
        .filter(|generation| !generation.is_empty())
        .unwrap_or_else(|| metadata.generation.clone());
    let size_bytes = metadata.size.parse::<u64>().unwrap_or(0);
    if kind == "text" && !completed.metadata_only {
        if completed.received > TEXT_FILE_LIMIT_BYTES {
            return Err("Bulk file content exceeded the 10 MiB limit.".into());
        }
        let content =
            String::from_utf8(bytes).map_err(|_| "Host returned invalid UTF-8 for a text file.")?;
        return Ok(Opened::Text(TextFile {
            line_ending: LineEnding::detect(&content),
            content,
            generation,
            size_bytes,
        }));
    }
    let mime = if metadata.mime.is_empty() {
        "application/octet-stream".to_owned()
    } else {
        metadata.mime.clone()
    };
    Ok(Opened::Binary(BinaryFile {
        path: metadata.path,
        generation,
        size_bytes,
        mime,
        image: kind == "image",
        preview: (!completed.metadata_only).then(|| Arc::from(bytes)),
    }))
}

fn written(completed: Completed, _: Vec<u8>) -> Result<String, String> {
    completed
        .generation
        .filter(|generation| !generation.is_empty())
        .or_else(|| completed.metadata.map(|metadata| metadata.generation))
        .filter(|generation| !generation.is_empty())
        .ok_or_else(|| "Host omitted saved file metadata.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::autosave::{Autosave, Snapshot};

    fn reader<T: Send>(
        finish: fn(Completed, Vec<u8>) -> Result<T, String>,
    ) -> (FrameReader<T>, Slot<T>) {
        let slot = Arc::new(Mutex::new(None));
        (
            FrameReader {
                state: Mutex::new(Frames::default()),
                slot: slot.clone(),
                finish,
            },
            slot,
        )
    }

    fn json(kind: u8, value: serde_json::Value) -> EventBody {
        let mut frame = vec![kind];
        frame.extend(serde_json::to_vec(&value).unwrap());
        EventBody::Raw(frame)
    }

    fn chunk(offset: u64, data: &[u8]) -> EventBody {
        let mut frame = vec![2];
        frame.extend(offset.to_be_bytes());
        frame.extend(data);
        EventBody::Raw(frame)
    }

    fn metadata(size: usize) -> serde_json::Value {
        serde_json::json!({ "path": "/r/a.txt", "size": size.to_string(), "generation": "7", "mime": "" })
    }

    #[test]
    fn reads_a_text_file_from_its_chunks() {
        let (sink, slot) = reader(opened);
        let sink: &dyn EventSink = &sink;
        sink.send(json(1, serde_json::json!({ "state": "running", "metadata": metadata(9), "contentKind": "text" })))
            .unwrap();
        sink.send(chunk(0, b"one\r\n")).unwrap();
        sink.send(chunk(5, b"two\r\n")).unwrap();
        sink.send(json(
            3,
            serde_json::json!({ "state": "completed", "totalBytes": "10", "generation": "8" }),
        ))
        .unwrap();
        let Some(Ok(Opened::Text(text))) = slot.lock().unwrap().take() else {
            panic!("no text");
        };
        assert_eq!(text.content, "one\r\ntwo\r\n");
        assert_eq!(text.generation, "8");
        assert_eq!(text.line_ending, LineEnding::Crlf);
    }

    #[test]
    fn refuses_chunks_out_of_order_and_stops_listening() {
        let (sink, slot) = reader(opened);
        let sink: &dyn EventSink = &sink;
        assert!(sink.send(chunk(3, b"late")).is_err());
        assert_eq!(
            slot.lock().unwrap().take(),
            Some(Err("Bulk file chunks arrived out of sequence.".into()))
        );
        assert!(sink.send(chunk(0, b"more")).is_err());
    }

    #[test]
    fn a_short_body_fails_verification() {
        let (sink, slot) = reader(opened);
        let sink: &dyn EventSink = &sink;
        sink.send(json(
            1,
            serde_json::json!({ "metadata": metadata(4), "contentKind": "text" }),
        ))
        .unwrap();
        sink.send(chunk(0, b"ab")).unwrap();
        sink.send(json(3, serde_json::json!({ "totalBytes": "4" })))
            .unwrap();
        assert_eq!(
            slot.lock().unwrap().take(),
            Some(Err("Bulk file byte count verification failed.".into()))
        );
    }

    #[test]
    fn invalid_utf8_is_not_text() {
        let (sink, slot) = reader(opened);
        let sink: &dyn EventSink = &sink;
        sink.send(json(
            1,
            serde_json::json!({ "metadata": metadata(2), "contentKind": "text" }),
        ))
        .unwrap();
        sink.send(chunk(0, &[0xff, 0xfe])).unwrap();
        sink.send(json(3, serde_json::json!({ "totalBytes": "2" })))
            .unwrap();
        assert_eq!(
            slot.lock().unwrap().take(),
            Some(Err("Host returned invalid UTF-8 for a text file.".into()))
        );
    }

    #[test]
    fn a_binary_or_oversized_file_is_described_not_read() {
        let (sink, slot) = reader(opened);
        let sink: &dyn EventSink = &sink;
        sink.send(json(
            3,
            serde_json::json!({
                "metadata": metadata(20_000_000), "contentKind": "tooLarge", "metadataOnly": true,
            }),
        ))
        .unwrap();
        let Some(Ok(Opened::Binary(file))) = slot.lock().unwrap().take() else {
            panic!("no description");
        };
        assert_eq!(file.mime, "application/octet-stream");
        assert_eq!(file.size_bytes, 20_000_000);
        assert!(!file.image);
        assert_eq!(file.preview, None);
    }

    #[test]
    fn an_image_keeps_its_bytes_for_the_preview() {
        let (sink, slot) = reader(opened);
        let sink: &dyn EventSink = &sink;
        let mut metadata = metadata(3);
        metadata["mime"] = "image/png".into();
        sink.send(json(
            1,
            serde_json::json!({ "metadata": metadata, "contentKind": "image" }),
        ))
        .unwrap();
        sink.send(chunk(0, b"png")).unwrap();
        sink.send(json(3, serde_json::json!({ "totalBytes": "3" })))
            .unwrap();
        let Some(Ok(Opened::Binary(file))) = slot.lock().unwrap().take() else {
            panic!("no image");
        };
        assert!(file.image);
        assert_eq!(file.preview.as_deref(), Some(&b"png"[..]));
    }

    #[test]
    fn a_write_answers_with_its_generation_or_the_host_refusal() {
        let (sink, slot) = reader(written);
        let sink: &dyn EventSink = &sink;
        sink.send(json(1, serde_json::json!({ "state": "verifying" })))
            .unwrap();
        sink.send(json(3, serde_json::json!({ "state": "completed", "generation": "12", "metadata": metadata(1) })))
            .unwrap();
        assert_eq!(slot.lock().unwrap().take(), Some(Ok("12".into())));

        let (sink, slot) = reader(written);
        let sink: &dyn EventSink = &sink;
        sink.send(json(4, serde_json::json!({
            "state": "failed", "error": "stale_file_generation: file changed since it was opened",
        })))
        .unwrap();
        assert_eq!(
            slot.lock().unwrap().take(),
            Some(Err(
                "stale_file_generation: file changed since it was opened".into()
            ))
        );
    }

    #[test]
    fn dropping_a_reply_before_its_answer_cancels_the_transfer_once() {
        let (reply, _, cancelled) = Reply::<String>::pending();
        drop(reply);
        assert!(cancelled.get(), "an obsolete read stops the host's work");

        let (mut reply, slot, cancelled) = Reply::<String>::pending();
        *slot.lock().unwrap() = Some(Ok("1".into()));
        assert_eq!(reply.take(), Some(Ok("1".into())));
        drop(reply);
        assert!(!cancelled.get(), "an answered transfer has nothing to stop");
    }

    fn save_over(original: &str, edited: &str) -> SaveRequest {
        let mut autosave = Autosave::new(Snapshot {
            content: original.into(),
            generation: "7".into(),
            line_ending: LineEnding::detect(original),
        });
        autosave.edit(edited.into());
        autosave.flush().unwrap()
    }

    #[test]
    fn a_write_carries_its_scope_and_the_files_own_line_endings() {
        let scope = FileScope {
            client_id: "client-1".into(),
            profile_id: "local".into(),
            server_identity: "server-a".into(),
            terminal_epoch: 3,
        };
        let root = Root {
            path: "/repo".into(),
            token: "root-token".into(),
        };
        let save = save_over("x\r\ny", "a\nb");
        let request = write_request(&scope, &root, "/repo/a.txt", &save).unwrap();
        assert_eq!(request.scope, scope);
        assert_eq!(request.root, root);
        assert_eq!(request.path, "/repo/a.txt");
        let write = request.write.unwrap();
        assert_eq!(write.content, b"a\r\nb");
        assert_eq!(write.base_generation, "7");
        assert_eq!(write.operation_id, save.operation_id);

        let huge = "a".repeat(TEXT_FILE_LIMIT_BYTES as usize + 1);
        let refused = write_request(&scope, &root, "/repo/a.txt", &save_over("", &huge));
        assert_eq!(
            refused.err().as_deref(),
            Some("Text files larger than 10 MiB cannot be edited.")
        );
    }
}
