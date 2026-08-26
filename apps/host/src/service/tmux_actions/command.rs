use anyhow::{Context, bail};
use tmux_agent_protocol::v1;

use super::super::terminal::validate_tmux_id;

const MAX_NAME_BYTES: usize = 200;
pub(super) const APP_SHELL: &str = "exec \"${SHELL:-/bin/sh}\"";

/// The whole `new-session` argv, built and judged before a session exists.
///
/// Two things ride here that the desktop cannot do for itself. The start
/// directory is resolved and checked *on the host that owns the filesystem*,
/// before `new-session` is spawned, so a path that is missing, relative, or not
/// a directory refuses the create outright instead of leaving a workspace
/// sitting in the wrong place. And the first window is named after the
/// workspace in the same command, so the name arrives with the session rather
/// than as a second round trip that can fail on its own.
///
/// Naming the window has one documented consequence: an explicit `-n` turns
/// tmux's automatic renaming off for that window, so it keeps the workspace's
/// name instead of following the running command. The agent naming hook
/// (`tmux_config`) still renames it when an agent takes the pane over — that
/// hook is a `rename-window`, not automatic renaming — and a later rename by
/// the user is likewise preserved.
pub(super) fn configure_new_session(
    command: &mut std::process::Command,
    action: &v1::TmuxAction,
) -> anyhow::Result<()> {
    // The window id rides along so the desktop can retire the pending
    // placeholder the moment the snapshot names this window — without it, a
    // session-create placeholder had no window to wait for and sat in the strip
    // forever.
    command.args([
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{session_id} #{window_id} #{pane_id}",
    ]);
    if !action.directory.is_empty() {
        command.arg("-c");
        command.arg(escaped_format_literal(&resolved_start_directory(
            &action.directory,
        )?));
    }
    if !action.name.is_empty() {
        validate_name(&action.name)?;
        // The same escape, and it has to be the same string in both places:
        // the session and its first window are one name, and a workspace whose
        // two halves disagree is worse than either.
        let name = escaped_format_literal(&action.name);
        command.args(["-s", &name]);
        command.args(["-n", &name]);
    }
    command.arg(APP_SHELL);
    Ok(())
}

/// Text tmux must take literally, in the one place tmux would not.
///
/// tmux runs the name and start-directory arguments of `new-session`,
/// `new-window`, `rename-session` and `rename-window` through its *format*
/// parser, so an argument is protected from the shell by being an argument and
/// protected from tmux by nothing. That is not academic in either direction: a
/// directory legitimately named `#Session-notes` would start the pane somewhere
/// else than the path this file just stood behind, and `#(…)` is tmux's
/// run-a-command substitution — on an SSH profile, a command that runs on the
/// remote machine. `##` is tmux's own escape for a literal `#`, so doubling
/// every one of them makes the value mean itself.
///
/// Applied at every one of those sites rather than only the new ones: a
/// workspace name that is safe to create and unsafe to rename to is the worst
/// of both, and the whole point is that a name means the characters in it.
pub(super) fn escaped_format_literal(value: &str) -> String {
    value.replace('#', "##")
}

/// The configured start directory, or the reason the workspace is not created.
///
/// Judged here, on the machine that owns the path, and before anything is
/// spawned. `~` is expanded against this host process's environment because
/// that is the home directory the session would have started in anyway; a
/// relative path has no meaning at the point tmux runs, so it is refused rather
/// than resolved against whatever this process's cwd happens to be.
fn resolved_start_directory(directory: &str) -> anyhow::Result<String> {
    if directory.contains('\0') || directory.chars().any(char::is_control) {
        bail!("workspace start directory must not contain control characters");
    }
    let expanded = if directory == "~" || directory.starts_with("~/") {
        let home = std::env::var("HOME").unwrap_or_default();
        if home.is_empty() {
            bail!("workspace start directory uses ~ but the home directory is unknown");
        }
        format!("{home}{}", &directory[1..])
    } else {
        directory.to_owned()
    };
    let path = std::path::Path::new(&expanded);
    if !path.is_absolute() {
        bail!("workspace start directory {expanded} must be an absolute path");
    }
    if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir()) {
        bail!("workspace start directory {expanded} does not exist or is not a directory");
    }
    // Existing is not the same as enterable, and the difference is the failure
    // this check exists to prevent: `metadata` needs only search permission on
    // the *parent*, so a directory this process cannot enter reports itself as
    // a directory, `new-session` succeeds anyway, and tmux quietly starts the
    // pane in $HOME instead — a workspace created somewhere the user did not
    // ask for, with nothing said about it.
    if !searchable(path) {
        bail!("workspace start directory {expanded} cannot be entered");
    }
    Ok(expanded)
}

/// Whether this process could `chdir` into the path — the exact thing tmux is
/// about to try. `access(2)` asks the kernel the question directly; reading the
/// directory would answer a stricter one and refuse a legitimate `--x` path.
fn searchable(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: `c_path` is a valid NUL-terminated string for the call's duration.
    unsafe { libc::access(c_path.as_ptr(), libc::X_OK) == 0 }
}

pub(super) fn configure_new_window(
    command: &mut std::process::Command,
    action: &v1::TmuxAction,
) -> anyhow::Result<()> {
    command.args([
        "new-window",
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
        command.args(["-n", &escaped_format_literal(&action.name)]);
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

pub(super) fn run_for_ids(
    mut command: std::process::Command,
    prefixes: &[char],
) -> anyhow::Result<Vec<String>> {
    let output = command.output().context("run tmux action")?;
    if !output.status.success() {
        bail!(
            "tmux rejected action: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let ids = String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if ids.len() != prefixes.len() {
        bail!("tmux action returned an incomplete identity tuple");
    }
    for (id, prefix) in ids.iter().zip(prefixes) {
        validate_tmux_id(id, *prefix)?;
    }
    Ok(ids)
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

    fn session_args(action: &v1::TmuxAction) -> anyhow::Result<Vec<String>> {
        let mut command = std::process::Command::new("tmux");
        configure_new_session(&mut command, action)?;
        Ok(command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect())
    }

    /// The workspace's name is the first window's name, in the command that
    /// creates the session — not a rename afterwards that can fail on its own.
    #[test]
    fn new_session_names_its_first_window_after_the_workspace() {
        let action = v1::TmuxAction {
            name: "checkout".into(),
            ..Default::default()
        };
        assert_eq!(
            session_args(&action).unwrap(),
            [
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{session_id} #{window_id} #{pane_id}",
                "-s",
                "checkout",
                "-n",
                "checkout",
                APP_SHELL,
            ]
        );
    }

    /// tmux runs `-c`, `-s` and `-n` through its format parser, so an argument
    /// is safe from the shell and not from tmux. `#Session-notes` is a legal
    /// directory name that would otherwise start the pane at `` — and `#(…)` is
    /// tmux's run-a-command substitution, which on an SSH profile runs there.
    #[test]
    fn new_session_hands_tmux_a_literal_directory_and_name() {
        let directory = std::env::temp_dir().join("muxflow-#test");
        std::fs::create_dir_all(&directory).unwrap();
        let action = v1::TmuxAction {
            directory: directory.to_string_lossy().into_owned(),
            name: "build #(id)".into(),
            ..Default::default()
        };
        let args = session_args(&action).unwrap();
        let after = |flag: &str| {
            args[args.iter().position(|argument| argument == flag).unwrap() + 1].clone()
        };
        assert_eq!(
            after("-c"),
            format!("{}", directory.to_string_lossy()).replace('#', "##")
        );
        assert_eq!(after("-n"), "build ##(id)");
        // One name, not two: the session and its first window must not disagree.
        assert_eq!(after("-s"), after("-n"));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// Existing is not enterable. `metadata` needs only search permission on the
    /// parent, so without this the create succeeds and tmux silently starts the
    /// pane in $HOME — a workspace somewhere the user never asked for.
    #[test]
    fn new_session_refuses_a_directory_it_could_not_enter() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join(format!("muxflow-sealed-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o000)).unwrap();
        let refusal = session_args(&v1::TmuxAction {
            directory: directory.to_string_lossy().into_owned(),
            ..Default::default()
        })
        .map(|_| String::new())
        .unwrap_or_else(|error| error.to_string());
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::remove_dir_all(&directory).unwrap();
        // Root ignores the permission bits, so there is nothing to assert there.
        if unsafe { libc::geteuid() } != 0 {
            assert!(refusal.contains("cannot be entered"), "{refusal}");
        }
    }

    /// A workspace name that is safe to create with and unsafe to rename to
    /// would be the worst of both. Every path that hands tmux a name escapes it.
    #[test]
    fn every_name_path_hands_tmux_a_literal() {
        assert_eq!(escaped_format_literal("build #(id)"), "build ##(id)");
        assert_eq!(escaped_format_literal("plain"), "plain");
        let action = v1::TmuxAction {
            session_id: "$1".into(),
            name: "tab #(id)".into(),
            ..Default::default()
        };
        let mut command = std::process::Command::new("tmux");
        configure_new_window(&mut command, &action).unwrap();
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.contains(&"tab ##(id)".to_owned()), "{args:?}");
    }

    #[test]
    fn new_session_without_a_configured_directory_leaves_tmuxs_own_choice_alone() {
        let args = session_args(&v1::TmuxAction::default()).unwrap();
        assert!(!args.iter().any(|argument| argument == "-c"), "{args:?}");
    }

    /// `~` is the shape people type, and tmux does not expand it: without this
    /// the create fails on a literal directory named `~`.
    ///
    /// Read from the live environment rather than by setting `HOME`, because
    /// mutating the process environment from one test races every other test in
    /// the binary. A host with no usable home has nothing to assert about here.
    #[test]
    fn new_session_expands_a_home_relative_directory_against_this_hosts_home() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        if !std::path::Path::new(&home).is_dir() {
            return;
        }
        let args = session_args(&v1::TmuxAction {
            directory: "~".into(),
            ..Default::default()
        })
        .unwrap();
        let index = args.iter().position(|argument| argument == "-c").unwrap();
        assert_eq!(args[index + 1], home.to_string_lossy());
    }

    /// Every refusal happens while building the argv, which is before
    /// `new-session` runs: a bad path cannot leave a half-placed workspace
    /// behind, because there is no workspace yet.
    #[test]
    fn new_session_refuses_a_directory_it_cannot_stand_behind() {
        let refused = |directory: &str| {
            session_args(&v1::TmuxAction {
                directory: directory.into(),
                ..Default::default()
            })
            .unwrap_err()
            .to_string()
        };
        assert!(refused("relative/path").contains("absolute"));
        assert!(refused("/nonexistent-muxflow-start-directory").contains("does not exist"));
        // A path that exists and is not a directory is refused for the same
        // reason a missing one is: tmux would fail after the session existed.
        let file = std::env::current_exe().unwrap();
        assert!(refused(&file.to_string_lossy()).contains("does not exist or is not a directory"));
        assert!(refused("/tmp\nkill-server").contains("control characters"));
    }

    #[test]
    fn new_session_passes_an_existing_directory_through_verbatim() {
        let directory = std::env::temp_dir();
        let action = v1::TmuxAction {
            directory: directory.to_string_lossy().into_owned(),
            name: "work".into(),
            ..Default::default()
        };
        let args = session_args(&action).unwrap();
        let index = args.iter().position(|argument| argument == "-c").unwrap();
        assert_eq!(args[index + 1], directory.to_string_lossy());
        // Before the name, and long before the shell: the whole argv is one
        // command, so the directory and the window name land together.
        assert!(index < args.iter().position(|argument| argument == "-n").unwrap());
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
