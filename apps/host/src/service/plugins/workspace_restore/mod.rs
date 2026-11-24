//! Workspace restore: the host remembers the tmux sessions it has seen, with
//! the directory of every window, and recreates them when the server comes
//! back after dying — a reboot, a crash, a `kill-server`.
//!
//! It lives on the host, not the desktop, for two reasons the desktop version
//! could not answer. The record outlives every client: a desktop only sees
//! what happened while it was connected, while the host watches sessions
//! close and knows the difference between "the user killed it" (the same
//! server is still here, so the record follows the live view) and "the server
//! died" (the socket's identity changed, so the record is the layout to
//! recreate). And the phone gets the same recovery as every desktop, from one
//! implementation.
//!
//! The server's identity — the dev:ino of its socket, as
//! [`crate::service::snapshot::server_identity`] — is the whole oracle. tmux
//! cannot hand a new server an old socket path with old inode numbers, so a
//! changed identity always means a new server, and an unchanged one always
//! means the same server no matter what was killed on it.
//!
//! Off until asked, per host, through `OPERATION_WORKSPACE_RESTORE`. While
//! off nothing is recorded, so a person who turned it off can trust that no
//! new paths are written. Enabling starts the record from what is open then.

use std::{
    collections::BTreeMap,
    sync::{Mutex, MutexGuard, OnceLock, PoisonError},
    time::Duration,
};

use tmux_agent_protocol::v1;

mod store;
#[cfg(test)]
mod tests;

use store::{Paths, Record, RememberedSession, RememberedWindow};

/// How often the live view is looked at. Structural truth is worth one
/// `list-panes` per quarter-minute, and the file is only written when the
/// layout actually moved.
const TICK: Duration = Duration::from_secs(15);

static STATE: OnceLock<Mutex<Tracker>> = OnceLock::new();

/// The tmux and clock calls a tick makes. Injected so the state machine can
/// be tested without a server: the recorder in `tests` answers instead.
pub(super) struct Env<'a> {
    /// The socket identity of the server that answers now; "tmux:none" when
    /// no server is up.
    pub server_identity: &'a dyn Fn() -> String,
    /// Every pane on every session, or an error when tmux could not answer.
    /// An error changes nothing: the record waits for the next look.
    pub panes: &'a dyn Fn() -> anyhow::Result<Vec<Pane>>,
    /// A tmux command with the socket arguments, ready for its subcommand.
    pub tmux: &'a dyn Fn() -> anyhow::Result<std::process::Command>,
    /// Runs a built command. Separated from the factory so a test can record
    /// what the restore would run without running it.
    pub exec: &'a dyn Fn(std::process::Command) -> anyhow::Result<()>,
}

/// One live pane, as the record's grouping needs it.
#[derive(Clone)]
pub(super) struct Pane {
    pub session: String,
    pub window: u32,
    pub pane: u32,
    pub path: String,
}

pub(super) fn start() -> tokio::task::JoinHandle<()> {
    tokio::spawn(async {
        let mut interval = tokio::time::interval(TICK);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let outcome = tokio::task::spawn_blocking(|| with_env(|env| tracker().tick(env))).await;
            if outcome.is_err() {
                // A tick that panicked took its record with it; the next one
                // reloads from disk.
                continue;
            }
        }
    })
}

/// `OPERATION_WORKSPACE_RESTORE`. Blocking on the record file, like every
/// detached handler.
pub(super) fn handle(request: v1::WorkspaceRestoreRequest) -> v1::Response {
    match v1::WorkspaceRestoreAction::try_from(request.action)
        .unwrap_or(v1::WorkspaceRestoreAction::Unspecified)
    {
        v1::WorkspaceRestoreAction::Get => tracker().response(),
        v1::WorkspaceRestoreAction::SetEnabled => {
            let mut tracker = tracker();
            tracker.set_enabled(request.enabled);
            tracker.response()
        }
        v1::WorkspaceRestoreAction::Unspecified => response_error(
            "invalid_workspace_restore",
            "a workspace restore request needs an action",
        ),
    }
}

fn response_error(code: &str, message: &str) -> v1::Response {
    v1::Response {
        ok: false,
        error_code: code.into(),
        display_message: message.into(),
        ..Default::default()
    }
}

/// The record plus the memory of what was restored when, so one dead server
/// is only ever mourned once.
pub(super) struct Tracker {
    paths: Paths,
    record: Record,
}

impl Tracker {
    fn new(paths: Paths) -> Self {
        let record = Record::load(&paths).unwrap_or_else(|_| {
            // A record that cannot be parsed is not a layout to recreate, and
            // it must not be parsed again every start: the file moves aside,
            // the same way history moves an unreadable sessions file.
            let _ = std::fs::rename(
                &paths.record,
                paths.record.with_extension("json.unreadable"),
            );
            Record::default()
        });
        Self { paths, record }
    }

    fn tick(&mut self, env: &Env) {
        if !self.record.enabled {
            // Off remembers nothing: no paths, no baselines.
            return;
        }
        let identity = (env.server_identity)();
        if identity == "tmux:none" {
            // No server. The record holds the layout the last server had, and
            // noting the loss here is what makes the next server — even one
            // reborn on the same socket path — a new one to this record.
            if self.record.server_identity != identity {
                self.record.server_identity = identity;
                let _ = self.record.save(&self.paths);
            }
            return;
        }
        let live = match (env.panes)() {
            Ok(live) => live,
            // A tmux that will not answer is not evidence of anything: the
            // record waits for the next look.
            Err(_) => return,
        };
        if self.record.server_identity == identity {
            // The same server: the live view is the whole truth. Sessions the
            // user closed leave the record here; a reboot cannot hide,
            // because a new server never keeps an old identity.
            let next = record_from_live(identity, &live);
            if next != self.record {
                self.record = next;
                let _ = self.record.save(&self.paths);
            }
            return;
        }
        // A different server than the record was watching. If there was a
        // watched server before — or its known death — the old one is gone,
        // and the record is the layout to recreate.
        if !self.record.server_identity.is_empty() {
            let present: BTreeMap<&str, ()> = live
                .iter()
                .map(|pane| (pane.session.as_str(), ()))
                .collect();
            for session in &self.record.sessions {
                if present.contains_key(session.name.as_str()) {
                    // Already open: the user or a respawn beat us to it.
                    continue;
                }
                if recreate(env, session).is_err() {
                    // One session that tmux refused must not stop the rest;
                    // the next line of the record still gets its chance. It
                    // leaves the record with the re-baseline below, so a
                    // refusal that will not heal is not retried forever.
                }
            }
            // Re-read with the recreates in, and make the new server's live
            // view the record from now on.
            let live = (env.panes)().unwrap_or_default();
            self.record = record_from_live(identity, &live);
            let _ = self.record.save(&self.paths);
        } else {
            // First watched server: take it as the baseline and record it.
            self.record = record_from_live(identity, &live);
            let _ = self.record.save(&self.paths);
        }
    }

    /// Turns the feature on or off, saving only when the answer changes.
    /// Enabling starts from nothing: the layout right now, remembered by the
    /// next tick, and no old record resurrected for a server that died while
    /// the feature was off. Disabling forgets the remembered directories too:
    /// a person who turned it off can trust that no paths of theirs are kept.
    fn set_enabled(&mut self, enabled: bool) {
        if enabled == self.record.enabled {
            return;
        }
        if enabled {
            self.record = Record::current(
                true,
                // The server running now, so the next tick sees a match
                // unless the server really is new.
                crate::service::snapshot::server_identity(),
                Vec::new(),
            );
        } else {
            self.record = Record::current(false, String::new(), Vec::new());
        }
        let _ = self.record.save(&self.paths);
    }

    fn response(&self) -> v1::Response {
        v1::Response {
            ok: true,
            workspace_restore: Some(v1::WorkspaceRestoreResponse {
                enabled: self.record.enabled,
                sessions: self
                    .record
                    .sessions
                    .iter()
                    .map(|session| v1::WorkspaceRestoreSession {
                        name: session.name.clone(),
                        windows: session
                            .windows
                            .iter()
                            .map(|window| v1::WorkspaceRestoreWindow {
                                directory: window.directory.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
            }),
            ..Default::default()
        }
    }
}

fn tracker() -> MutexGuard<'static, Tracker> {
    STATE
        .get_or_init(|| Mutex::new(Tracker::new(Paths::under(&crate::paths::state_dir()))))
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// The record the live view writes: sessions in name order (the order a
/// person reading `GET` expects), each holding its windows in strip order,
/// each window keeping the directory of its first pane.
fn record_from_live(identity: String, live: &[Pane]) -> Record {
    let mut grouped: BTreeMap<&str, BTreeMap<u32, &Pane>> = BTreeMap::new();
    for pane in live {
        let windows = grouped.entry(&pane.session).or_default();
        // The first pane of a window owns its directory: `pane_current_path`
        // of a split's second pane is wherever that split was made, which is
        // usually the same but is not what the window means.
        match windows.get(&pane.window) {
            Some(seen) if seen.pane <= pane.pane => {}
            _ => {
                windows.insert(pane.window, pane);
            }
        }
    }
    Record::current(
        true,
        identity,
        grouped
            .into_iter()
            .map(|(name, windows)| RememberedSession {
                name: name.to_owned(),
                windows: windows
                    .into_values()
                    .map(|pane| RememberedWindow {
                        directory: pane.path.clone(),
                    })
                    .collect(),
            })
            .collect(),
    )
}

/// Recreate one remembered session on a new server: `new-session` for its
/// first window, `new-window` for the rest, each at the directory it was
/// last in. A directory that no longer exists starts that window at tmux's
/// own choice instead — the session coming back is worth more than the
/// exact starting point, and the agent that lives there will say so.
fn recreate(env: &Env, session: &RememberedSession) -> anyhow::Result<()> {
    let action = v1::TmuxAction {
        name: session.name.clone(),
        directory: usable_directory(session.windows.first()),
        ..Default::default()
    };
    // The same builder a desktop's create goes through: name validation,
    // directory resolution, a detached session running the login shell.
    let mut command = (env.tmux)()?;
    crate::service::tmux_actions::configure_new_session(&mut command, &action, false)?;
    (env.exec)(command)?;
    for window in session.windows.iter().skip(1) {
        let mut command = (env.tmux)()?;
        command.args(["new-window", "-d", "-t", &escaped(&session.name)]);
        let directory = usable_directory(Some(window));
        if !directory.is_empty() {
            command.args(["-c", &escaped(&directory)]);
        }
        command.args(crate::service::tmux_actions::APP_SHELL);
        (env.exec)(command)?;
    }
    Ok(())
}

/// The remembered directory if the host's own judge would accept it, empty
/// (tmux's own choice) otherwise. The same `resolved_start_directory` a
/// desktop's create passes through, run now so one gone directory cannot
/// make tmux refuse the whole `new-session`.
fn usable_directory(window: Option<&RememberedWindow>) -> String {
    let Some(window) = window else {
        return String::new();
    };
    crate::service::tmux_actions::resolved_start_directory(&window.directory).unwrap_or_default()
}

fn escaped(value: &str) -> String {
    crate::service::tmux_actions::escaped_format_literal(value)
}

/// Runs `body` with the real tmux server and clock.
fn with_env<R>(body: impl FnOnce(&Env) -> R) -> R {
    body(&Env {
        server_identity: &crate::service::snapshot::server_identity,
        panes: &live_panes,
        tmux: &crate::service::snapshot::tmux_command,
        exec: &crate::service::tmux_actions::run,
    })
}

/// Every pane on every session, one `list-panes` away. The record groups
/// this into sessions and windows; a pane whose line tmux explained
/// differently than the format promises is skipped rather than guessed at.
fn live_panes() -> anyhow::Result<Vec<Pane>> {
    let mut command = crate::service::snapshot::tmux_command()?;
    // Unit separators: no tmux format field can contain one, so a split
    // cannot fake a column.
    command.args([
        "list-panes",
        "-a",
        "-F",
        "#{session_name}\u{1f}#{window_index}\u{1f}#{pane_index}\u{1f}#{pane_current_path}",
    ]);
    let output = command.output()?;
    if !output.status.success() {
        anyhow::bail!(
            "tmux rejected list-panes: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let mut panes = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.split('\u{1f}');
        let (Some(session), Some(window), Some(pane), Some(path)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(window), Ok(pane)) = (window.parse::<u32>(), pane.parse::<u32>()) else {
            continue;
        };
        panes.push(Pane {
            session: session.to_owned(),
            window,
            pane,
            path: path.to_owned(),
        });
    }
    Ok(panes)
}
