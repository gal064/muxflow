use anyhow::{Context, bail};
use tmux_agent_protocol::v1;

use super::super::terminal::validate_tmux_id;

const MAX_NAME_BYTES: usize = 200;
pub(super) const APP_SHELL: &str = "exec \"${SHELL:-/bin/sh}\"";

pub(super) fn configure_new_window(
    command: &mut std::process::Command,
    action: &v1::TmuxAction,
) -> anyhow::Result<()> {
    command.args([
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{window_id}",
        // Resolve the active pane path in the target session at execution time.
        "-c",
        "#{pane_current_path}",
        "-t",
        &action.session_id,
    ]);
    if !action.name.is_empty() {
        validate_name(&action.name)?;
        command.args(["-n", &action.name]);
    }
    command.arg(APP_SHELL);
    Ok(())
}

pub(super) fn configure_split(
    command: &mut std::process::Command,
    kind: v1::TmuxActionKind,
    action: &v1::TmuxAction,
) -> anyhow::Result<()> {
    command.args(["split-window", "-d", "-P", "-F", "#{pane_id}"]);
    if kind == v1::TmuxActionKind::SplitPaneRight {
        command.arg("-h");
    }
    if action.split_size != 0 {
        if !(1..=99).contains(&action.split_size) {
            bail!("split size must be between 1 and 99 percent");
        }
        command.args(["-p", &action.split_size.to_string()]);
    }
    // Let tmux resolve the target pane's live path. Passing a discovered path
    // back through `-c` would allow tmux format syntax in a directory name to
    // be reinterpreted and would race a cwd change between discovery/action.
    command.args(["-c", "#{pane_current_path}", "-t", &action.pane_id]);
    command.arg(APP_SHELL);
    Ok(())
}

pub(super) fn validate_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES || name.chars().any(char::is_control) {
        bail!("tmux name must be 1 to {MAX_NAME_BYTES} bytes without control characters");
    }
    Ok(())
}

pub(super) fn run(mut command: std::process::Command) -> anyhow::Result<()> {
    let output = command.output().context("run tmux action")?;
    if !output.status.success() {
        bail!(
            "tmux rejected action: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

pub(super) fn run_for_id(
    mut command: std::process::Command,
    prefix: char,
) -> anyhow::Result<String> {
    let output = command.output().context("run tmux action")?;
    if !output.status.success() {
        bail!(
            "tmux rejected action: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    validate_tmux_id(&id, prefix)?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_reject_command_record_boundaries() {
        assert!(validate_name("normal name").is_ok());
        assert!(validate_name("bad\nkill-server").is_err());
    }

    #[test]
    fn app_shell_executes_the_configured_shell_directly() {
        assert_eq!(APP_SHELL, "exec \"${SHELL:-/bin/sh}\"");
    }

    #[test]
    fn split_inherits_the_live_target_pane_path_without_rediscovering_it() {
        let action = v1::TmuxAction {
            pane_id: "%1".into(),
            split_size: 50,
            ..Default::default()
        };
        let mut command = std::process::Command::new("tmux");
        configure_split(&mut command, v1::TmuxActionKind::SplitPaneRight, &action).unwrap();
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "split-window",
                "-d",
                "-P",
                "-F",
                "#{pane_id}",
                "-h",
                "-p",
                "50",
                "-c",
                "#{pane_current_path}",
                "-t",
                "%1",
                APP_SHELL,
            ]
        );
    }

    #[test]
    fn new_window_inherits_the_target_sessions_live_active_pane_path() {
        let action = v1::TmuxAction {
            session_id: "$1".into(),
            name: "new tab".into(),
            ..Default::default()
        };
        let mut command = std::process::Command::new("tmux");
        configure_new_window(&mut command, &action).unwrap();
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{window_id}",
                "-c",
                "#{pane_current_path}",
                "-t",
                "$1",
                "-n",
                "new tab",
                APP_SHELL,
            ]
        );
    }
}
