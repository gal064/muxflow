//! Open an `http(s)` URL in the user's default browser.
//!
//! One boundary for every surface that can show a link — Markdown preview,
//! terminal text, OSC 8 hyperlinks: the string is validated here before any
//! platform call, so a `file:`, `javascript:` or credential-bearing URL that a
//! document or a process could print is refused. `confirmed` is the caller's
//! statement that the user made a deliberate gesture (a dialog, or the
//! modifier-click a terminal link requires).

const MAX_URL_BYTES: usize = 8192;

#[tauri::command]
pub fn open_external_link(url: String, confirmed: bool) -> Result<(), String> {
    validate_external_url(&url)?;
    if !confirmed {
        return Err("external_link_confirmation_required".into());
    }
    open_in_browser(&url)
}

#[cfg(target_os = "macos")]
fn open_in_browser(url: &str) -> Result<(), String> {
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::{NSString, NSURL};

    let Some(target) = NSURL::URLWithString(&NSString::from_str(url)) else {
        return Err("macOS could not parse the link".into());
    };
    if NSWorkspace::sharedWorkspace().openURL(&target) {
        return Ok(());
    }
    Err("macOS refused to open the link".into())
}

#[cfg(not(target_os = "macos"))]
fn open_in_browser(url: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};

    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not open external link: {error}"))?;
    Ok(())
}

fn validate_external_url(url: &str) -> Result<(), String> {
    if url.is_empty()
        || url.len() > MAX_URL_BYTES
        || url
            .chars()
            .any(|value| value.is_control() || value.is_whitespace())
        || url.contains('\\')
        || url.bytes().any(|byte| byte == 0)
    {
        return Err("invalid external URL".into());
    }
    let remainder = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or("only absolute http(s) links may be opened externally")?;
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || authority.starts_with('.') {
        return Err("external URL must have a host and cannot contain credentials".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_url_boundary_rejects_relative_active_and_credential_urls() {
        assert!(validate_external_url("https://example.com/docs?q=one").is_ok());
        assert!(validate_external_url("http://localhost:3000/").is_ok());
        for rejected in [
            "README.md",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "data:text/html,x",
            "https://user:password@example.com/",
            "https:///missing-host",
        ] {
            assert!(
                validate_external_url(rejected).is_err(),
                "accepted {rejected}"
            );
        }
    }
}
