use super::*;

/// One editor open, classified and read once from one descriptor.
///
/// The whole point of the type is that everything the desktop needs — identity,
/// classification, size, generation, and the bytes themselves — comes from the
/// same open file description. The staircase it replaces asked three separate
/// questions (stat, preflight, then a chunk request per mebibyte), each of
/// which could answer about a different file and each of which cost a round
/// trip on the remote link.
pub(crate) struct FileStreamBody {
    header: v1::FileStreamHeader,
    content: Vec<u8>,
    digest: String,
}

impl FileStreamBody {
    pub(crate) fn header(&self) -> &v1::FileStreamHeader {
        &self.header
    }

    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }

    /// The body as bounded `(offset, chunk)` windows, in order, consuming it.
    ///
    /// Consuming rather than borrowing so the dispatcher moves each window into
    /// its frame instead of copying it: a 25 MiB image was otherwise resident
    /// twice, once as the body and once as the frame being written.
    ///
    /// Empty when the classification carries no content, so a binary, oversized,
    /// or ineligible open is exactly one header frame and its response.
    pub(crate) fn into_chunks(self) -> impl Iterator<Item = (u64, Vec<u8>)> {
        let mut content = self.content;
        let mut offset = 0_u64;
        std::iter::from_fn(move || {
            if content.is_empty() {
                return None;
            }
            let taken = content.len().min(MAX_TRANSFER_CHUNK);
            // Drains from the front, so the buffer shrinks as frames leave.
            let chunk: Vec<u8> = content.drain(..taken).collect();
            let at = offset;
            offset += taken as u64;
            Some((at, chunk))
        })
    }
}

impl FileService {
    #[cfg(test)]
    pub(crate) fn open_file_stream(
        &self,
        root: &str,
        path: &str,
    ) -> anyhow::Result<FileStreamBody> {
        let token = root_token(root)?;
        self.open_file_stream_authorized(root, &token, path, &NEVER_CANCELLED)
    }

    /// Opens one descriptor, classifies from it, and reads whatever content the
    /// classification says the editor may show.
    pub(crate) fn open_file_stream_authorized(
        &self,
        root: &str,
        root_token: &str,
        path: &str,
        cancellation: &AtomicBool,
    ) -> anyhow::Result<FileStreamBody> {
        if cancellation.load(Ordering::Acquire) {
            return Err(cancelled("file open"));
        }
        let root = RootCapability::validate(root, root_token)?;
        let (logical_target, target) = root.resolve_existing(path)?;
        if logical_target == root.logical_root() {
            bail!("the active root itself cannot be opened as a file");
        }
        let (logical_opened, opened_path) = root.regular_file_target(&logical_target, &target)?;
        let mut file = root.anchor(&logical_opened)?.open_file()?;
        let opened_before = file.metadata()?;
        let image = image_mime(&opened_path).is_some();
        let size = opened_before.len();

        let (kind, content) = if image {
            if size <= MAX_IMAGE_BYTES {
                (
                    v1::FileContentKind::Image,
                    read_bounded(&mut file, size, cancellation)?,
                )
            } else {
                (v1::FileContentKind::Image, Vec::new())
            }
        } else if size > MAX_TEXT_BYTES {
            (v1::FileContentKind::TooLarge, Vec::new())
        } else {
            let bytes = read_bounded(&mut file, size, cancellation)?;
            if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
                (v1::FileContentKind::Binary, Vec::new())
            } else {
                (v1::FileContentKind::Text, bytes)
            }
        };

        // Re-stat the same description. A file rewritten while it was being
        // read would otherwise be published as content from one version under
        // the identity of another, and the editor's next save would then be
        // written against a generation that never described these bytes.
        let opened_after = file.metadata()?;
        if metadata_generation(&opened_before) != metadata_generation(&opened_after) {
            return Err(stale_generation("file changed while it was being opened"));
        }

        let mut metadata = metadata_for_anchored(&root.stable_root(), &target, &logical_target)?;
        // Size, time, and mode describe the bytes the editor is about to show;
        // `generation` stays the leaf's, so it means the same thing here as in
        // any listing of the same path. The opened content's own identity
        // travels separately, in the stream header.
        let leaf_generation = metadata.generation;
        apply_effective_metadata(&mut metadata, &opened_after);
        metadata.generation = leaf_generation;
        if metadata.symlink {
            metadata.symlink_target_kind = v1::FileKind::File.into();
        }
        metadata.image_preview_eligible = image && size <= MAX_IMAGE_BYTES;

        let content_streaming =
            matches!(kind, v1::FileContentKind::Text | v1::FileContentKind::Image)
                && !(image && !metadata.image_preview_eligible);
        let digest = blake3::hash(&content).to_hex().to_string();
        Ok(FileStreamBody {
            header: v1::FileStreamHeader {
                generation: metadata_generation(&opened_after),
                total_bytes: if content_streaming {
                    content.len() as u64
                } else {
                    0
                },
                metadata: Some(metadata),
                content_kind: kind.into(),
                content_streaming,
            },
            content,
            digest,
        })
    }
}

/// Reads a whole bounded file, refusing content that grew past its stat.
///
/// Read in bounded windows rather than one call so a cancelled open — a
/// superseded preview, a closed tab — stops touching a slow remote filesystem
/// instead of running to completion for an answer nobody will read.
fn read_bounded(
    file: &mut File,
    expected: u64,
    cancellation: &AtomicBool,
) -> anyhow::Result<Vec<u8>> {
    let capacity = usize::try_from(expected).context("file is larger than this host can open")?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut bounded = file.take(expected.saturating_add(1));
    loop {
        if cancellation.load(Ordering::Acquire) {
            return Err(cancelled("file open"));
        }
        let before = bytes.len();
        let read = (&mut bounded)
            .take(MAX_TRANSFER_CHUNK as u64)
            .read_to_end(&mut bytes)?;
        if read == 0 || bytes.len() == before {
            break;
        }
    }
    if bytes.len() as u64 != expected {
        return Err(stale_generation("file changed while it was being opened"));
    }
    Ok(bytes)
}
