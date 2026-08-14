use anyhow::{Context, bail};

use super::snapshot::tmux_command;

/// The one tmux setting this app's contract depends on.
///
/// The contract is "tabs are tmux windows", and the user's decision is that
/// window names must be right *in tmux*, not prettified in the app. On a stock
/// server every agent window is called `claude`, because tmux names a window
/// after the command running in it; the descriptive names on the developer
/// machines came from a personal `~/.tmux.conf` the product never declared it
/// needed.
///
/// This is that declaration, reduced to the single thing that produces the
/// behaviour: when a pane changes its title — which is how Claude Code and
/// Codex announce what they are working on — the window takes that title.
/// Nothing cosmetic is included. The status bar, key bindings and colours
/// remain entirely the user's business.
const PANE_TITLE_HOOK: &str = "pane-title-changed";
const RECOMMENDED_HOOK_COMMAND: &str = "rename-window \"#{pane_title}\"";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NamingOutcome {
    /// This app set the hook. The tmux server holds it in memory only.
    Applied,
    /// The user's own configuration already syncs pane titles to window names.
    /// Their version is kept: it is theirs, and it may carry exemptions this
    /// app knows nothing about — the field machine's config exempts one window
    /// by name so a verbose editor title cannot clobber it.
    AlreadyConfigured,
}

impl NamingOutcome {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::AlreadyConfigured => "alreadyConfigured",
        }
    }
}

/// Apply the recommended naming, unless the server already does it.
///
/// Idempotent in both directions: running it twice sets the same global hook to
/// the same value, and running it against a server that already has an
/// equivalent hook does nothing at all. Nothing is written to disk — the hook
/// lives in the running tmux server, so a server restart drops it and the next
/// connect re-asserts it.
pub(crate) fn apply_recommended_naming() -> anyhow::Result<NamingOutcome> {
    if !existing_hook()?.is_empty() {
        return Ok(NamingOutcome::AlreadyConfigured);
    }
    let output = tmux_command()
        .args(["set-hook", "-g", PANE_TITLE_HOOK, RECOMMENDED_HOOK_COMMAND])
        .output()
        .context("apply the recommended tmux window naming")?;
    if !output.status.success() {
        bail!(
            "tmux rejected the recommended window naming: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    // Read back rather than trusting the exit status: an option that did not
    // take is the failure mode worth catching, and it costs one command.
    if existing_hook()?.is_empty() {
        bail!("tmux accepted the recommended window naming but did not retain it");
    }
    Ok(NamingOutcome::Applied)
}

/// Every command bound to `pane-title-changed` on this server, one per line.
///
/// `show-options -g <hook>` prints nothing but the option name when the hook is
/// unset and `pane-title-changed[0] <command>` for each bound command, so an
/// empty result is the precise, positive test for "the user has not configured
/// this" — and any non-empty result, whatever its shape, is left alone.
fn existing_hook() -> anyhow::Result<Vec<String>> {
    let output = tmux_command()
        .args(["show-options", "-g", PANE_TITLE_HOOK])
        .output()
        .context("read the tmux window-naming hook")?;
    if !output.status.success() {
        bail!(
            "tmux could not report its window-naming hook: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|(name, _)| name.starts_with(PANE_TITLE_HOOK))
        .map(|(_, command)| command.trim().to_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recommended set is one hook, and it is the hook that produces the
    /// behaviour the product's tab contract depends on. A future addition here
    /// is a change to what this app does to a user's tmux server, so it should
    /// have to move this assertion too.
    #[test]
    fn the_recommended_set_is_one_noncosmetic_hook() {
        assert_eq!(PANE_TITLE_HOOK, "pane-title-changed");
        assert_eq!(RECOMMENDED_HOOK_COMMAND, "rename-window \"#{pane_title}\"");
        assert!(!RECOMMENDED_HOOK_COMMAND.contains("status"));
        assert_eq!(NamingOutcome::Applied.label(), "applied");
        assert_eq!(
            NamingOutcome::AlreadyConfigured.label(),
            "alreadyConfigured"
        );
    }
}
