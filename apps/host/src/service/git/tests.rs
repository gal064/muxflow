use super::*;
use std::{
    fs,
    os::unix::{ffi::OsStringExt, fs::PermissionsExt},
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture {
    root: PathBuf,
}

static COMMIT_HOOK_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::current_dir()
            .unwrap()
            .join("tmp")
            .join(format!("phase5-unit-{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let fixture = Self { root };
        fixture.git(&["init", "-q"]);
        fixture.git(&["config", "user.name", "Phase Five"]);
        fixture.git(&["config", "user.email", "phase5@example.test"]);
        fixture
    }

    fn git(&self, args: &[&str]) -> Output {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn write(&self, path: &str, value: &[u8]) {
        let full = self.root.join(path);
        if let Some(parent) = full.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full, value).unwrap();
    }

    fn request(&self) -> v1::GitRequest {
        let root = self.root.to_str().unwrap().to_owned();
        v1::GitRequest {
            root_token: super::super::filesystem::root_token(&root).unwrap(),
            root,
            expected_server_identity: server_identity(),
            operation_id: "test".into(),
            ..Default::default()
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

mod capability_safety;
mod mutation;
mod status_diff;
mod watch_reconnect;
