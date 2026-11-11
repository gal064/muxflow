//! The update check and install behind the title bar's update pill. Port of
//! `features/shell/useUpdateCheck.ts` over `desktop_core::update_check`.
//!
//! The check runs at launch and then daily, so a release is at most a day late
//! to be noticed. A failed check (offline, GitHub unreachable, an unreadable
//! manifest) changes nothing: the next successful check decides, never a
//! network error.

use std::{io::IsTerminal, path::PathBuf, rc::Rc, time::Duration};

use desktop_core::update_check::{self, AvailableUpdate};
use futures::future::LocalBoxFuture;
use gpui_kit::{App, Global, Window};

use crate::commands::dialogs::{Confirmation, open_confirmation};

pub const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

type Check = Rc<dyn Fn() -> LocalBoxFuture<'static, Result<Option<AvailableUpdate>, String>>>;
type Install = Rc<dyn Fn() -> LocalBoxFuture<'static, Result<(), String>>>;

/// How the check and the install reach the outside, so a test never does.
#[derive(Clone)]
pub struct Updater {
    pub check: Check,
    pub install: Install,
}

struct Update {
    updater: Updater,
    available: Option<AvailableUpdate>,
    installing: bool,
}

impl Global for Update {}

/// The release to offer, or `None` while the running app is the newest.
pub fn available(cx: &App) -> Option<AvailableUpdate> {
    cx.try_global::<Update>()?.available.clone()
}

/// The real check and install. Called from [`crate::app::run`] only, so a test
/// window never reaches the network.
pub fn start(cx: &mut App) {
    let running = running_version(env!("CARGO_PKG_VERSION"));
    start_with(
        Updater {
            check: Rc::new(move || Box::pin(update_check::check_for_update(running))),
            install: Rc::new(|| Box::pin(update_check::install_update())),
        },
        cx,
    );
    // Read now: once the installer swaps the package in, this path names the
    // new version, while the running process's own link may not.
    let exe = std::env::current_exe().ok();
    cx.on_app_quit(move |_| {
        relaunch(exe.clone());
        async {}
    })
    .detach();
}

/// Starts the periodic check with `updater`.
pub fn start_with(updater: Updater, cx: &mut App) {
    let check = updater.check.clone();
    cx.set_global(Update {
        updater,
        available: None,
        installing: false,
    });
    cx.spawn(async move |cx| {
        loop {
            if let Ok(update) = check().await {
                cx.update(|cx| {
                    cx.global_mut::<Update>().available = update;
                    cx.refresh_windows();
                });
            }
            cx.background_executor().timer(INTERVAL).await;
        }
    })
    .detach();
}

/// The pill's click: installs the offered release once the user agrees to the
/// restart. On any failure the release page is the way to update.
pub fn install(window: &mut Window, cx: &mut App) {
    let Some(update) = cx
        .try_global::<Update>()
        .filter(|update| !update.installing)
    else {
        return;
    };
    let Some(offered) = update.available.clone() else {
        return;
    };
    open_confirmation(
        Confirmation {
            title: "Update Muxflow".into(),
            detail: format!(
                "Install Muxflow {} and restart? Port forwards and file transfers in progress will stop.",
                offered.version
            )
            .into(),
            confirm_label: "Update and restart".into(),
            destructive: false,
            on_confirm: Rc::new(move |_, cx| run_install(offered.clone(), cx)),
        },
        window,
        cx,
    );
}

fn run_install(update: AvailableUpdate, cx: &mut App) {
    let state = cx.global_mut::<Update>();
    if std::mem::replace(&mut state.installing, true) {
        return;
    }
    let install = state.updater.install.clone();
    crate::editor::report(format!("Updating to Muxflow {}…", update.version), cx);
    // Before anything is replaced: a save that fails here leaves the app as it was.
    let flush = crate::editor::flush_all(cx);
    cx.spawn(async move |cx| {
        let result = match flush.await {
            Ok(()) => install().await,
            Err(error) => Err(error),
        };
        cx.update(|cx| {
            cx.global_mut::<Update>().installing = false;
            match result {
                // Quitting goes through the normal save path; the exit relaunches.
                Ok(()) => crate::editor::quit(cx),
                Err(error) => {
                    crate::editor::report(error, cx);
                    cx.open_url(&update.url);
                }
            }
        });
    })
    .detach();
}

/// Passed to a relaunched process, which then waits for its predecessor. An
/// argument rather than the environment, so nothing the new app starts inherits it.
const RELAUNCHED: &str = "--relaunched";

/// Starts the updated app as this one quits. Quit handlers are still saving
/// state when this runs, so the new process waits until the old one has
/// exited: it holds a pipe this process keeps open until then.
fn relaunch(exe: Option<PathBuf>) {
    if !update_check::relaunch_on_exit() {
        return;
    }
    let Some(exe) = exe else {
        return;
    };
    let child = std::process::Command::new(exe)
        .arg(RELAUNCHED)
        .stdin(std::process::Stdio::piped())
        .spawn();
    if let Some(pipe) = child.ok().and_then(|mut child| child.stdin.take()) {
        // Closed by the exit, never before.
        std::mem::forget(pipe);
    }
}

/// Called first in [`crate::app::run`]: a relaunched app starts once the one
/// it replaces has exited, so it reads the state that one saved last.
pub fn wait_for_predecessor() {
    let stdin = std::io::stdin();
    if std::env::args_os().any(|arg| arg == RELAUNCHED) && !stdin.is_terminal() {
        wait_for_close(stdin.lock());
    }
}

/// Returns when the writer of `pipe` is gone.
fn wait_for_close(mut pipe: impl std::io::Read) {
    let _ = std::io::copy(&mut pipe, &mut std::io::sink());
}

/// `X.Y.Z` as the check compares it. An unversioned build reads as 0.0.0, so
/// every published release is offered to it.
fn running_version(version: &str) -> (u64, u64, u64) {
    let mut parts = version.split('.').map(|part| part.parse().unwrap_or(0));
    let mut next = || parts.next().unwrap_or(0);
    (next(), next(), next())
}

#[cfg(test)]
mod tests {
    use super::{running_version, wait_for_close};

    #[test]
    fn an_unversioned_build_reads_as_zero() {
        assert_eq!(running_version("0.1.31"), (0, 1, 31));
        assert_eq!(running_version("0.0.0"), (0, 0, 0));
        assert_eq!(running_version("x"), (0, 0, 0));
    }

    #[test]
    fn a_relaunch_waits_until_the_old_process_lets_go() {
        let (reader, mut writer) = std::io::pipe().unwrap();
        let waiting = std::thread::spawn(move || wait_for_close(reader));
        std::io::Write::write_all(&mut writer, b"anything").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!waiting.is_finished(), "returned while the writer lives");
        drop(writer);
        waiting.join().unwrap();
    }
}
