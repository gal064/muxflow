//! The native app resolves the same directories as the Tauri app did. These
//! expected values are that app's golden ones. The environment is set on a
//! child test process, since other tests read `HOME`.

use std::process::Command;

use desktop_core::platform::AppPaths;
use muxflow::paths::NativePaths;

fn run_child(xdg: &[(&str, &str)]) {
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--ignored",
            "--exact",
            "--test-threads=1",
            "native_dirs_match",
        ])
        .env("HOME", "/fixture-home")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .envs(xdg.iter().copied());
    let output = child.output().expect("run child test");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "child test failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn native_app_dirs_are_tauris() {
    run_child(&[]);
    #[cfg(target_os = "linux")]
    run_child(&[
        ("XDG_CONFIG_HOME", "/xdg/config"),
        ("XDG_DATA_HOME", "/xdg/data"),
    ]);
}

#[test]
#[ignore = "run by native_app_dirs_are_tauris with a fixed environment"]
fn native_dirs_match() {
    let paths = NativePaths;

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

    #[cfg(target_os = "macos")]
    let (config, data, log) = (
        "/fixture-home/Library/Application Support/dev.muxflow.desktop",
        "/fixture-home/Library/Application Support/dev.muxflow.desktop",
        "/fixture-home/Library/Logs/dev.muxflow.desktop",
    );

    assert_eq!(paths.app_config_dir().unwrap().to_str().unwrap(), config);
    assert_eq!(paths.app_data_dir().unwrap().to_str().unwrap(), data);
    assert_eq!(paths.app_log_dir().unwrap().to_str().unwrap(), log);
}
