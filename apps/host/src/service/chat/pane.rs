//! The tmux side of a chat pane: the options that tag it, and what runs in
//! it.
//!
//! A chat pane is a pane whose `@muxflow-chat` option names a chat adapter.
//! The options live on the pane, so they outlive the daemon: one that
//! restarts finds its chats again by reading them.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

use super::provider::Launch;
use crate::service::{snapshot::tmux_command, tmux_actions::escaped_format_literal};

/// The chat adapter's id. Its presence is what makes a pane a chat.
pub(super) const ADAPTER: &str = "@muxflow-chat";
/// The unix socket the pane's agent talks over.
pub(super) const SOCKET: &str = "@muxflow-chat-socket";
/// The agent's own id for the conversation, once it is known.
pub(super) const SESSION: &str = "@muxflow-chat-session";
/// The message a rewind dropped, for an agent whose record of the
/// conversation still ends with it until a new message follows. An agent
/// that needs no such mark leaves it unset.
pub(super) const REWOUND_TO: &str = "@muxflow-chat-at";
/// Set on a pane opened for a side question, so that it is never listed as a
/// session of its own.
pub(crate) const SIDE: &str = "@muxflow-chat-side";

/// How the chat was started, so it can be started again the same way: a
/// [`Launch`] as JSON, in hex so that no byte of it means anything to tmux.
pub(super) const START: &str = "@muxflow-chat-start";

const TAG_OPTIONS: [&str; 5] = [ADAPTER, SOCKET, SESSION, REWOUND_TO, START];
const SEPARATOR: &str = "__MUXFLOW_CHAT_FIELD_4E1B__";

/// What tmux says about one pane.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Tag {
    /// Empty for a pane that is not a chat.
    pub(super) adapter: String,
    pub(super) socket: PathBuf,
    pub(super) session: String,
    pub(super) rewound_to: String,
    pub(super) launch: Launch,
    /// The pane's own process, which the agent runs beneath.
    pub(super) pid: u32,
    pub(super) cwd: String,
}

/// Reads a pane's tag. `None` when the pane is gone.
pub(super) fn read(pane_id: &str) -> Option<Tag> {
    let format = [
        "#{@muxflow-chat}",
        "#{@muxflow-chat-socket}",
        "#{@muxflow-chat-session}",
        "#{@muxflow-chat-at}",
        "#{@muxflow-chat-start}",
        "#{pane_pid}",
        "#{pane_current_path}",
    ]
    .join(SEPARATOR);
    let output = tmux_command()
        .ok()?
        .args(["display-message", "-p", "-t", pane_id, &format])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8(output.stdout).ok()?;
    let mut fields = line.trim_end_matches('\n').split(SEPARATOR);
    let mut next = || fields.next().map(str::to_owned);
    Some(Tag {
        adapter: next()?,
        socket: PathBuf::from(next()?),
        session: next()?,
        rewound_to: next()?,
        launch: decode_launch(&next()?),
        pid: next()?.parse().ok()?,
        cwd: next()?,
    })
}

pub(super) fn encode_launch(launch: &Launch) -> String {
    let json = serde_json::to_vec(launch).unwrap_or_default();
    json.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A launch that cannot be read is the default one: the agent as it starts
/// with no arguments.
fn decode_launch(hex: &str) -> Launch {
    let bytes: Option<Vec<u8>> = hex
        .as_bytes()
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect();
    bytes
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// A value that is safe as one tmux argument: tmux ends a command at an
/// argument that ends in a semicolon.
fn checked(value: &str) -> anyhow::Result<&str> {
    if value.is_empty() || value.ends_with(';') || value.chars().any(char::is_control) {
        bail!("not a usable tmux option value");
    }
    Ok(value)
}

fn run(mut command: Command) -> anyhow::Result<()> {
    let output = command.output().context("run tmux")?;
    if !output.status.success() {
        bail!(
            "tmux refused: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

/// Adds `set-option` commands for `options` to a tmux command line. An empty
/// value unsets the option.
fn push_options(command: &mut Command, pane_id: &str, options: &[(&str, &str)]) {
    for (name, value) in options {
        command.args([";", "set-option", "-p", "-t", pane_id]);
        if value.is_empty() {
            command.args(["-u", name]);
        } else {
            command.args([name, value]);
        }
    }
}

/// Sets pane options; an empty value unsets one.
pub(super) fn set(pane_id: &str, options: &[(&str, &str)]) -> anyhow::Result<()> {
    let Some(((name, value), rest)) = options.split_first() else {
        return Ok(());
    };
    for (_, value) in options {
        if !value.is_empty() {
            checked(value)?;
        }
    }
    let mut command = tmux_command()?;
    command.args(["set-option", "-p", "-t", pane_id]);
    if value.is_empty() {
        command.args(["-u", name]);
    } else {
        command.args([name, value]);
    }
    push_options(&mut command, pane_id, rest);
    run(command)
}

/// Makes the pane a terminal again.
pub(super) fn untag(pane_id: &str) {
    let _ = set(pane_id, &TAG_OPTIONS.map(|name| (name, "")));
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The shell line a chat pane runs: the agent behind `chat-pipe`, and the
/// user's shell once the agent has gone, so the pane outlives its chat.
pub(super) fn shell_line(
    helper: &Path,
    environment: Option<&Path>,
    socket: Option<&Path>,
    argv: &[String],
) -> String {
    let mut words = vec![
        shell_quote(&helper.to_string_lossy()),
        "chat-pipe".to_owned(),
    ];
    if let Some(environment) = environment {
        words.extend([
            "--env".to_owned(),
            shell_quote(&environment.to_string_lossy()),
        ]);
    }
    if let Some(socket) = socket {
        words.extend([
            "--socket".to_owned(),
            shell_quote(&socket.to_string_lossy()),
        ]);
    }
    words.push("--".to_owned());
    words.extend(argv.iter().map(|argument| shell_quote(argument)));
    format!("{}; exec \"${{SHELL:-/bin/sh}}\" -i", words.join(" "))
}

/// Replaces what runs in the pane with `shell_line`, in `cwd`, and sets
/// `options` on it in the same step, so nobody sees the new process without
/// its tag.
pub(super) fn respawn(
    pane_id: &str,
    cwd: &str,
    shell_line: &str,
    options: &[(&str, &str)],
) -> anyhow::Result<()> {
    for (_, value) in options {
        if !value.is_empty() {
            checked(value)?;
        }
    }
    let mut command = tmux_command()?;
    command.args(respawn_arguments(pane_id, cwd, shell_line)?);
    push_options(&mut command, pane_id, options);
    run(command)
}

fn respawn_arguments(pane_id: &str, cwd: &str, shell_line: &str) -> anyhow::Result<Vec<String>> {
    let mut arguments: Vec<String> = ["respawn-pane", "-k", "-t", pane_id]
        .map(str::to_owned)
        .into();
    if !cwd.is_empty() {
        // tmux reads a start directory as a format: a `#` in a folder's name
        // must mean itself.
        arguments.extend(["-c".to_owned(), escaped_format_literal(checked(cwd)?)]);
    }
    // Separate arguments, so tmux runs `/bin/sh` itself instead of handing
    // the line to a shell that may not be a POSIX one.
    arguments.extend(["/bin/sh", "-c", shell_line].map(str::to_owned));
    Ok(arguments)
}

/// Where the chat sockets and the environment files of this host live.
pub(super) fn directory() -> PathBuf {
    crate::paths::runtime_dir().join("chat")
}

/// A new socket path, and the environment file that goes with it.
pub(super) fn new_paths() -> anyhow::Result<(PathBuf, PathBuf)> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let directory = directory();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&directory)
        .context("create the chat socket directory")?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let name = uuid::Uuid::new_v4().simple().to_string();
    let socket = directory.join(format!("{name}.sock"));
    crate::paths::check_socket_path_length(&socket)?;
    Ok((socket, directory.join(format!("{name}.env"))))
}

/// Removes what chats that are gone left in the chat directory: a socket no
/// tagged pane names, an environment file nothing read. Files younger than a
/// minute are left, since a chat may be starting.
pub(super) fn sweep(panes: &[tmux_control::Pane]) {
    let Ok(entries) = std::fs::read_dir(directory()) else {
        return;
    };
    // A pane that cannot be read may be using any of them: nothing is
    // removed on a guess.
    let Some(in_use) = panes
        .iter()
        .filter(|pane| !pane.chat_adapter_id.is_empty())
        .map(|pane| read(&pane.id).map(|tag| tag.socket))
        .collect::<Option<Vec<PathBuf>>>()
    else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let old = std::fs::symlink_metadata(&path)
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age.as_secs() > 60);
        if old && !in_use.contains(&path) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_survives_being_a_pane_option() {
        let launch = Launch {
            program: vec!["node".into(), "/opt/it's here/cli.js".into()],
            model: "opus".into(),
            directories: vec!["/a;".into(), "/b #{pane_id}".into()],
            resume: super::super::provider::Resume::Session {
                id: "abc".into(),
                fork: true,
            },
            prompt: "not kept".into(),
            ..Default::default()
        };
        let encoded = encode_launch(&launch);
        assert!(encoded.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(
            decode_launch(&encoded),
            Launch {
                prompt: String::new(),
                ..launch
            }
        );
        assert_eq!(decode_launch(""), Launch::default());
        assert_eq!(decode_launch("zz"), Launch::default());
    }

    #[test]
    fn the_pane_line_quotes_every_word_and_ends_in_the_users_shell() {
        let line = shell_line(
            Path::new("/opt/mux flow/muxflow-host"),
            Some(Path::new("/run/chat/a.env")),
            Some(Path::new("/run/chat/a.sock")),
            &["claude".into(), "--name".into(), "it's; rm -rf".into()],
        );
        assert_eq!(
            line,
            "'/opt/mux flow/muxflow-host' chat-pipe --env '/run/chat/a.env' \
             --socket '/run/chat/a.sock' -- 'claude' '--name' 'it'\\''s; rm -rf'; \
             exec \"${SHELL:-/bin/sh}\" -i"
        );
        // An agent that listens on the socket itself gets no `--socket`.
        let line = shell_line(Path::new("/h"), None, None, &["codex".into()]);
        assert_eq!(
            line,
            "'/h' chat-pipe -- 'codex'; exec \"${SHELL:-/bin/sh}\" -i"
        );
    }

    #[test]
    fn a_folder_name_is_never_a_tmux_format() {
        let arguments = respawn_arguments("%1", "/work/#(touch x)/#{pane_id}", "line").unwrap();
        assert_eq!(
            arguments,
            [
                "respawn-pane",
                "-k",
                "-t",
                "%1",
                "-c",
                "/work/##(touch x)/##{pane_id}",
                "/bin/sh",
                "-c",
                "line"
            ]
        );
        assert!(respawn_arguments("%1", "/work/a;", "line").is_err());
        // No folder: tmux keeps the pane's.
        assert_eq!(respawn_arguments("%1", "", "line").unwrap().len(), 7);
    }

    #[test]
    fn a_value_tmux_would_read_as_a_command_end_is_refused() {
        assert!(checked("abc").is_ok());
        assert!(checked("abc;").is_err());
        assert!(checked("a\nb").is_err());
        assert!(checked("").is_err());
    }
}
