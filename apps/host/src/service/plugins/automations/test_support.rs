//! A private tmux server per test, and a definition to run on it.

use std::{
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

use super::{
    cron,
    store::{Agent, Definition},
};

static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(1);

pub(super) struct TmuxFixture {
    socket: String,
}

impl TmuxFixture {
    pub(super) fn new() -> Self {
        let fixture_id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
        let fixture = Self {
            socket: format!("ade-automations-test-{}-{fixture_id}", std::process::id()),
        };
        let _ = fixture.command().arg("kill-server").output();
        fixture
    }

    fn command(&self) -> Command {
        client(&self.socket)
    }

    pub(super) fn factory(&self) -> impl Fn() -> anyhow::Result<Command> + use<> {
        let socket = self.socket.clone();
        move || Ok(client(&socket))
    }

    pub(super) fn run(&self, arguments: &[&str]) -> Output {
        let output = self
            .command()
            .args(arguments)
            .output()
            .expect("tmux must be installed for automation tests");
        assert!(
            output.status.success(),
            "tmux {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    pub(super) fn display(&self, target: &str, format: &str) -> String {
        String::from_utf8_lossy(
            &self
                .run(&["display-message", "-p", "-t", target, format])
                .stdout,
        )
        .trim()
        .to_owned()
    }

    pub(super) fn server_running(&self) -> bool {
        self.command()
            .arg("list-sessions")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    pub(super) fn wait_until(&self, mut predicate: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !predicate() {
            assert!(Instant::now() < deadline, "condition not reached in time");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// A client of a fixture's server. `SHELL` is pinned so every pane runs the
/// same plain shell whatever the developer's login shell is.
fn client(socket: &str) -> Command {
    let mut command = Command::new("tmux");
    command
        .env_remove("TMUX")
        .env("SHELL", "/bin/sh")
        .env("ENV", "/dev/null")
        .args(["-L", socket, "-f", "/dev/null"]);
    command
}

impl Drop for TmuxFixture {
    fn drop(&mut self) {
        let _ = self.command().arg("kill-server").output();
    }
}

pub(super) fn definition(agent: Agent, command: &str) -> Definition {
    Definition {
        id: "ci".into(),
        name: "Fix CI".into(),
        enabled: true,
        schedule: cron::parse("* * * * *", 2026).unwrap(),
        cwd: "/tmp".into(),
        agent,
        command: command.into(),
        prompt: "unused".into(),
        keep_tabs: 5,
        max_runtime_minutes: 120,
    }
}
