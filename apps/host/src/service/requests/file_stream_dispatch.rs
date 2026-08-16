use super::*;

/// Answers one `OpenFileStream` request: one descriptor-bound classification,
/// one header frame, bounded body frames, then exactly one terminal response.
///
/// Every exit through here writes a response for `request_id`, so a desktop
/// waiting on this exchange is never left holding the bulk bridge.
pub(super) async fn handle(
    request_id: u64,
    request: v1::Request,
    cancellation: Arc<AtomicBool>,
    control_tx: &mpsc::Sender<SequencerControl>,
    files: &Arc<FileService>,
) {
    let Some(file) = request.file else {
        send_response(
            control_tx,
            request_id,
            response_error("invalid_file_request", "file request payload is required"),
        )
        .await;
        return;
    };
    if let Err(error) = validate_root_token(&file.root, &file.root_token) {
        send_response(
            control_tx,
            request_id,
            response_error("invalid_root_token", &error.to_string()),
        )
        .await;
        return;
    }
    let service = Arc::clone(files);
    let work = file.clone();
    let opened = tokio::task::spawn_blocking(move || {
        service.open_file_stream_authorized(&work.root, &work.root_token, &work.path)
    })
    .await;
    let body = match opened {
        Ok(Ok(body)) => body,
        Ok(Err(error)) => {
            send_response(
                control_tx,
                request_id,
                response_error("file_open_rejected", &error.to_string()),
            )
            .await;
            return;
        }
        Err(error) => {
            send_response(
                control_tx,
                request_id,
                response_error("file_open_task_failed", &error.to_string()),
            )
            .await;
            return;
        }
    };
    if cancellation.load(Ordering::Acquire) {
        send_response(
            control_tx,
            request_id,
            response_error("cancelled", "file open was cancelled"),
        )
        .await;
        return;
    }
    send_stream_frame(
        control_tx,
        request_id,
        v1::FileStreamFrame {
            operation_id: file.operation_id.clone(),
            header: Some(body.header().clone()),
            ..Default::default()
        },
    )
    .await;

    if body.header().content_streaming {
        let digest = body.digest().to_owned();
        let mut chunks = body.chunks().peekable();
        let mut sent_any = false;
        while let Some((offset, chunk)) = chunks.next() {
            if cancellation.load(Ordering::Acquire) {
                send_response(
                    control_tx,
                    request_id,
                    response_error("cancelled", "file open was cancelled"),
                )
                .await;
                return;
            }
            sent_any = true;
            let eof = chunks.peek().is_none();
            send_stream_frame(
                control_tx,
                request_id,
                v1::FileStreamFrame {
                    operation_id: file.operation_id.clone(),
                    offset,
                    data: chunk.to_vec(),
                    eof,
                    blake3: if eof { digest.clone() } else { String::new() },
                    ..Default::default()
                },
            )
            .await;
        }
        if !sent_any {
            // An empty file still owes one eof frame, or the desktop cannot
            // tell "zero bytes" from "the body was cut short".
            send_stream_frame(
                control_tx,
                request_id,
                v1::FileStreamFrame {
                    operation_id: file.operation_id.clone(),
                    eof: true,
                    blake3: digest,
                    ..Default::default()
                },
            )
            .await;
        }
    }
    send_response(
        control_tx,
        request_id,
        file_response(&file.operation_id, |value| {
            value.content = Some(v1::FileContent {
                metadata: body.header().metadata.clone(),
                kind: body.header().content_kind,
                content: Vec::new(),
                generation: body.header().generation,
            });
        }),
    )
    .await;
}

async fn send_stream_frame(
    control_tx: &mpsc::Sender<SequencerControl>,
    request_id: u64,
    frame: v1::FileStreamFrame,
) {
    let _ = control_tx
        .send(SequencerControl::FileStream { request_id, frame })
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::serve_with_shutdown;
    use std::io::Write as _;
    use tmux_agent_protocol::{
        HOST_CAPABILITIES, envelope, read_frame, v1::envelope::Payload, write_frame,
    };
    use tokio::{net::UnixStream, time::timeout};

    struct BulkPeer {
        stream: UnixStream,
        next_request_id: u64,
    }

    impl BulkPeer {
        async fn connect() -> (Self, tokio::task::JoinHandle<anyhow::Result<()>>) {
            let (mut client, server) = UnixStream::pair().unwrap();
            let task = tokio::spawn(serve_with_shutdown(server, None));
            write_frame(
                &mut client,
                &envelope(
                    1,
                    0,
                    Payload::ClientHello(v1::ClientHello {
                        desktop_version: "file-stream-test".into(),
                        requested_capabilities: HOST_CAPABILITIES,
                        bulk_connection: true,
                        expected_server_identity: crate::service::snapshot::server_identity(),
                        ..Default::default()
                    }),
                ),
            )
            .await
            .unwrap();
            let hello = timeout(Duration::from_secs(3), read_frame(&mut client))
                .await
                .expect("server hello timed out")
                .unwrap()
                .unwrap();
            let Some(Payload::ServerHello(hello)) = hello.payload else {
                panic!("expected a server hello")
            };
            assert!(!hello.read_only);
            (
                Self {
                    stream: client,
                    next_request_id: 2,
                },
                task,
            )
        }

        /// Issues one `OpenFileStream` and reads through its terminal response.
        async fn open(
            &mut self,
            root: &str,
            token: &str,
            path: &str,
        ) -> (Vec<v1::FileStreamFrame>, v1::Response) {
            let request_id = self.next_request_id;
            self.next_request_id += 1;
            write_frame(
                &mut self.stream,
                &envelope(
                    request_id,
                    0,
                    Payload::Request(v1::Request {
                        operation: v1::Operation::OpenFileStream.into(),
                        file: Some(v1::FileServiceRequest {
                            operation_id: "open-1".into(),
                            root: root.to_owned(),
                            root_token: token.to_owned(),
                            path: path.to_owned(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                ),
            )
            .await
            .unwrap();
            let mut frames = Vec::new();
            loop {
                let frame = timeout(Duration::from_secs(5), read_frame(&mut self.stream))
                    .await
                    .expect("file stream timed out")
                    .unwrap()
                    .unwrap();
                match frame.payload {
                    Some(Payload::FileStream(stream)) => {
                        assert_eq!(
                            frame.request_id, request_id,
                            "a body frame left its request"
                        );
                        frames.push(stream);
                    }
                    Some(Payload::Response(response)) => {
                        assert_eq!(frame.request_id, request_id);
                        return (frames, response);
                    }
                    _ => continue,
                }
            }
        }
    }

    fn text_fixture(bytes: usize) -> (std::path::PathBuf, String, String) {
        let root = std::env::temp_dir().join(format!("ade-open-stream-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut file = std::fs::File::create(root.join("note.txt")).unwrap();
        // Deliberately not a repeated byte: a body reassembled out of order or
        // with a dropped window would still hash to the same value.
        let body: Vec<u8> = (0..bytes).map(|index| b'a' + (index % 26) as u8).collect();
        file.write_all(&body).unwrap();
        drop(file);
        let path = root.to_string_lossy().into_owned();
        let token = root_token(&path).unwrap();
        (root, path, token)
    }

    /// The whole point of the operation: one request, one header, a continuous
    /// body, one response — no per-mebibyte round trip and no text probe.
    #[tokio::test]
    async fn a_warm_open_is_one_request_one_header_and_a_continuous_body() {
        let (root, path, token) = text_fixture(2 * 1024 * 1024 + 17);
        let (mut peer, task) = BulkPeer::connect().await;
        let (frames, response) = peer.open(&path, &token, "note.txt").await;
        assert!(response.ok, "{}", response.display_message);

        let headers: Vec<_> = frames
            .iter()
            .filter_map(|frame| frame.header.as_ref())
            .collect();
        assert_eq!(headers.len(), 1, "exactly one classification per open");
        let header = headers[0];
        assert_eq!(header.content_kind, v1::FileContentKind::Text as i32);
        assert!(header.content_streaming);
        assert_eq!(header.total_bytes, 2 * 1024 * 1024 + 17);

        let body: Vec<_> = frames
            .iter()
            .filter(|frame| frame.header.is_none())
            .collect();
        assert_eq!(body.len(), 3, "1 MiB windows over a 2 MiB + 17 byte file");
        let mut offset = 0_u64;
        let mut content = Vec::new();
        for (index, frame) in body.iter().enumerate() {
            assert_eq!(
                frame.offset, offset,
                "body frame {index} arrived out of order"
            );
            assert_eq!(frame.eof, index + 1 == body.len());
            offset += frame.data.len() as u64;
            content.extend_from_slice(&frame.data);
        }
        assert_eq!(offset, header.total_bytes);
        assert_eq!(
            body.last().unwrap().blake3,
            blake3::hash(&content).to_hex().to_string()
        );
        assert_eq!(content, std::fs::read(root.join("note.txt")).unwrap());

        // And the terminal response repeats the identity the header carried, so
        // the desktop can bind them without a second exchange.
        let content_response = response.file.and_then(|file| file.content).unwrap();
        assert_eq!(content_response.generation, header.generation);
        assert_eq!(content_response.kind, header.content_kind);
        assert!(
            content_response.content.is_empty(),
            "bodies travel as frames"
        );

        drop(peer);
        task.abort();
        let _ = task.await;
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A classification the editor cannot show costs the header and nothing
    /// else, so a binary file is never transferred to be thrown away.
    #[tokio::test]
    async fn a_classification_only_open_sends_no_body_at_all() {
        let root = std::env::temp_dir().join(format!("ade-open-binary-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("blob.bin"), [0_u8, 1, 2, 3]).unwrap();
        let path = root.to_string_lossy().into_owned();
        let token = root_token(&path).unwrap();
        let (mut peer, task) = BulkPeer::connect().await;
        let (frames, response) = peer.open(&path, &token, "blob.bin").await;
        assert!(response.ok);
        assert_eq!(frames.len(), 1);
        let header = frames[0].header.as_ref().unwrap();
        assert_eq!(header.content_kind, v1::FileContentKind::Binary as i32);
        assert!(!header.content_streaming);
        drop(peer);
        task.abort();
        let _ = task.await;
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn a_rejected_open_answers_once_and_streams_nothing() {
        let (root, path, token) = text_fixture(4);
        let (mut peer, task) = BulkPeer::connect().await;
        let (frames, response) = peer.open(&path, &token, "absent.txt").await;
        assert!(!response.ok);
        assert_eq!(response.error_code, "file_open_rejected");
        assert!(frames.is_empty());
        drop(peer);
        task.abort();
        let _ = task.await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
