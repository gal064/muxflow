use std::{
    io::{Read, Write},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use tmux_control::{ControlParser, ControlRecord, discover_with_socket_name, parse_layout};

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(1);

struct IsolatedTmux {
    socket_name: String,
    ordinary_client: Option<Child>,
}

impl IsolatedTmux {
    fn start() -> Option<Self> {
        if Command::new("tmux").arg("-V").output().is_err() {
            eprintln!("skipping local tmux integration test: tmux unavailable");
            return None;
        }
        let socket_name = format!(
            "ade-phase0-{}-{}",
            std::process::id(),
            NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
        );
        let status = Command::new("tmux")
            .env_remove("TMUX")
            .args([
                "-L",
                &socket_name,
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-x",
                "120",
                "-y",
                "40",
                "-s",
                "phase0",
            ])
            .status()
            .unwrap();
        assert!(status.success());
        Some(Self {
            socket_name,
            ordinary_client: None,
        })
    }

    fn tmux(&self, arguments: &[&str]) {
        let status = Command::new("tmux")
            .env_remove("TMUX")
            .args(["-L", &self.socket_name])
            .args(arguments)
            .status()
            .unwrap();
        assert!(status.success(), "tmux command failed: {arguments:?}");
    }

    fn attach_ordinary_client(&mut self) {
        let command = format!(
            "env -u TMUX TERM=xterm-256color COLUMNS=90 LINES=30 tmux -L {} attach-session -t phase0",
            self.socket_name
        );
        self.ordinary_client = Some(
            Command::new("script")
                .args(["-q", "-c", &command, "/dev/null"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let output = Command::new("tmux")
                .env_remove("TMUX")
                .args([
                    "-L",
                    &self.socket_name,
                    "list-clients",
                    "-F",
                    "#{client_control_mode}",
                ])
                .output()
                .unwrap();
            if String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == "0")
            {
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("ordinary tmux client did not attach");
    }

    fn assert_compound_capture_metadata(&self, pane_id: &str) {
        let mut client = Command::new("tmux")
            .env_remove("TMUX")
            .args([
                "-L",
                &self.socket_name,
                "-C",
                "attach-session",
                "-t",
                "phase0",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = client.stdin.take().unwrap();
        writeln!(
            stdin,
            "capture-pane -p -e -S -2000 -t {pane_id} ; display-message -p -t {pane_id} '__ADE_META__:{pane_id}:#{{cursor_x}}:#{{cursor_y}}:#{{alternate_on}}'"
        )
        .unwrap();
        stdin.flush().unwrap();
        thread::sleep(Duration::from_millis(100));
        client.kill().unwrap();

        let mut bytes = Vec::new();
        client
            .stdout
            .take()
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        client.wait().unwrap();
        let mut parser = ControlParser::default();
        parser.push(&bytes);
        let mut found = false;
        while let Some(record) = parser.next_record() {
            if let Ok(ControlRecord::CommandOutput(line)) = record {
                found |= line.starts_with(format!("__ADE_META__:{pane_id}:").as_bytes());
            }
        }
        assert!(found, "compound capture did not return cursor metadata");
    }
}

impl Drop for IsolatedTmux {
    fn drop(&mut self) {
        if let Some(client) = &mut self.ordinary_client {
            let _ = client.kill();
            let _ = client.wait();
        }
        let _ = Command::new("tmux")
            .env_remove("TMUX")
            .args(["-L", &self.socket_name, "kill-server"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[test]
fn discovers_splits_and_coexists_with_an_ordinary_client() {
    let Some(mut server) = IsolatedTmux::start() else {
        return;
    };
    server.tmux(&["split-window", "-h", "-t", "phase0:0"]);
    server.tmux(&[
        "send-keys",
        "-t",
        "phase0:0.0",
        "printf 'unicode: λ 🚀\\n'",
        "Enter",
    ]);

    let snapshot = discover_with_socket_name(&server.socket_name).unwrap();
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.windows.len(), 1);
    assert_eq!(snapshot.panes.len(), 2);
    parse_layout(&snapshot.windows[0].layout).unwrap();
    server.assert_compound_capture_metadata(&snapshot.panes[0].id);

    server.attach_ordinary_client();
    server.tmux(&["split-window", "-v", "-t", "phase0:0.1"]);

    let updated = discover_with_socket_name(&server.socket_name).unwrap();
    assert_eq!(updated.panes.len(), 3);
    parse_layout(&updated.windows[0].layout).unwrap();
}
