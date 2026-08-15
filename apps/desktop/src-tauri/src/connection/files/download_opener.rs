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

use tauri::State;

use super::download_manager::DownloadManager;

/// Open the file with the user's default application.
#[tauri::command]
pub fn open_download(path: String, transfers: State<'_, DownloadManager>) -> Result<(), String> {
    let path = authorized_download(&path, &transfers)?;
    open_with_default_application(&path)
}

/// Select the file in the platform's file manager, leaving it selected rather
/// than opening it — the "Show in Finder" half of the pair.
#[tauri::command]
pub fn reveal_download(path: String, transfers: State<'_, DownloadManager>) -> Result<(), String> {
    let path = authorized_download(&path, &transfers)?;
    reveal_in_file_manager(&path)
}

fn authorized_download(path: &str, transfers: &DownloadManager) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(path);
    if !transfers.published().contains(&candidate) {
        return Err("only a download this session completed can be opened".into());
    }
    Ok(candidate)
}

#[cfg(target_os = "macos")]
fn file_url(path: &Path) -> Result<objc2::rc::Retained<objc2_foundation::NSURL>, String> {
    use objc2_foundation::{NSString, NSURL};

    let path = path
        .to_str()
        .ok_or("a download path that is not valid UTF-8 cannot be opened")?;
    Ok(NSURL::fileURLWithPath(&NSString::from_str(path)))
}

#[cfg(target_os = "macos")]
fn open_with_default_application(path: &Path) -> Result<(), String> {
    use objc2_app_kit::NSWorkspace;

    let url = file_url(path)?;
    if NSWorkspace::sharedWorkspace().openURL(&url) {
        return Ok(());
    }
    Err("macOS refused to open the download".into())
}

#[cfg(target_os = "macos")]
fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::NSArray;

    NSWorkspace::sharedWorkspace()
        .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[file_url(path)?]));
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn open_with_default_application(path: &Path) -> Result<(), String> {
    xdg_open(path)
}

/// No `xdg` verb selects a file, so the parent directory is the closest honest
/// answer on this platform.
#[cfg(not(target_os = "macos"))]
fn reveal_in_file_manager(path: &Path) -> Result<(), String> {
    xdg_open(path.parent().unwrap_or(path))
}

#[cfg(not(target_os = "macos"))]
fn xdg_open(target: &Path) -> Result<(), String> {
    use std::process::{Command, Stdio};

    Command::new("xdg-open")
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not open the download: {error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_path_this_session_published_may_be_opened() {
        let transfers = DownloadManager::default();
        assert!(authorized_download("/tmp/report.pdf", &transfers).is_err());
        assert!(authorized_download("/etc/passwd", &transfers).is_err());

        transfers
            .published()
            .record_for_test(PathBuf::from("/tmp/report.pdf"));
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
