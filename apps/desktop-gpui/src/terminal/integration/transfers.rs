//! What a file or image pasted or dropped into a terminal becomes: port of the
//! logic in `terminalTransfers.ts` and the record rules of
//! `terminalTransferRegistry.ts`. No GPUI here, so all of it is table-tested.

pub use desktop_core::connection::files::upload_manager::UploadCollisionPolicy;
use serde::Deserialize;
use serde_json::Value;

/// Above this, an upload has to be confirmed.
pub const LARGE_UPLOAD_BYTES: u64 = 500 * 1024 * 1024;
/// The most records the upload list keeps, newest first.
const MAX_RECORDS: usize = 100;

/// POSIX single-quote escaping. A relative path starting with a dash is made
/// explicitly relative, so a command cannot read it as an option.
pub fn shell_escape_path(path: &str) -> Result<String, String> {
    if path.is_empty() || path.contains('\0') {
        return Err("Cannot paste an empty or NUL-containing path.".into());
    }
    let safe = if path.starts_with('-') && !path.contains('/') {
        format!("./{path}")
    } else {
        path.to_owned()
    };
    Ok(format!("'{}'", safe.replace('\'', r#"'"'"'"#)))
}

pub fn join_shell_escaped_paths(paths: &[String]) -> Result<String, String> {
    if paths.is_empty() {
        return Err("No file paths were provided.".into());
    }
    let escaped = paths
        .iter()
        .map(|path| shell_escape_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(escaped.join(" "))
}

/// A path dragged from inside the app, such as an Explorer row: port of
/// `internalPathDrag.ts`. GPUI carries it in-process as the drag's value, so
/// the MIME payload, gesture ids and the native-lane bridge have nothing left
/// to do; the host check and the path rules stay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternalPathDrag {
    pub host_profile_id: String,
    pub server_identity: String,
    pub path: String,
}

impl InternalPathDrag {
    /// The text a drop pastes into a terminal on `target`, a host profile and
    /// its server identity, or why it pastes nothing.
    pub fn shell_text(&self, target: Option<(&str, &str)>) -> Result<String, String> {
        let (profile, server) = target.ok_or("The target terminal is disconnected.")?;
        let named = |value: &str| !value.is_empty() && !value.contains('\0');
        if !named(&self.host_profile_id)
            || !named(&self.server_identity)
            || !is_canonical_absolute_path(&self.path)
        {
            return Err("The internal path drag payload is invalid.".into());
        }
        if self.host_profile_id != profile || self.server_identity != server {
            return Err("Paths can only be dropped into a terminal on the same host.".into());
        }
        shell_escape_path(&self.path)
    }
}

/// `/a/b`, with no empty, `.` or `..` part. The root alone is not a file.
fn is_canonical_absolute_path(path: &str) -> bool {
    path.strip_prefix('/').is_some_and(|rest| {
        !rest.contains('\0')
            && rest
                .split('/')
                .all(|part| !part.is_empty() && part != "." && part != "..")
    })
}

/// Image staging paths are pasted raw, because Codex and Claude Code recognize
/// an attachment only by its bare path.
pub fn validate_agent_image_path(path: &str) -> Result<&str, String> {
    let unsafe_byte = path
        .bytes()
        .any(|byte| byte <= 0x20 || byte == 0x7f || matches!(byte, b'\'' | b'"' | b'\\'));
    if !path.starts_with('/') || unsafe_byte {
        return Err("The staged image path is not agent-compatible.".into());
    }
    Ok(path)
}

/// Local paths from a `text/uri-list`: `file:` URIs on this machine only.
pub fn parse_file_uri_list(value: &str) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    for line in value.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(rest) = line
            .get(..5)
            .filter(|scheme| scheme.eq_ignore_ascii_case("file:"))
            .map(|_| &line[5..])
        else {
            return Err("Only local file URIs can be pasted into a terminal.".into());
        };
        let path = match rest.strip_prefix("//") {
            Some(authority) => {
                let slash = authority.find('/').unwrap_or(authority.len());
                let host = &authority[..slash];
                if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
                    return Err("Only local file URIs can be pasted into a terminal.".into());
                }
                &authority[slash..]
            }
            None => rest,
        };
        // Query and fragment are not part of a file's path.
        let path = path.split(['?', '#']).next().unwrap_or_default();
        if !path.starts_with('/') {
            return Err("The clipboard file list contains an invalid URI.".into());
        }
        paths.push(percent_decode(path)?);
    }
    Ok(paths)
}

fn percent_decode(text: &str) -> Result<String, String> {
    let invalid = || "The clipboard file list contains an invalid URI.".to_owned();
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = text.get(index + 1..index + 3).ok_or_else(invalid)?;
            decoded.push(u8::from_str_radix(hex, 16).map_err(|_| invalid())?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| invalid())
}

pub fn basename(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    let name = &trimmed[trimmed.rfind('/').map_or(0, |slash| slash + 1)..];
    if name.is_empty() {
        "upload".into()
    } else {
        name.into()
    }
}

/// What the native clipboard held, as `read_native_terminal_clipboard` says.
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum NativeClipboard {
    Files { uris: Vec<String> },
    Image { staged: StagedImage },
    Text { text: String },
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StagedImage {
    pub path: String,
    pub name: String,
}

impl NativeClipboard {
    pub fn from_wire(value: Value) -> Result<Self, String> {
        let clipboard: Self = serde_json::from_value(value)
            .map_err(|_| "The native clipboard returned an unknown payload.".to_owned())?;
        if let Self::Image { staged } = &clipboard {
            validate_clipboard_name(&staged.name)?;
        }
        Ok(clipboard)
    }
}

/// A staged image is `clipboard-<uuid v1..v8>.png`, which agents recognize and
/// which never collides on the host.
fn validate_clipboard_name(name: &str) -> Result<(), String> {
    let refused = || "The clipboard staging service did not return a unique agent-compatible PNG destination.";
    let uuid = name
        .strip_prefix("clipboard-")
        .and_then(|rest| rest.strip_suffix(".png"))
        .ok_or_else(refused)?;
    let groups: Vec<&str> = uuid.split('-').collect();
    let shaped = groups.iter().map(|group| group.len()).eq([8, 4, 4, 4, 12])
        && uuid
            .bytes()
            .all(|byte| byte == b'-' || matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    let version = groups.get(2).and_then(|group| group.bytes().next());
    let variant = groups.get(3).and_then(|group| group.bytes().next());
    if shaped
        && version.is_some_and(|byte| (b'1'..=b'8').contains(&byte))
        && variant.is_some_and(|byte| matches!(byte, b'8' | b'9' | b'a' | b'b'))
    {
        Ok(())
    } else {
        Err(refused().into())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TransferState {
    Queued,
    Preflighting,
    Running,
    Verifying,
    Completed,
    Cancelled,
    Failed,
}

impl TransferState {
    pub fn finished(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Preflighting => "Checking",
            Self::Running => "Uploading",
            Self::Verifying => "Verifying",
            Self::Completed => "Completed",
            Self::Cancelled => "Cancelled",
            Self::Failed => "Failed",
        }
    }
}

/// One upload or preflight event from `desktop-core`, the fields this surface
/// reads. Byte counts are decimal strings on the wire.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TransferEvent {
    pub transfer_id: String,
    #[serde(default)]
    pub source_path: String,
    #[serde(default)]
    pub name: String,
    pub state: TransferState,
    #[serde(default)]
    pub transferred_bytes: Option<String>,
    #[serde(default)]
    pub total_bytes: Option<String>,
    /// Present on a preflight's events.
    #[serde(default)]
    pub size_bytes: Option<String>,
    #[serde(default)]
    pub destination: Option<String>,
    /// The digest the host verified the upload against.
    #[serde(default)]
    pub blake3: Option<String>,
    /// `published`, `notPublished` or `unknown`.
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub collision: bool,
    #[serde(default)]
    pub collision_renamed: bool,
    #[serde(default)]
    pub source_kind: Option<String>,
    #[serde(default)]
    pub readable: Option<bool>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub cleanup_error: Option<String>,
    #[serde(default)]
    pub cleanup_status: Option<String>,
    pub server_identity: String,
    pub connection_epoch: String,
}

impl TransferEvent {
    pub fn parse(json: &str) -> Result<Self, String> {
        let mut event: Self = serde_json::from_str(json)
            .map_err(|error| format!("The host returned an invalid transfer event: {error}"))?;
        // The host's messages are proto strings, empty when there is nothing
        // to say: every clean completion carries `"cleanupError": ""`.
        for message in [&mut event.error, &mut event.cleanup_error] {
            if message.as_deref() == Some("") {
                *message = None;
            }
        }
        Ok(event)
    }

    /// An event for a connection other than the one the batch started on is a
    /// stale scope, never progress.
    pub fn belongs_to(&self, scope: &TransferScope) -> bool {
        self.server_identity == scope.server_identity
            && self.connection_epoch == scope.terminal_epoch.to_string()
    }

    /// The failure a finished event reports, if it is one.
    pub fn failure(&self) -> Option<String> {
        match self.state {
            TransferState::Failed => Some(
                self.error
                    .clone()
                    .unwrap_or_else(|| "Upload failed.".into()),
            ),
            TransferState::Cancelled => Some("Upload cancelled.".into()),
            _ => None,
        }
    }

    /// Where a completed upload landed, only when the host verified it: a
    /// digest, and an outcome that is not in doubt. Port of the check in
    /// `terminalTransferApi.ts`'s `start`.
    pub fn verified_destination(&self) -> Option<String> {
        let verified = self.state == TransferState::Completed
            && self
                .blake3
                .as_deref()
                .is_some_and(|digest| !digest.is_empty())
            && self.outcome.as_deref() != Some("unknown");
        verified.then(|| self.destination.clone()).flatten()
    }

    /// A finished upload that left nothing on the host to tell the user about.
    pub fn delivered_cleanly(&self) -> bool {
        self.state == TransferState::Completed
            && self.cleanup_error.is_none()
            && self
                .cleanup_status
                .as_deref()
                .is_none_or(|status| matches!(status, "notNeeded" | "removed"))
    }

    /// `"3.0 MiB / 10.0 MiB"`, or how much moved when the total is unknown.
    pub fn progress_text(&self) -> String {
        let done = decimal(self.transferred_bytes.as_deref()).unwrap_or(0);
        match decimal(self.total_bytes.as_deref()) {
            Some(total) => format!("{} / {}", format_bytes(done), format_bytes(total)),
            None => format!("{} transferred", format_bytes(done)),
        }
    }
}

fn decimal(value: Option<&str>) -> Option<u64> {
    value?.parse().ok()
}

/// A preflight's answer about one source, once it completed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preflight {
    pub source_path: String,
    pub name: String,
    pub size_bytes: u64,
    pub collision: bool,
}

impl Preflight {
    /// Port of `validateUploadPreflight`: only a readable regular file uploads.
    pub fn from_completed(event: &TransferEvent, name: &str) -> Result<Self, String> {
        if let Some(error) = &event.cleanup_error {
            return Err(format!("Upload preflight cleanup failed: {error}"));
        }
        let size_bytes = decimal(event.size_bytes.as_deref())
            .ok_or("Invalid source size from transfer service.")?;
        let source = &event.source_path;
        match event.source_kind.as_deref() {
            Some("directory") => {
                return Err(format!(
                    "Directories cannot be dropped into terminals: {source}"
                ));
            }
            Some("other") => {
                return Err(format!(
                    "Only regular files can be dropped into terminals: {source}"
                ));
            }
            _ => {}
        }
        if event.readable == Some(false) {
            return Err(format!("The source file is not readable: {source}"));
        }
        Ok(Self {
            source_path: event.source_path.clone(),
            name: name.to_owned(),
            size_bytes,
            collision: event.collision || event.collision_renamed,
        })
    }
}

/// What the review dialog has to ask before a batch starts, if anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    pub large: bool,
    pub collisions: bool,
}

/// Port of the check in `startRemoteBatch`: a large file, a name already on
/// the host, or two files with one name all need the user.
pub fn review_for(items: &[Preflight]) -> Option<Review> {
    let large = items
        .iter()
        .any(|item| item.size_bytes > LARGE_UPLOAD_BYTES);
    let duplicate = items
        .iter()
        .enumerate()
        .any(|(index, item)| items[..index].iter().any(|other| other.name == item.name));
    let collisions = duplicate || items.iter().any(|item| item.collision);
    (large || collisions).then_some(Review { large, collisions })
}

/// `"1.5 KiB"`, one decimal, binary units: port of `formatBytes`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [(u64, &str); 4] = [
        (1 << 40, "TiB"),
        (1 << 30, "GiB"),
        (1 << 20, "MiB"),
        (1 << 10, "KiB"),
    ];
    for (size, label) in UNITS {
        if bytes >= size {
            let tenths = u128::from(bytes) * 10 / u128::from(size);
            return format!("{}.{} {label}", tenths / 10, tenths % 10);
        }
    }
    format!("{bytes} B")
}

/// The connection an upload batch is bound to. A batch whose pane's scope
/// changes is abandoned, because its paths would land on another host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferScope {
    pub client_id: String,
    pub profile_id: String,
    pub server_identity: String,
    pub terminal_epoch: u64,
    pub pane_id: String,
    pub local: bool,
}

/// One upload, or with `preflight` its non-destructive check, for the backend.
pub struct UploadRequest {
    pub scope: TransferScope,
    pub source_path: String,
    pub destination_name: String,
    pub collision: UploadCollisionPolicy,
    pub large_upload_confirmed: bool,
    pub image_png: bool,
    pub preflight: bool,
}

/// One row of the upload list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferRecord {
    pub pane_id: String,
    pub progress: TransferEvent,
}

/// The upload list, newest first. Port of `useTerminalTransferRegistry`.
#[derive(Default)]
pub struct TransferRecords {
    pub records: Vec<TransferRecord>,
}

impl TransferRecords {
    pub fn record(&mut self, pane_id: &str, progress: TransferEvent) {
        match self
            .records
            .iter_mut()
            .find(|record| record.progress.transfer_id == progress.transfer_id)
        {
            // A late progress frame never reopens a finished record.
            Some(record) if record.progress.state.finished() && !progress.state.finished() => {}
            Some(record) => record.progress = progress,
            None => {
                self.records.insert(
                    0,
                    TransferRecord {
                        pane_id: pane_id.to_owned(),
                        progress,
                    },
                );
                self.records.truncate(MAX_RECORDS);
            }
        }
    }

    /// Clears what a batch delivered: by id, so a completion an earlier batch
    /// could not paste stays the only sign its bytes reached the host, and only
    /// when nothing was left on the host to mention.
    pub fn dismiss_delivered(&mut self, transfer_ids: &[String]) {
        self.records.retain(|record| {
            !(transfer_ids.contains(&record.progress.transfer_id)
                && record.progress.delivered_cleanly())
        });
    }

    /// The user closes a finished record; one still moving bytes stays.
    pub fn dismiss(&mut self, transfer_id: &str) {
        self.records.retain(|record| {
            !(record.progress.transfer_id == transfer_id && record.progress.state.finished())
        });
    }

    pub fn mark_verifying(&mut self, transfer_id: &str) {
        for record in &mut self.records {
            if record.progress.transfer_id == transfer_id && !record.progress.state.finished() {
                record.progress.state = TransferState::Verifying;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn escapes_paths_for_a_posix_shell() {
        assert_eq!(shell_escape_path("/tmp/a b").unwrap(), "'/tmp/a b'");
        assert_eq!(
            shell_escape_path("/tmp/it's").unwrap(),
            r#"'/tmp/it'"'"'s'"#
        );
        assert_eq!(shell_escape_path("-rf").unwrap(), "'./-rf'");
        assert_eq!(shell_escape_path("/tmp/-x").unwrap(), "'/tmp/-x'");
        assert!(shell_escape_path("").is_err());
        assert!(shell_escape_path("/a\0b").is_err());
        assert_eq!(
            join_shell_escaped_paths(&["/a".into(), "/b c".into()]).unwrap(),
            "'/a' '/b c'"
        );
        assert!(join_shell_escaped_paths(&[]).is_err());
    }

    #[test]
    fn pastes_a_staged_image_path_raw_only_when_an_agent_can_read_it() {
        let path = "/tmp/muxflow/clipboard-1.png";
        assert_eq!(validate_agent_image_path(path).unwrap(), path);
        for bad in [
            "relative.png",
            "/tmp/a b.png",
            "/tmp/a'b.png",
            "/tmp/a\\b.png",
            "/t\x7f",
        ] {
            assert!(validate_agent_image_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_in_app_path_drop_pastes_only_a_canonical_path_on_the_same_host() {
        let drag = |path: &str| InternalPathDrag {
            host_profile_id: "local".into(),
            server_identity: "socket:1".into(),
            path: path.into(),
        };
        let here = Some(("local", "socket:1"));
        assert_eq!(drag("/srv/a b").shell_text(here).unwrap(), "'/srv/a b'");
        assert!(drag("/srv/a").shell_text(None).is_err());
        assert!(
            drag("/srv/a")
                .shell_text(Some(("local", "socket:2")))
                .is_err()
        );
        assert!(
            drag("/srv/a")
                .shell_text(Some(("remote", "socket:1")))
                .is_err()
        );
        for bad in [
            "relative", "/", "/a//b", "/a/./b", "/a/../b", "/a/", "/a\0b",
        ] {
            assert!(drag(bad).shell_text(here).is_err(), "{bad:?}");
        }
        let nameless = InternalPathDrag {
            host_profile_id: String::new(),
            ..drag("/srv/a")
        };
        assert!(nameless.shell_text(Some(("", "socket:1"))).is_err());
    }

    #[test]
    fn reads_local_file_uris_and_refuses_anything_else() {
        assert_eq!(
            parse_file_uri_list("# comment\r\nfile:///tmp/a%20b.txt\nfile://localhost/x\n\n")
                .unwrap(),
            ["/tmp/a b.txt", "/x"]
        );
        assert_eq!(
            parse_file_uri_list("file:///tmp/%C3%A9t%C3%A9").unwrap(),
            ["/tmp/été"]
        );
        assert!(parse_file_uri_list("file://server/share").is_err());
        assert!(parse_file_uri_list("https://example.com/a").is_err());
        assert!(parse_file_uri_list("file:///bad%zz").is_err());
        assert!(parse_file_uri_list("file:///bad%ff").is_err());
    }

    #[test]
    fn names_an_upload_after_its_last_path_part() {
        assert_eq!(basename("/tmp/report.pdf"), "report.pdf");
        assert_eq!(basename("/tmp/dir/"), "dir");
        assert_eq!(basename("/"), "upload");
    }

    #[test]
    fn decodes_the_three_native_clipboard_answers() {
        assert_eq!(
            NativeClipboard::from_wire(json!({"kind": "files", "uris": ["file:///a"]})).unwrap(),
            NativeClipboard::Files {
                uris: vec!["file:///a".into()]
            }
        );
        assert_eq!(
            NativeClipboard::from_wire(json!({"kind": "text", "text": "hi"})).unwrap(),
            NativeClipboard::Text { text: "hi".into() }
        );
        let name = "clipboard-0f8fad5b-d9cb-469f-a165-70867728950e.png";
        let image = json!({"kind": "image", "staged": {"path": "/s/x.png", "sizeBytes": "9", "name": name}});
        assert!(matches!(
            NativeClipboard::from_wire(image).unwrap(),
            NativeClipboard::Image { .. }
        ));
        let not_unique = json!({"kind": "image", "staged": {"path": "/s/x.png", "name": "x.png"}});
        assert!(NativeClipboard::from_wire(not_unique).is_err());
        let wrong_variant = json!({"kind": "image", "staged": {
            "path": "/s/x.png", "name": "clipboard-0f8fad5b-d9cb-469f-c165-70867728950e.png"
        }});
        assert!(NativeClipboard::from_wire(wrong_variant).is_err());
        assert!(NativeClipboard::from_wire(json!({"kind": "html"})).is_err());
    }

    fn event(state: &str, extra: Value) -> TransferEvent {
        let mut value = json!({
            "transferId": "t1",
            "sourcePath": "/tmp/a.txt",
            "name": "a.txt",
            "state": state,
            "serverIdentity": "socket:1",
            "connectionEpoch": "7",
        });
        value
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        TransferEvent::parse(&value.to_string()).unwrap()
    }

    fn scope() -> TransferScope {
        TransferScope {
            client_id: "c".into(),
            profile_id: "p".into(),
            server_identity: "socket:1".into(),
            terminal_epoch: 7,
            pane_id: "%1".into(),
            local: false,
        }
    }

    #[test]
    fn an_event_from_another_connection_is_not_this_batchs() {
        let mine = event("running", json!({}));
        assert!(mine.belongs_to(&scope()));
        let replaced = TransferScope {
            terminal_epoch: 8,
            ..scope()
        };
        assert!(!mine.belongs_to(&replaced));
        let other_server = TransferScope {
            server_identity: "socket:2".into(),
            ..scope()
        };
        assert!(!mine.belongs_to(&other_server));
        assert!(TransferEvent::parse(r#"{"transferId": "t", "state": "exploded"}"#).is_err());
    }

    #[test]
    fn a_preflight_admits_only_a_readable_regular_file() {
        let ok = event(
            "completed",
            json!({"sizeBytes": "12", "sourceKind": "regularFile"}),
        );
        assert_eq!(
            Preflight::from_completed(&ok, "a.txt").unwrap(),
            Preflight {
                source_path: "/tmp/a.txt".into(),
                name: "a.txt".into(),
                size_bytes: 12,
                collision: false,
            }
        );
        let renamed = event(
            "completed",
            json!({"sizeBytes": "1", "collisionRenamed": true}),
        );
        assert!(Preflight::from_completed(&renamed, "a").unwrap().collision);
        for refused in [
            json!({"sizeBytes": "1", "sourceKind": "directory"}),
            json!({"sizeBytes": "1", "sourceKind": "other"}),
            json!({"sizeBytes": "1", "readable": false}),
            json!({"sizeBytes": "x"}),
            json!({"sizeBytes": "1", "cleanupError": "left behind"}),
        ] {
            assert!(Preflight::from_completed(&event("completed", refused), "a").is_err());
        }
    }

    fn item(name: &str, size_bytes: u64, collision: bool) -> Preflight {
        Preflight {
            source_path: format!("/src/{name}"),
            name: name.into(),
            size_bytes,
            collision,
        }
    }

    #[test]
    fn asks_the_user_only_about_large_files_and_names_that_collide() {
        assert_eq!(
            review_for(&[item("a", 1, false), item("b", 2, false)]),
            None
        );
        assert_eq!(
            review_for(&[item("a", LARGE_UPLOAD_BYTES + 1, false)]),
            Some(Review {
                large: true,
                collisions: false
            })
        );
        assert_eq!(
            review_for(&[item("a", 1, false), item("a", 1, false)]),
            Some(Review {
                large: false,
                collisions: true
            })
        );
        assert_eq!(
            review_for(&[item("a", 1, true)]),
            Some(Review {
                large: false,
                collisions: true
            })
        );
    }

    #[test]
    fn formats_bytes_with_one_decimal() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(5 * (1 << 30)), "5.0 GiB");
        let partial = event(
            "running",
            json!({"transferredBytes": "1048576", "totalBytes": "3145728"}),
        );
        assert_eq!(partial.progress_text(), "1.0 MiB / 3.0 MiB");
        assert_eq!(
            event("running", json!({"transferredBytes": "10"})).progress_text(),
            "10 B transferred"
        );
    }

    #[test]
    fn only_a_digest_verified_published_upload_has_a_destination_to_paste() {
        let landed = json!({"destination": "/srv/a.txt", "blake3": "ab12", "outcome": "published"});
        assert_eq!(
            event("completed", landed.clone()).verified_destination(),
            Some("/srv/a.txt".into())
        );
        let mut unverified = landed.clone();
        unverified.as_object_mut().unwrap().remove("blake3");
        assert_eq!(event("completed", unverified).verified_destination(), None);
        let mut doubtful = landed.clone();
        doubtful["outcome"] = json!("unknown");
        assert_eq!(event("completed", doubtful).verified_destination(), None);
        assert_eq!(event("running", landed).verified_destination(), None);
    }

    #[test]
    fn a_late_progress_frame_never_reopens_a_finished_record() {
        let mut list = TransferRecords::default();
        list.record("%1", event("cancelled", json!({})));
        list.record("%1", event("running", json!({})));
        assert_eq!(list.records[0].progress.state, TransferState::Cancelled);
    }

    #[test]
    fn the_list_drops_only_what_was_delivered_cleanly_or_dismissed_once_finished() {
        let mut list = TransferRecords::default();
        list.record("%1", event("running", json!({})));
        list.record("%1", event("completed", json!({"outcome": "published"})));
        assert_eq!(list.records.len(), 1, "one record per transfer");
        let clean = event("completed", json!({"cleanupError": "", "error": ""}));
        assert!(clean.delivered_cleanly(), "an empty message is no error");
        assert_eq!(clean.cleanup_error, None);
        let mut retained = event("completed", json!({"cleanupStatus": "retained"}));
        retained.transfer_id = "t2".into();
        list.record("%1", retained);
        list.dismiss_delivered(&["t1".into(), "t2".into()]);
        assert_eq!(
            list.records
                .iter()
                .map(|record| record.progress.transfer_id.as_str())
                .collect::<Vec<_>>(),
            ["t2"],
            "a completion that left bytes behind stays"
        );

        let mut running = event("running", json!({}));
        running.transfer_id = "t3".into();
        list.record("%1", running);
        list.dismiss("t3");
        list.mark_verifying("t3");
        assert_eq!(list.records[0].progress.state, TransferState::Verifying);
        list.dismiss("t2");
        assert_eq!(list.records.len(), 1);
    }
}
