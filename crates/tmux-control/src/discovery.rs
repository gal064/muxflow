use std::process::{Command, Output};

use serde::{Deserialize, Serialize};
use thiserror::Error;

// Printable because OpenSSH's remote tmux client sanitizes C0 control bytes in
// command arguments before forwarding them to the server.
const SEPARATOR: &str = "__ADE_TMUX_FIELD_9C71__";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    pub name: String,
    pub window_count: u32,
    pub attached_clients: u32,
    /// Application presentation order. tmux has no native session index;
    /// discovery initializes this deterministically and the host may overlay a
    /// private sidecar order without changing tmux options.
    pub order: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Window {
    pub id: String,
    pub session_id: String,
    pub index: u32,
    pub name: String,
    pub active: bool,
    pub layout: String,
    pub zoomed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pane {
    pub id: String,
    pub session_id: String,
    pub window_id: String,
    pub index: u32,
    pub active: bool,
    pub width: u16,
    pub height: u16,
    pub left: u16,
    pub top: u16,
    pub current_path: String,
    pub current_command: String,
    pub pane_pid: u32,
    pub start_command: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TmuxSnapshot {
    pub sessions: Vec<Session>,
    pub windows: Vec<Window>,
    pub panes: Vec<Pane>,
}

#[derive(Debug, Error)]
pub enum DiscoverError {
    #[error("failed to run tmux: {0}")]
    Spawn(#[from] std::io::Error),
    #[error("tmux command failed: {0}")]
    Command(String),
    #[error("invalid tmux {kind} record: {record:?}")]
    InvalidRecord { kind: &'static str, record: String },
}

pub fn discover() -> Result<TmuxSnapshot, DiscoverError> {
    discover_with(|args| Command::new("tmux").args(args).output())
}

/// Discovers an explicitly named tmux server (`tmux -L <name>`).
///
/// This is used by isolated integration tests now and by saved host profiles
/// once alternate-server selection is introduced.
pub fn discover_with_socket_name(socket_name: &str) -> Result<TmuxSnapshot, DiscoverError> {
    if socket_name.is_empty() || socket_name.bytes().any(|byte| byte == 0) {
        return Err(DiscoverError::Command(
            "invalid empty tmux socket name".into(),
        ));
    }
    discover_with(|args| {
        let mut command = Command::new("tmux");
        // `TMUX` identifies the caller's current server and takes precedence
        // over `-L`; explicit server selection must therefore drop it.
        command
            .env_remove("TMUX")
            .args(["-L", socket_name])
            .args(args)
            .output()
    })
}

/// Discovers tmux through a caller-provided process transport.
///
/// The desktop uses this to execute the same argv over system OpenSSH without
/// duplicating discovery or parsing behavior.
pub fn discover_with<F>(mut execute: F) -> Result<TmuxSnapshot, DiscoverError>
where
    F: FnMut(&[String]) -> Result<Output, std::io::Error>,
{
    let sessions = query(
        &mut execute,
        "list-sessions",
        "#{session_id}__ADE_TMUX_FIELD_9C71__#{session_name}__ADE_TMUX_FIELD_9C71__#{session_windows}__ADE_TMUX_FIELD_9C71__#{session_attached}",
    )?;
    if sessions.is_empty() {
        return Ok(TmuxSnapshot::default());
    }

    Ok(TmuxSnapshot {
        sessions: sessions
            .iter()
            .enumerate()
            .map(|(order, line)| {
                let mut session = parse_session(line)?;
                session.order = order.try_into().unwrap_or(u32::MAX);
                Ok(session)
            })
            .collect::<Result<_, DiscoverError>>()?,
        windows: query(
            &mut execute,
            "list-windows",
            "#{session_id}__ADE_TMUX_FIELD_9C71__#{window_id}__ADE_TMUX_FIELD_9C71__#{window_index}__ADE_TMUX_FIELD_9C71__#{window_name}__ADE_TMUX_FIELD_9C71__#{window_active}__ADE_TMUX_FIELD_9C71__#{window_layout}__ADE_TMUX_FIELD_9C71__#{window_zoomed_flag}",
        )?
        .iter()
        .map(|line| parse_window(line))
        .collect::<Result<_, _>>()?,
        panes: query(
            &mut execute,
            "list-panes",
            "#{session_id}__ADE_TMUX_FIELD_9C71__#{window_id}__ADE_TMUX_FIELD_9C71__#{pane_id}__ADE_TMUX_FIELD_9C71__#{pane_index}__ADE_TMUX_FIELD_9C71__#{pane_active}__ADE_TMUX_FIELD_9C71__#{pane_width}__ADE_TMUX_FIELD_9C71__#{pane_height}__ADE_TMUX_FIELD_9C71__#{pane_left}__ADE_TMUX_FIELD_9C71__#{pane_top}__ADE_TMUX_FIELD_9C71__#{pane_current_path}__ADE_TMUX_FIELD_9C71__#{pane_current_command}__ADE_TMUX_FIELD_9C71__#{pane_pid}__ADE_TMUX_FIELD_9C71__#{pane_start_command}",
        )?
        .iter()
        .map(|line| parse_pane(line))
        .collect::<Result<_, _>>()?,
    })
}

// ---------------------------------------------------------------------------
// Batched discovery
// ---------------------------------------------------------------------------

const SESSION_FORMAT: &str = "#{session_id}__ADE_TMUX_FIELD_9C71__#{session_name}__ADE_TMUX_FIELD_9C71__#{session_windows}__ADE_TMUX_FIELD_9C71__#{session_attached}";
const WINDOW_FORMAT: &str = "#{session_id}__ADE_TMUX_FIELD_9C71__#{window_id}__ADE_TMUX_FIELD_9C71__#{window_index}__ADE_TMUX_FIELD_9C71__#{window_name}__ADE_TMUX_FIELD_9C71__#{window_active}__ADE_TMUX_FIELD_9C71__#{window_layout}__ADE_TMUX_FIELD_9C71__#{window_zoomed_flag}";
const PANE_FORMAT: &str = "#{session_id}__ADE_TMUX_FIELD_9C71__#{window_id}__ADE_TMUX_FIELD_9C71__#{pane_id}__ADE_TMUX_FIELD_9C71__#{pane_index}__ADE_TMUX_FIELD_9C71__#{pane_active}__ADE_TMUX_FIELD_9C71__#{pane_width}__ADE_TMUX_FIELD_9C71__#{pane_height}__ADE_TMUX_FIELD_9C71__#{pane_left}__ADE_TMUX_FIELD_9C71__#{pane_top}__ADE_TMUX_FIELD_9C71__#{pane_current_path}__ADE_TMUX_FIELD_9C71__#{pane_current_command}__ADE_TMUX_FIELD_9C71__#{pane_pid}__ADE_TMUX_FIELD_9C71__#{pane_start_command}";

const IDENTITY_MARKER: &str = "__ADE_ID__";
const SESSION_MARKER: &str = "__ADE_S__";
const WINDOW_MARKER: &str = "__ADE_W__";
const PANE_MARKER: &str = "__ADE_P__";

/// One tmux invocation that returns the server's socket path and its whole
/// topology.
///
/// Discovery used to cost five client forks — identity, sessions, windows,
/// panes, identity again — and a tmux client fork is milliseconds, which is
/// most of the latency of every action. tmux accepts several commands per
/// invocation, so the same records travel on one fork. The repeated identity
/// probe becomes unnecessary at the same time: one client invocation is bound
/// to one server for its whole life, so there is no window in which the records
/// could come from two different servers.
pub fn batched_discovery_args() -> Vec<String> {
    [
        "display-message",
        "-p",
        &format!("{IDENTITY_MARKER}#{{socket_path}}"),
        ";",
        "list-sessions",
        "-F",
        &format!("{SESSION_MARKER}{SESSION_FORMAT}"),
        ";",
        "list-windows",
        "-a",
        "-F",
        &format!("{WINDOW_MARKER}{WINDOW_FORMAT}"),
        ";",
        "list-panes",
        "-a",
        "-F",
        &format!("{PANE_MARKER}{PANE_FORMAT}"),
    ]
    .into_iter()
    .map(ToOwned::to_owned)
    .collect()
}

/// The socket path and topology carried by one [`batched_discovery_args`] run.
pub struct BatchedDiscovery {
    pub socket_path: String,
    pub snapshot: TmuxSnapshot,
}

/// Parses the output of [`batched_discovery_args`].
///
/// `success` is the invocation's exit status. tmux aborts a chained command
/// list at the first failure, and a live server with no sessions fails at the
/// first command that needs a current target — after the identity line has
/// already been printed. That case is a real empty topology, not an error, so
/// it is accepted; a failure that already produced topology records is not,
/// because a truncated topology would let an action run against a snapshot the
/// server never actually had.
pub fn parse_batched_discovery(
    stdout: &[u8],
    stderr: &[u8],
    success: bool,
) -> Result<BatchedDiscovery, DiscoverError> {
    let text = String::from_utf8_lossy(stdout);
    let mut socket_path = None;
    let (mut sessions, mut windows, mut panes) = (Vec::new(), Vec::new(), Vec::new());
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix(IDENTITY_MARKER) {
            socket_path = Some(rest.trim_end().to_owned());
        } else if let Some(rest) = line.strip_prefix(SESSION_MARKER) {
            sessions.push(rest);
        } else if let Some(rest) = line.strip_prefix(WINDOW_MARKER) {
            windows.push(rest);
        } else if let Some(rest) = line.strip_prefix(PANE_MARKER) {
            panes.push(rest);
        }
    }
    let Some(socket_path) = socket_path else {
        return Err(DiscoverError::Command(
            String::from_utf8_lossy(stderr).trim().to_owned(),
        ));
    };
    if !success && !(sessions.is_empty() && windows.is_empty() && panes.is_empty()) {
        return Err(DiscoverError::Command(
            String::from_utf8_lossy(stderr).trim().to_owned(),
        ));
    }
    Ok(BatchedDiscovery {
        socket_path,
        snapshot: TmuxSnapshot {
            sessions: sessions
                .iter()
                .enumerate()
                .map(|(order, line)| {
                    let mut session = parse_session(line)?;
                    session.order = order.try_into().unwrap_or(u32::MAX);
                    Ok(session)
                })
                .collect::<Result<_, DiscoverError>>()?,
            windows: windows
                .iter()
                .map(|line| parse_window(line))
                .collect::<Result<_, _>>()?,
            panes: panes
                .iter()
                .map(|line| parse_pane(line))
                .collect::<Result<_, _>>()?,
        },
    })
}

fn query<F>(execute: &mut F, subcommand: &str, format: &str) -> Result<Vec<String>, DiscoverError>
where
    F: FnMut(&[String]) -> Result<Output, std::io::Error>,
{
    let mut args = vec![subcommand.to_owned()];
    if subcommand != "list-sessions" {
        args.push("-a".into());
    }
    args.extend(["-F".into(), format.into()]);
    let output = execute(&args)?;
    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if error.contains("no server running") || error.contains("no sessions") {
            return Ok(Vec::new());
        }
        return Err(DiscoverError::Command(error));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(ToOwned::to_owned)
        .collect())
}

fn fields<'a>(
    kind: &'static str,
    line: &'a str,
    count: usize,
) -> Result<Vec<&'a str>, DiscoverError> {
    let result: Vec<_> = line.split(SEPARATOR).collect();
    if result.len() != count {
        return Err(DiscoverError::InvalidRecord {
            kind,
            record: line.into(),
        });
    }
    Ok(result)
}

fn number<T: std::str::FromStr>(
    kind: &'static str,
    line: &str,
    value: &str,
) -> Result<T, DiscoverError> {
    value.parse().map_err(|_| DiscoverError::InvalidRecord {
        kind,
        record: line.into(),
    })
}

fn parse_session(line: &str) -> Result<Session, DiscoverError> {
    let value = fields("session", line, 4)?;
    Ok(Session {
        id: value[0].into(),
        name: value[1].into(),
        window_count: number("session", line, value[2])?,
        attached_clients: number("session", line, value[3])?,
        order: 0,
    })
}

fn parse_window(line: &str) -> Result<Window, DiscoverError> {
    let value = fields("window", line, 7)?;
    Ok(Window {
        session_id: value[0].into(),
        id: value[1].into(),
        index: number("window", line, value[2])?,
        name: value[3].into(),
        active: value[4] == "1",
        layout: value[5].into(),
        zoomed: value[6] == "1",
    })
}

fn parse_pane(line: &str) -> Result<Pane, DiscoverError> {
    let value = fields("pane", line, 13)?;
    let pane_pid = number::<u32>("pane", line, value[11])?;
    Ok(Pane {
        session_id: value[0].into(),
        window_id: value[1].into(),
        id: value[2].into(),
        index: number("pane", line, value[3])?,
        active: value[4] == "1",
        width: number("pane", line, value[5])?,
        height: number("pane", line, value[6])?,
        left: number("pane", line, value[7])?,
        top: number("pane", line, value[8])?,
        current_path: value[9].into(),
        current_command: value[10].into(),
        pane_pid,
        start_command: value[12].into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::time::{Duration, Instant};

    fn batched_output(sessions: &[&str], windows: &[&str], panes: &[&str]) -> Vec<u8> {
        let mut text = String::from("__ADE_ID__/tmp/tmux-501/default\n");
        for line in sessions {
            text.push_str(&format!("__ADE_S__{line}\n"));
        }
        for line in windows {
            text.push_str(&format!("__ADE_W__{line}\n"));
        }
        for line in panes {
            text.push_str(&format!("__ADE_P__{line}\n"));
        }
        text.into_bytes()
    }

    #[test]
    fn batched_discovery_parses_one_invocation_into_the_same_records_as_five() {
        let session = format!("$1{SEPARATOR}work{SEPARATOR}1{SEPARATOR}0");
        let window = format!(
            "$1{SEPARATOR}@2{SEPARATOR}0{SEPARATOR}editor{SEPARATOR}1{SEPARATOR}b25d,80x24,0,0,2{SEPARATOR}0"
        );
        let pane = format!(
            "$1{SEPARATOR}@2{SEPARATOR}%3{SEPARATOR}0{SEPARATOR}1{SEPARATOR}80{SEPARATOR}24{SEPARATOR}0{SEPARATOR}0{SEPARATOR}/tmp/a path{SEPARATOR}fish{SEPARATOR}123{SEPARATOR}"
        );
        let parsed = parse_batched_discovery(
            &batched_output(&[&session], &[&window], &[&pane]),
            b"",
            true,
        )
        .unwrap();
        assert_eq!(parsed.socket_path, "/tmp/tmux-501/default");
        assert_eq!(parsed.snapshot.sessions.len(), 1);
        assert_eq!(parsed.snapshot.sessions[0].order, 0);
        assert_eq!(parsed.snapshot.windows[0].name, "editor");
        assert_eq!(parsed.snapshot.panes[0].current_path, "/tmp/a path");

        // The batched argv and the per-command formats must describe the same
        // fields, or one path would parse records the other cannot produce.
        let arguments = batched_discovery_args();
        assert!(arguments.iter().any(|value| value.contains(SESSION_FORMAT)));
        assert!(arguments.iter().any(|value| value.contains(WINDOW_FORMAT)));
        assert!(arguments.iter().any(|value| value.contains(PANE_FORMAT)));
    }

    #[test]
    fn a_live_server_with_no_sessions_is_an_empty_topology_but_a_partial_one_is_an_error() {
        // tmux aborts a chained command list at the first command needing a
        // current target, after the identity line has already printed. That is
        // a real empty topology, not a failure.
        let empty =
            parse_batched_discovery(&batched_output(&[], &[], &[]), b"no current target", false)
                .unwrap();
        assert!(empty.snapshot.sessions.is_empty());
        assert_eq!(empty.socket_path, "/tmp/tmux-501/default");

        // A failure that already produced records is a truncated topology, and
        // acting on one would let a mutation run against a server state that
        // never existed.
        let session = format!("$1{SEPARATOR}work{SEPARATOR}1{SEPARATOR}0");
        assert!(
            parse_batched_discovery(&batched_output(&[&session], &[], &[]), b"boom", false)
                .is_err()
        );

        // No identity line at all means tmux never answered.
        assert!(parse_batched_discovery(b"", b"no server running", false).is_err());
    }

    #[test]
    fn parses_names_and_paths_without_whitespace_splitting() {
        let pane = parse_pane("$1__ADE_TMUX_FIELD_9C71__@2__ADE_TMUX_FIELD_9C71__%3__ADE_TMUX_FIELD_9C71__0__ADE_TMUX_FIELD_9C71__1__ADE_TMUX_FIELD_9C71__80__ADE_TMUX_FIELD_9C71__24__ADE_TMUX_FIELD_9C71__0__ADE_TMUX_FIELD_9C71__0__ADE_TMUX_FIELD_9C71__/tmp/a path__ADE_TMUX_FIELD_9C71__fish__ADE_TMUX_FIELD_9C71__123__ADE_TMUX_FIELD_9C71__").unwrap();
        assert_eq!(pane.current_path, "/tmp/a path");

        let session = parse_session("$1__ADE_TMUX_FIELD_9C71__name with spaces__ADE_TMUX_FIELD_9C71__2__ADE_TMUX_FIELD_9C71__1").unwrap();
        assert_eq!(session.name, "name with spaces");
    }

    #[test]
    fn parses_twenty_sessions_and_one_hundred_windows_within_scale_budget() {
        let sessions = (0..20)
            .map(|session| format!("${session}{SEPARATOR}s{session}{SEPARATOR}5{SEPARATOR}0"))
            .collect::<Vec<_>>()
            .join("\n");
        let windows = (0..100)
            .map(|window| {
                format!(
                    "${}{SEPARATOR}@{window}{SEPARATOR}{}{SEPARATOR}w{window}{SEPARATOR}{}{SEPARATOR}b25d,80x24,0,0,{window}{SEPARATOR}0",
                    window / 5,
                    window % 5 + 1,
                    u8::from(window % 5 == 0),
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let panes = (0..100)
            .map(|pane| {
                format!(
                    "${}{SEPARATOR}@{pane}{SEPARATOR}%{pane}{SEPARATOR}0{SEPARATOR}1{SEPARATOR}80{SEPARATOR}24{SEPARATOR}0{SEPARATOR}0{SEPARATOR}/tmp{SEPARATOR}bash{SEPARATOR}999999{SEPARATOR}exec bash",
                    pane / 5,
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let started = Instant::now();
        let snapshot = discover_with(|args| {
            let stdout = match args[0].as_str() {
                "list-sessions" => sessions.as_bytes(),
                "list-windows" => windows.as_bytes(),
                "list-panes" => panes.as_bytes(),
                _ => unreachable!(),
            };
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: stdout.to_vec(),
                stderr: Vec::new(),
            })
        })
        .unwrap();
        assert_eq!(snapshot.sessions.len(), 20);
        assert_eq!(snapshot.windows.len(), 100);
        assert_eq!(snapshot.panes.len(), 100);
        assert!(started.elapsed() < Duration::from_millis(100));
    }
}
