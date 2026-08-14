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
const AUTOMATIC_RENAME_FORMAT: &str = "automatic-rename-format";

/// The hook, built from the adapter registry so it cannot drift from the set of
/// agents this app understands.
///
/// Scoped to agent panes, and this is the whole difference between a
/// recommendation and an imposition. An unscoped `rename-window` fires for
/// every pane, so a shell whose prompt sets the terminal title renames its
/// window to `user@host:~/some/path` — measured on the field machine — and
/// `rename-window` also turns tmux's own automatic renaming off for that
/// window, permanently, which outlives this app's connection. Guarding with
/// `if -F` means the command never runs for a non-agent pane at all, so an
/// ordinary shell window keeps the name tmux would have given it.
fn recommended_hook_command() -> String {
    let mut condition = String::new();
    for adapter in super::agents::adapters::all() {
        let test = format!("#{{==:#{{pane_current_command}},{}}}", adapter.executable());
        condition = if condition.is_empty() {
            test
        } else {
            format!("#{{||:{condition},{test}}}")
        };
    }
    format!("if -F \"{condition}\" \"rename-window \\\"#{{pane_title}}\\\"\"")
}

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
    if already_syncs_pane_titles()? {
        return Ok(NamingOutcome::AlreadyConfigured);
    }
    let command = recommended_hook_command();
    let output = tmux_command()
        .args(["set-hook", "-g", PANE_TITLE_HOOK, &command])
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
    if setting(PANE_TITLE_HOOK)?.is_empty() {
        bail!("tmux accepted the recommended window naming but did not retain it");
    }
    Ok(NamingOutcome::Applied)
}

/// Whether this server already puts pane titles into window names, by either
/// of the two mechanisms that can.
///
/// A `pane-title-changed` hook is the direct one and the one this app would
/// add. An `automatic-rename-format` that mentions `pane_title` reaches the
/// same result by a different route, and a user who chose that route has
/// configured this as deliberately as one who wrote the hook. Neither is
/// overridden — theirs may carry exemptions this app knows nothing about.
fn already_syncs_pane_titles() -> anyhow::Result<bool> {
    Ok(!setting(PANE_TITLE_HOOK)?.is_empty()
        || setting(AUTOMATIC_RENAME_FORMAT)?
            .iter()
            .any(|value| value.contains("pane_title")))
}

/// Every value bound to a global option, one per returned entry.
///
/// `show-options -g <name>` prints nothing but the option name when it is unset
/// and `<name>[0] <value>` for each bound value, so an empty result is the
/// precise, positive test for "the user has not configured this".
fn setting(name: &str) -> anyhow::Result<Vec<String>> {
    let output = tmux_command()
        .args(["show-options", "-g", name])
        .output()
        .with_context(|| format!("read the tmux {name} setting"))?;
    if !output.status.success() {
        bail!(
            "tmux could not report its {name} setting: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|(option, _)| option.starts_with(name))
        .map(|(_, value)| value.trim().to_owned())
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
    fn the_recommended_set_is_one_noncosmetic_hook_scoped_to_agent_panes() {
        assert_eq!(PANE_TITLE_HOOK, "pane-title-changed");
        let command = recommended_hook_command();
        // Every adapter the app understands, and nothing else: a pane running
        // something else must not reach `rename-window` at all, because that
        // command also disables tmux's automatic renaming for the window
        // permanently.
        for adapter in super::super::agents::adapters::all() {
            assert!(
                command.contains(&format!(
                    "#{{==:#{{pane_current_command}},{}}}",
                    adapter.executable()
                )),
                "{} is not covered by {command}",
                adapter.id()
            );
        }
        assert!(command.starts_with("if -F "), "{command}");
        assert!(command.contains("rename-window"));
        assert!(!command.contains("status"), "nothing cosmetic: {command}");
        assert_eq!(NamingOutcome::Applied.label(), "applied");
        assert_eq!(
            NamingOutcome::AlreadyConfigured.label(),
            "alreadyConfigured"
        );
    }
}
