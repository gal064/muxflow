//! Golden tests for the directories Tauri resolves for this app. The native
//! GPUI app must use the same directories so profiles, app state and logs carry
//! over. Expected values come from tauri 2.11 `path/desktop.rs`:
//! `app_config_dir` = config_dir/ID, `app_data_dir` = data_dir/ID,
//! `app_log_dir` = data_local_dir/ID/logs (Linux) or ~/Library/Logs/ID (macOS).
//! `dirs` honours absolute XDG_CONFIG_HOME / XDG_DATA_HOME on Linux.
//!
//! Tests in a crate share one process, and other tests read `HOME`, so the
//! environment is set on a child test process and never changed in this one.

use std::process::Command;

use crate::{platform::AppPaths, tauri_adapters::TauriApp};

const IDENTIFIER: &str = "dev.muxflow.desktop";

fn run_child(xdg: &[(&str, &str)]) {
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--ignored",
            "--exact",
            "--test-threads=1",
            "golden_paths::resolved_dirs_match",
        ])
        .env("HOME", "/fixture-home")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .envs(xdg.iter().copied());
    let output = child.output().expect("run child test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // A wrong filter runs nothing and still exits 0.
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "child test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn tauri_app_dirs_are_pinned() {
    run_child(&[]);
    #[cfg(target_os = "linux")]
    run_child(&[
        ("XDG_CONFIG_HOME", "/xdg/config"),
        ("XDG_DATA_HOME", "/xdg/data"),
    ]);
}

#[test]
#[ignore = "run by tauri_app_dirs_are_pinned with a fixed environment"]
fn resolved_dirs_match() {
    let app = tauri::test::mock_builder()
        .build(tauri::generate_context!())
        .expect("build mock app");
    assert_eq!(app.config().identifier, IDENTIFIER);
    let path: &dyn AppPaths = &TauriApp(app.handle().clone());

    #[cfg(target_os = "linux")]
    let (config, data, log) = if std::env::var_os("XDG_CONFIG_HOME").is_some() {
        (
            "/xdg/config/dev.muxflow.desktop",
            "/xdg/data/dev.muxflow.desktop",
            "/xdg/data/dev.muxflow.desktop/logs",
        )
    } else {
        (
            "/fixture-home/.config/dev.muxflow.desktop",
            "/fixture-home/.local/share/dev.muxflow.desktop",
            "/fixture-home/.local/share/dev.muxflow.desktop/logs",
        )
    };

    // Not run locally: written from the tauri/dirs source, CI runs it on macOS.
    #[cfg(target_os = "macos")]
    let (config, data, log) = (
        "/fixture-home/Library/Application Support/dev.muxflow.desktop",
        "/fixture-home/Library/Application Support/dev.muxflow.desktop",
        "/fixture-home/Library/Logs/dev.muxflow.desktop",
    );

    assert_eq!(path.app_config_dir().unwrap().to_str().unwrap(), config);
    assert_eq!(path.app_data_dir().unwrap().to_str().unwrap(), data);
    assert_eq!(path.app_log_dir().unwrap().to_str().unwrap(), log);
}
