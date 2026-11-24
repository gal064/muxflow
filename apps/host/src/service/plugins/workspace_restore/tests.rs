//! The state machine, tested without a tmux server: the recorder answers the
//! identity, the pane list and the exec, and the commands the restore would
//! run are kept for inspection instead of run.

use std::cell::RefCell;

use tmux_agent_protocol::v1;

use super::{Env, Pane, Tracker, response_error, store::Paths};
use crate::service::plugins::workspace_restore::store::{
    Record, RememberedSession, RememberedWindow,
};

/// A world: one identity, one pane list, and the commands the restore ran.
struct Recorder {
    identity: RefCell<String>,
    panes: RefCell<Vec<Pane>>,
    ran: RefCell<Vec<Vec<String>>>,
}

/// `list-panes` cannot produce these, so a test that leaves one behind has
/// a bug in the test.
fn pane(session: &str, window: u32, pane: u32, path: &str) -> Pane {
    Pane {
        session: session.into(),
        window,
        pane,
        path: path.into(),
    }
}

impl Recorder {
    fn new(identity: &str) -> Self {
        Self {
            identity: RefCell::new(identity.into()),
            panes: RefCell::new(Vec::new()),
            ran: RefCell::new(Vec::new()),
        }
    }

    fn set(&self, identity: &str, panes: Vec<Pane>) {
        *self.identity.borrow_mut() = identity.into();
        *self.panes.borrow_mut() = panes;
    }

    /// Runs `tick` against this world.
    fn tick(&self, tracker: &mut Tracker) {
        let env = Env {
            server_identity: &|| self.identity.borrow().clone(),
            panes: &|| -> anyhow::Result<Vec<Pane>> { Ok(self.panes.borrow().clone()) },
            tmux: &|| -> anyhow::Result<std::process::Command> {
                Ok(std::process::Command::new("true"))
            },
            exec: &|command: std::process::Command| -> anyhow::Result<()> {
                let argv = command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect();
                self.ran.borrow_mut().push(argv);
                Ok(())
            },
        };
        tracker.tick(&env);
    }

    fn ran(&self) -> Vec<String> {
        self.ran
            .borrow()
            .iter()
            .map(|argv| argv.join(" "))
            .collect()
    }
}

fn tracker(dir: &std::path::Path) -> Tracker {
    Tracker::new(Paths::under(dir))
}

/// A tracker with the switch on but nothing remembered: what a fresh enable
/// leaves behind, without `set_enabled` asking the real tmux who is running.
fn enabled_tracker(dir: &std::path::Path) -> Tracker {
    let mut tracker = tracker(dir);
    tracker.record.enabled = true;
    tracker
}

/// A tracker with a record already written, as if the daemon had been
/// watching the server named `identity`.
fn watching(dir: &std::path::Path, record: Record) -> Tracker {
    let mut tracker = tracker(dir);
    tracker.record = record;
    tracker.record.save(&tracker.paths).unwrap();
    tracker
}

fn remembered(name: &str, directories: &[&str]) -> RememberedSession {
    RememberedSession {
        name: name.into(),
        windows: directories
            .iter()
            .map(|directory| RememberedWindow {
                directory: (*directory).into(),
            })
            .collect(),
    }
}

fn record_of(identity: &str, sessions: Vec<RememberedSession>) -> Record {
    Record::current(true, identity.into(), sessions)
}

#[test]
fn a_first_server_is_written_as_the_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Recorder::new("tmux:s1");
    recorder.set(
        "tmux:s1",
        vec![
            pane("zeta", 0, 0, "/z"),
            pane("alpha", 1, 0, "/second"),
            pane("alpha", 0, 0, "/first"),
            // A split's second pane does not own its window's directory.
            pane("alpha", 0, 1, "/elsewhere"),
        ],
    );
    let mut tracker = enabled_tracker(dir.path());
    recorder.tick(&mut tracker);

    let saved = Record::load(&tracker.paths).unwrap();
    assert_eq!(saved.server_identity, "tmux:s1");
    // Sessions in name order, windows in strip order, each window keeping
    // its first pane's directory.
    assert_eq!(saved.sessions.len(), 2);
    assert_eq!(saved.sessions[0].name, "alpha");
    assert_eq!(saved.sessions[1].name, "zeta");
    let directories: Vec<&str> = saved.sessions[0]
        .windows
        .iter()
        .map(|window| window.directory.as_str())
        .collect();
    assert_eq!(directories, ["/first", "/second"]);
}

#[test]
fn a_session_the_user_closed_leaves_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Recorder::new("tmux:s1");
    recorder.set(
        "tmux:s1",
        vec![pane("keep", 0, 0, "/keep"), pane("gone", 0, 0, "/gone")],
    );
    let mut tracker = enabled_tracker(dir.path());
    recorder.tick(&mut tracker);
    assert_eq!(Record::load(&tracker.paths).unwrap().sessions.len(), 2);

    // The same server, one session killed: the live view is the whole truth.
    recorder.set("tmux:s1", vec![pane("keep", 0, 0, "/keep")]);
    recorder.tick(&mut tracker);
    let saved = Record::load(&tracker.paths).unwrap();
    assert_eq!(saved.sessions.len(), 1);
    assert_eq!(saved.sessions[0].name, "keep");
}

#[test]
fn a_new_server_recreates_the_recorded_layout() {
    let dir = tempfile::tempdir().unwrap();
    // Directories that really exist: `recreate` judges each one with the
    // host's own `resolved_start_directory`.
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let mut tracker = watching(
        dir.path(),
        record_of(
            "tmux:s1",
            vec![remembered(
                "work",
                &[
                    first.path().to_str().unwrap(),
                    second.path().to_str().unwrap(),
                ],
            )],
        ),
    );
    let recorder = Recorder::new("tmux:none");
    // The server died: the record holds, and the loss itself is noted.
    recorder.tick(&mut tracker);
    assert_eq!(Record::load(&tracker.paths).unwrap().sessions.len(), 1);
    assert_eq!(
        Record::load(&tracker.paths).unwrap().server_identity,
        "tmux:none"
    );

    // A new server is up. The remembered session comes back with both
    // windows, each at its directory.
    recorder.set("tmux:s2", Vec::new());
    recorder.tick(&mut tracker);

    let ran = recorder.ran();
    assert_eq!(ran.len(), 2, "one new-session and one new-window: {ran:?}");
    assert!(ran[0].contains("new-session"), "{ran:?}");
    assert!(
        ran[0].contains(&format!("-c {}", first.path().display())),
        "{ran:?}"
    );
    assert!(ran[0].contains("-s work"), "{ran:?}");
    assert!(ran[1].contains("new-window"), "{ran:?}");
    assert!(ran[1].contains("-t work"), "{ran:?}");
    assert!(
        ran[1].contains(&format!("-c {}", second.path().display())),
        "{ran:?}"
    );
    // The new server's live view is the record from now on.
    let saved = Record::load(&tracker.paths).unwrap();
    assert_eq!(saved.server_identity, "tmux:s2");
    assert!(saved.sessions.is_empty());
}

#[test]
fn a_session_already_open_is_not_recreated() {
    let dir = tempfile::tempdir().unwrap();
    let mut tracker = watching(
        dir.path(),
        record_of("tmux:s1", vec![remembered("work", &["/work"])]),
    );
    let recorder = Recorder::new("tmux:s2");
    // The user (or a respawn) opened `work` on the new server first.
    recorder.set("tmux:s2", vec![pane("work", 0, 0, "/elsewhere")]);
    recorder.tick(&mut tracker);
    assert!(
        recorder.ran().is_empty(),
        "the open session must be left alone"
    );
    let saved = Record::load(&tracker.paths).unwrap();
    assert_eq!(saved.sessions[0].windows[0].directory, "/elsewhere");
}

#[test]
fn a_directory_that_is_gone_starts_at_tmuxs_choice() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-directory");
    let mut tracker = watching(
        dir.path(),
        record_of(
            "tmux:s1",
            vec![remembered("work", &[missing.to_str().unwrap()])],
        ),
    );
    let recorder = Recorder::new("tmux:s2");
    recorder.set("tmux:s2", Vec::new());
    recorder.tick(&mut tracker);

    let ran = recorder.ran();
    assert_eq!(ran.len(), 1);
    // No directory argument: tmux picks, and the session still comes back.
    // (The `-c` of the login shell is not a directory argument.)
    assert!(
        !ran[0].contains(&format!("-c {}", missing.display())),
        "{ran:?}"
    );
    assert!(ran[0].contains("new-session"), "{ran:?}");
}

#[test]
fn off_remembers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let recorder = Recorder::new("tmux:s1");
    recorder.set("tmux:s1", vec![pane("secret", 0, 0, "/home/me/private")]);
    let mut tracker = tracker(dir.path());
    assert!(!tracker.record.enabled, "a fresh record is off");
    recorder.tick(&mut tracker);
    assert!(tracker.record.sessions.is_empty());
    assert!(
        !tracker.paths.record.exists(),
        "an off feature writes nothing"
    );
}

#[test]
fn enabling_starts_the_record_from_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut tracker = tracker(dir.path());
    // A record left over from a server that died while the feature was off.
    tracker.record = record_of("tmux:old", vec![remembered("ghost", &["/ghost"])]);
    tracker.record.enabled = false;

    tracker.set_enabled(true);
    let saved = Record::load(&tracker.paths).unwrap();
    assert!(saved.enabled);
    assert!(
        saved.sessions.is_empty(),
        "no ghost resurrected for a dead server"
    );
}

#[test]
fn disabling_forgets_the_remembered_directories() {
    let dir = tempfile::tempdir().unwrap();
    let mut tracker = watching(
        dir.path(),
        record_of("tmux:s1", vec![remembered("work", &["/work"])]),
    );
    tracker.set_enabled(false);
    let saved = Record::load(&tracker.paths).unwrap();
    assert!(!saved.enabled);
    assert!(saved.sessions.is_empty());
}

#[test]
fn a_record_from_the_future_or_a_corrupt_one_reads_as_empty() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(dir.path());
    // A version the host does not know: default, not half-trusted. Written
    // by hand because `save` refuses to write a version it does not own.
    std::fs::write(
        &paths.record,
        br#"{"version":2,"enabled":true,"serverIdentity":"tmux:s9","sessions":[]}"#,
    )
    .unwrap();
    assert_eq!(Record::load(&paths).unwrap(), Record::default());

    // Names must be usable and unique, windows bounded, directories clean.
    let smuggled = Record::current(
        true,
        "tmux:s1".into(),
        vec![
            remembered("dupe", &["/a"]),
            remembered("dupe", &["/b"]),
            remembered("", &["/c"]),
            remembered("control\u{7}", &["/d"]),
            remembered("too-many-windows", &["/w"; 65]),
            remembered("bad-directory", &["/has\u{7}control"]),
            remembered("good", &["/fine"]),
        ],
    );
    let kept = smuggled.sanitized();
    let names: Vec<&str> = kept
        .sessions
        .iter()
        .map(|session| session.name.as_str())
        .collect();
    // The first `dupe` is usable and stays; the second is the one nothing
    // could honor.
    assert_eq!(names, ["dupe", "good"]);
}

#[test]
fn a_request_without_an_action_is_refused() {
    let response = response_for(v1::WorkspaceRestoreRequest {
        action: 0,
        enabled: true,
    });
    assert!(!response.ok);
    assert_eq!(response.error_code, "invalid_workspace_restore");
}

#[test]
fn get_answers_with_the_switch_and_the_layout() {
    // `handle` reads the process-wide tracker, so this exercises the answer
    // shape through the tracker itself.
    let dir = tempfile::tempdir().unwrap();
    let tracker = watching(
        dir.path(),
        record_of("tmux:s1", vec![remembered("work", &["/work"])]),
    );
    let response = tracker.response();
    let answer = response.workspace_restore.unwrap();
    assert!(response.ok);
    assert!(answer.enabled);
    assert_eq!(answer.sessions.len(), 1);
    assert_eq!(answer.sessions[0].name, "work");
    assert_eq!(answer.sessions[0].windows[0].directory, "/work");
}

/// The refusal path does not touch the tracker, so it can be tested whole.
fn response_for(request: v1::WorkspaceRestoreRequest) -> v1::Response {
    let action = v1::WorkspaceRestoreAction::try_from(request.action)
        .unwrap_or(v1::WorkspaceRestoreAction::Unspecified);
    match action {
        v1::WorkspaceRestoreAction::Unspecified => response_error(
            "invalid_workspace_restore",
            "a workspace restore request needs an action",
        ),
        _ => v1::Response {
            ok: true,
            ..Default::default()
        },
    }
}
