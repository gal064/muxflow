//! Open a finished download, or show it where it landed.
//!
//! A download used to end in a toast naming the *remote* source path, with
//! nothing to click. These are the two things a person actually wants next.
//!
//! The security boundary is `PublishedDownloads`: neither command accepts a
//! path on the renderer's word. Handing the webview a general "open this with
//! the default application" primitive would turn any string it can produce
//! into an execution request — a rendered markdown link, a filename from the
//! remote host, an error message. Instead the download manager remembers what
//! it published, and a path that is not in that set is refused before any
//! platform call happens. The set only ever contains files this app wrote.

use std::path::{Path, PathBuf};

use super::download_manager::DownloadManager;

/// Open the file with the user's default application.
pub fn open_download(path: String, transfers: &DownloadManager) -> Result<(), String> {
    let path = authorized_download(&path, transfers)?;
    open_with_default_application(&path)
}

/// Select the file in the platform's file manager, leaving it selected rather
/// than opening it — the "Show in Finder" half of the pair.
pub fn reveal_download(path: String, transfers: &DownloadManager) -> Result<(), String> {
    let path = authorized_download(&path, transfers)?;
    reveal_in_file_manager(&path)
}

fn authorized_download(path: &str, transfers: &DownloadManager) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(path);
    if !transfers.published().contains(&candidate) {
        return Err("only a download this session completed can be opened".into());
    }
    Ok(candidate)
}

// Both platforms go through a child process. On macOS,
// `activateFileViewerSelectingURLs` spins a nested run loop while it waits on
// Finder; called from a click handler, that loop runs queued gpui tasks while
// the app is still borrowed, and the second borrow aborts the process.

#[cfg(target_os = "macos")]
fn open_with_default_application(path: &Path) -> Result<(), String> {
    launch("/usr/bin/open", &[], path)
}

#[cfg(target_os = "macos")]
fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    launch("/usr/bin/open", &["-R"], path)
}

#[cfg(not(target_os = "macos"))]
fn open_with_default_application(path: &Path) -> Result<(), String> {
    launch("xdg-open", &[], path)
}

/// No `xdg` verb selects a file, so the parent directory is the closest honest
/// answer on this platform.
#[cfg(not(target_os = "macos"))]
fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    launch("xdg-open", &[], path.parent().unwrap_or(path))
}

fn launch(program: &str, flags: &[&str], target: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};

    // The opener runs detached, so a file moved or deleted since the download
    // would otherwise fail without a word.
    if !target.exists() {
        return Err("the download is no longer where it was saved".into());
    }
    let mut command = Command::new(program);
    command
        .args(flags)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    spawn_reaped(command).map_err(|error| format!("could not open the download: {error}"))?;
    Ok(())
}

/// Starts a program without waiting for it, and waits on a thread of its own:
/// a child nobody waits for stays a zombie until the app exits.
fn spawn_reaped(mut command: std::process::Command) -> std::io::Result<u32> {
    let mut child = command.spawn()?;
    let pid = child.id();
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn an_opener_that_exits_is_reaped() {
        let pid = spawn_reaped(std::process::Command::new("true")).unwrap();
        let entry = std::path::PathBuf::from(format!("/proc/{pid}"));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while entry.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(!entry.exists(), "the exited opener was left a zombie");
    }

    #[test]
    fn only_a_path_this_session_published_may_be_opened() {
        let transfers = DownloadManager::default();
        assert!(authorized_download("/tmp/report.pdf", &transfers).is_err());
        assert!(authorized_download("/etc/passwd", &transfers).is_err());

        transfers
            .published()
            .record(PathBuf::from("/tmp/report.pdf"));
        assert_eq!(
            authorized_download("/tmp/report.pdf", &transfers).unwrap(),
            PathBuf::from("/tmp/report.pdf")
        );
        // Neither a neighbour nor a traversal that merely resolves to the same
        // file is the recorded path.
        assert!(authorized_download("/etc/passwd", &transfers).is_err());
        assert!(authorized_download("/tmp/../tmp/report.pdf", &transfers).is_err());
        assert!(authorized_download("/tmp/report.pdf.bak", &transfers).is_err());
    }
}
