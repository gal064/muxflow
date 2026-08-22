use std::sync::OnceLock;

use anyhow::{Context, bail};

use crate::service::snapshot::tmux_command;

/// Returns the command table used for process-wide tmux capability checks.
///
/// A failed probe is deliberately not cached: tmux may be temporarily
/// unavailable during startup, and the next request should be able to retry.
pub(super) fn tmux_command_table() -> anyhow::Result<&'static [u8]> {
    static COMMAND_TABLE: OnceLock<Vec<u8>> = OnceLock::new();
    cache_successful_probe(&COMMAND_TABLE, || {
        let output = tmux_command()
            .arg("list-commands")
            .output()
            .context("inspect tmux capabilities")?;
        if !output.status.success() {
            bail!(
                "inspect tmux capabilities: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output.stdout)
    })
    .map(Vec::as_slice)
}

fn cache_successful_probe<T>(
    cache: &OnceLock<T>,
    probe: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<&T> {
    if let Some(value) = cache.get() {
        return Ok(value);
    }
    let value = probe()?;
    let _ = cache.set(value);
    Ok(cache
        .get()
        .expect("a successful capability probe always populates the cache"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_probe_failure_is_retried_and_only_success_is_cached() {
        let cache = OnceLock::new();
        assert!(
            cache_successful_probe(&cache, || { anyhow::bail!("tmux temporarily unavailable") })
                .is_err()
        );
        assert!(*cache_successful_probe(&cache, || Ok(true)).unwrap());
        assert!(*cache_successful_probe(&cache, || Ok(false)).unwrap());
    }
}
