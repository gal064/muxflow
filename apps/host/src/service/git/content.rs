//! Deferred diff bodies, served over the independent bulk connection.
//!
//! The control response describes a large diff body with its exact size and
//! digest instead of carrying it. The desktop then reads it here, where the
//! bytes cannot delay a keystroke. Nothing is staged between the two requests:
//! the body is re-derived from the repository and accepted only if it still
//! hashes to what the control response described.

use super::*;
use diff::read_diff_side;

/// Bytes returned per bulk chunk. Matches the file transfer chunk size, which
/// the bulk framing and flow control are already sized for.
pub(super) const GIT_CONTENT_CHUNK: usize = 1024 * 1024;

/// The most recently served body, so a multi-chunk read costs one Git read
/// rather than one per chunk.
pub(super) struct CachedDiffBody {
    key: DiffBodyKey,
    body: Arc<Vec<u8>>,
}

/// Everything a cached body is bound to.
///
/// The scope fields are in here on purpose: a cache hit skips the discovery
/// path that would otherwise revalidate them, and a body served across a tmux
/// server swap or a root replacement would be a body from a repository the
/// client can no longer address.
#[derive(Clone, PartialEq, Eq)]
struct DiffBodyKey {
    server_identity: String,
    connection_epoch: u64,
    root: String,
    root_token: String,
    repository_id: String,
    path: Vec<u8>,
    original_path: Vec<u8>,
    diff_target: i32,
    side: i32,
    digest: String,
    /// Part of the key, so a cache hit can never be addressed with a different
    /// size than the one its bounds were checked against.
    size: u64,
}

impl DiffBodyKey {
    fn new(request: &v1::GitRequest, content: &v1::GitDiffContentRequest) -> Self {
        Self {
            server_identity: request.expected_server_identity.clone(),
            connection_epoch: request.connection_epoch,
            root: request.root.clone(),
            root_token: request.root_token.clone(),
            repository_id: request.repository_id.clone(),
            path: request.path.clone(),
            original_path: request.original_path.clone(),
            diff_target: request.diff_target,
            side: content.side,
            digest: content.expected_content_digest.clone(),
            size: content.expected_size,
        }
    }
}

impl GitService {
    pub(in crate::service) async fn diff_content(
        &self,
        request: &v1::GitRequest,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> anyhow::Result<v1::GitDiffContentChunk> {
        // Checked before the cache, not only on the miss that reaches
        // discovery: every chunk must be answered under the same identity.
        ensure_server_identity(request)?;
        require_repository_id(request)?;
        let content = request
            .content
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Git diff content request is required"))?;
        let side = v1::GitDiffContentSide::try_from(content.side).unwrap_or_default();
        if side == v1::GitDiffContentSide::Unspecified {
            bail!("Git diff content side is required");
        }
        if content.expected_content_digest.is_empty() || content.expected_size == 0 {
            bail!("Git diff content requires the size and digest it was classified with");
        }
        if content.expected_size > MAX_DIFF_CONTENT as u64 {
            bail!("Git diff content exceeds the bounded diff limit");
        }
        if content.offset > content.expected_size {
            bail!("Git diff content offset is outside the described body");
        }
        if content.length == 0 {
            bail!("Git diff content length must be positive");
        }
        let key = DiffBodyKey::new(request, &content);
        let key_for_release = key.clone();
        let cached = {
            let held = self.diff_body.lock().unwrap();
            held.as_ref()
                .filter(|entry| entry.key == key)
                .map(|entry| Arc::clone(&entry.body))
        };
        let body = match cached {
            Some(body) => body,
            None => {
                let body = self.read_diff_body(request, side, cancellation).await?;
                if body.len() as u64 != content.expected_size
                    || blake3::hash(&body).to_hex().as_str() != content.expected_content_digest
                {
                    bail!("Git diff content changed since it was classified; refresh the diff");
                }
                let body = Arc::new(body);
                *self.diff_body.lock().unwrap() = Some(CachedDiffBody {
                    key,
                    body: Arc::clone(&body),
                });
                body
            }
        };
        // The key carries the size the offset was checked against, so this
        // cannot address past the body — the clamp is belt and braces.
        let offset = (content.offset as usize).min(body.len());
        let length = (content.length as usize)
            .min(GIT_CONTENT_CHUNK)
            .min(body.len().saturating_sub(offset));
        let end = offset.saturating_add(length);
        if end >= body.len() {
            // This stream is finished. Releasing it bounds retention to one
            // in-progress body and makes a later request re-read and
            // re-validate rather than replay something the repository may no
            // longer contain. Released by key, because an interleaved stream
            // may have taken the slot in the meantime.
            let mut held = self.diff_body.lock().unwrap();
            if held
                .as_ref()
                .is_some_and(|entry| entry.key == key_for_release)
            {
                held.take();
            }
        }
        Ok(v1::GitDiffContentChunk {
            offset: content.offset,
            data: body[offset..end].to_vec(),
            last: end >= body.len(),
            total_size: body.len() as u64,
        })
    }

    async fn read_diff_body(
        &self,
        request: &v1::GitRequest,
        side: v1::GitDiffContentSide,
        cancellation: Option<Arc<AtomicBool>>,
    ) -> anyhow::Result<Vec<u8>> {
        let (_coordinator, capabilities) = self.repository(request, cancellation.clone()).await?;
        let work = request.clone();
        tokio::task::spawn_blocking(move || {
            let _guard = capabilities.metadata.install();
            read_diff_side(
                &capabilities.stable_root(),
                &work,
                side,
                cancellation.as_deref(),
            )
        })
        .await
        .map_err(|error| anyhow::anyhow!("Git diff content task failed: {error}"))?
    }
}
