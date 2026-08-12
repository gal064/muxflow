use std::{fs::File, io::Write};

use anyhow::{Context, bail};

use super::MAX_TRANSFER_CHUNK;

pub(super) struct UploadStream {
    pub(super) file: File,
    pub(super) offset: u64,
    pub(super) total: u64,
    hasher: blake3::Hasher,
}

impl UploadStream {
    pub(super) fn new(file: File, total: u64) -> Self {
        Self {
            file,
            offset: 0,
            total,
            hasher: blake3::Hasher::new(),
        }
    }

    pub(super) fn write_chunk(&mut self, offset: u64, data: &[u8]) -> anyhow::Result<u64> {
        if data.is_empty() || data.len() > MAX_TRANSFER_CHUNK {
            bail!("terminal-upload chunks must contain 1 byte through 1 MiB");
        }
        if offset != self.offset {
            bail!(
                "stale terminal-upload offset: expected {}, received {offset}",
                self.offset
            );
        }
        let next = offset
            .checked_add(data.len() as u64)
            .context("terminal-upload byte counter overflow")?;
        if next > self.total {
            bail!("terminal-upload chunk exceeds declared byte count");
        }
        self.file.write_all(data)?;
        self.hasher.update(data);
        self.offset = next;
        Ok(next)
    }

    pub(super) fn verified_digest(&self, expected: &str) -> anyhow::Result<String> {
        if self.offset != self.total {
            bail!(
                "terminal upload is incomplete: received {} of {} bytes",
                self.offset,
                self.total
            );
        }
        let digest = self.hasher.clone().finalize().to_hex().to_string();
        if expected.is_empty() || digest != expected {
            bail!("terminal-upload BLAKE3 verification failed");
        }
        Ok(digest)
    }
}
