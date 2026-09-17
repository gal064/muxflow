use std::{fs::File, io::Read, sync::OnceLock};

use anyhow::Context;
use sha2::{Digest, Sha256};

static DIGEST: OnceLock<Result<String, String>> = OnceLock::new();

/// SHA-256 of the executable image this process started from.
///
/// Linux opens `/proc/self/exe`, which continues to name the running inode
/// after an atomic helper upgrade. macOS app updates do not replace a running
/// bundle executable, so its resolved executable path has the same property for
/// the supported update flow. The result is captured once per process.
pub fn digest() -> anyhow::Result<&'static str> {
    match DIGEST.get_or_init(|| compute().map_err(|error| format!("{error:#}"))) {
        Ok(digest) => Ok(digest),
        Err(error) => anyhow::bail!(error.clone()),
    }
}

fn compute() -> anyhow::Result<String> {
    #[cfg(target_os = "linux")]
    let path = std::path::PathBuf::from("/proc/self/exe");
    #[cfg(not(target_os = "linux"))]
    let path = std::env::current_exe().context("resolve running helper executable")?;

    let mut file = File::open(&path).context("open running helper executable")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .context("read running helper executable")?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn build_digest_is_stable_lowercase_sha256() {
        let first = super::digest().unwrap();
        assert_eq!(first.len(), 64);
        assert!(
            first
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        assert_eq!(super::digest().unwrap(), first);
    }
}
