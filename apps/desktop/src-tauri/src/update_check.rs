//! Whether a newer Muxflow release has been published.
//!
//! There is no auto-update: this only reads the small manifest the release
//! workflow attaches to every GitHub release, so the title bar can say that a
//! newer version exists and link to its release page. `releases/latest` never
//! resolves to a draft or a pre-release, so a version appears here only once it
//! has been published. The fetch happens here rather than in the webview so the
//! content security policy stays closed to every outside origin.

use std::time::Duration;

use serde::{Deserialize, Serialize};

const MANIFEST_URL: &str = "https://github.com/gal064/muxflow/releases/latest/download/latest.json";
/// The only links the notice may open, whatever the manifest says.
const RELEASE_PAGE_PREFIX: &str = "https://github.com/gal064/muxflow/releases/";
/// The manifest is a two-field object; anything larger is not it.
const MANIFEST_LIMIT_BYTES: u64 = 16 * 1024;
const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Deserialize)]
struct Manifest {
    version: String,
    url: String,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableUpdate {
    pub version: String,
    pub url: String,
}

/// `Some` when the published release is newer than the running app. A failed
/// fetch or an unreadable manifest is an error the caller may ignore; it never
/// shows a notice.
#[tauri::command]
pub async fn check_for_update(app: tauri::AppHandle) -> Result<Option<AvailableUpdate>, String> {
    let running = app.package_info().version.clone();
    let running = (running.major, running.minor, running.patch);
    // MUXFLOW_UPDATE_MANIFEST_URL points a development build at a local
    // manifest; releases always read the published one.
    let url = std::env::var("MUXFLOW_UPDATE_MANIFEST_URL").unwrap_or_else(|_| MANIFEST_URL.into());
    let body = tauri::async_runtime::spawn_blocking(move || fetch_manifest(&url))
        .await
        .map_err(|error| error.to_string())??;
    evaluate(running, &body)
}

fn fetch_manifest(url: &str) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    agent
        .get(url)
        .call()
        .map_err(|error| format!("update check failed: {error}"))?
        .body_mut()
        .with_config()
        .limit(MANIFEST_LIMIT_BYTES)
        .read_to_string()
        .map_err(|error| format!("update check failed: {error}"))
}

fn evaluate(running: (u64, u64, u64), body: &str) -> Result<Option<AvailableUpdate>, String> {
    let manifest: Manifest = serde_json::from_str(body)
        .map_err(|error| format!("unreadable update manifest: {error}"))?;
    let published = parse_version(&manifest.version)
        .ok_or_else(|| format!("unreadable update version: {:?}", manifest.version))?;
    if !manifest.url.starts_with(RELEASE_PAGE_PREFIX) {
        return Err("update manifest points outside the Muxflow releases".into());
    }
    Ok((published > running).then_some(AvailableUpdate {
        version: manifest.version,
        url: manifest.url,
    }))
}

/// A strict `X.Y.Z`: the only shape set-version.sh writes.
fn parse_version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.split('.');
    let mut next = || -> Option<u64> {
        let part = parts.next()?;
        if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(version: &str, url: &str) -> String {
        serde_json::json!({ "version": version, "url": url }).to_string()
    }

    const PAGE: &str = "https://github.com/gal064/muxflow/releases/tag/v0.2.0";

    #[test]
    fn a_newer_release_is_offered_with_its_page() {
        assert_eq!(
            evaluate((0, 1, 0), &manifest("0.2.0", PAGE)).unwrap(),
            Some(AvailableUpdate {
                version: "0.2.0".into(),
                url: PAGE.into()
            })
        );
        assert!(
            evaluate((0, 1, 9), &manifest("0.1.10", PAGE))
                .unwrap()
                .is_some()
        );
        assert!(
            evaluate((1, 9, 9), &manifest("2.0.0", PAGE))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn the_same_or_an_older_release_offers_nothing() {
        assert_eq!(evaluate((0, 2, 0), &manifest("0.2.0", PAGE)).unwrap(), None);
        assert_eq!(evaluate((0, 3, 0), &manifest("0.2.0", PAGE)).unwrap(), None);
        assert_eq!(
            evaluate((0, 1, 10), &manifest("0.1.9", PAGE)).unwrap(),
            None
        );
    }

    #[test]
    fn a_link_outside_the_muxflow_releases_is_refused() {
        for url in [
            "https://example.com/muxflow",
            "http://github.com/gal064/muxflow/releases/tag/v0.2.0",
            "https://github.com/gal064/muxflow-evil/releases/tag/v0.2.0",
            "https://github.com.evil.example/gal064/muxflow/releases/",
        ] {
            assert!(
                evaluate((0, 1, 0), &manifest("0.2.0", url)).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn only_a_strict_version_is_understood() {
        for version in [
            "0.2",
            "0.2.0.1",
            "v0.2.0",
            "0.2.0-rc.1",
            "0.2.x",
            "",
            "0..1",
            "+1.0.0",
        ] {
            assert!(
                evaluate((0, 1, 0), &manifest(version, PAGE)).is_err(),
                "{version}"
            );
        }
        assert!(evaluate((0, 1, 0), "not json").is_err());
        assert!(evaluate((0, 1, 0), r#"{"version":"0.2.0"}"#).is_err());
    }
}
