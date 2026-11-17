//! Files sent with a message (docs/design/chat-polish-plan.md §3.1).
//!
//! A desktop uploads what is pasted or dropped into a chat through the same
//! path a terminal's uploads take, into this user's private staging
//! directory, and names the staged files in SEND. The host attaches nothing
//! else: a path that does not lie in that directory once its links are
//! resolved, or that is not a regular file of this user's there, is refused
//! before anything of it is read. So naming `/etc/passwd`, or a link in the
//! staging directory that points out of it, reads nothing into a message.
//!
//! What the agent gets is each provider's business: an image as an image,
//! any other file as its path on a line of the message (D9). The bytes of an
//! image small enough to go inline are read here, off the async runtime, so a
//! provider only has to place them.

use std::{
    fmt,
    fs::File,
    io::Read as _,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use tmux_agent_protocol::v1;

use super::provider::ChatError;

/// The most files one message carries.
pub(crate) const MAX_ATTACHMENTS: usize = 10;
/// The largest image handed over inline, base64-encoded, as the API takes
/// one. A larger one goes as its path.
const MAX_INLINE_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// The most the inline images of one message weigh, encoded. Images past it
/// go as their paths. The message comes back whole on one line of the agent's
/// (`session::MAX_MESSAGE_BYTES`) and lies whole in its record, whose reader
/// starts from a window of its end (`claude::history`): with its text, a
/// message stays well inside both.
pub(in crate::service::chat) const MAX_INLINE_TOTAL_BYTES: usize = 6 * 1024 * 1024;
/// The longest name kept for an attachment.
const MAX_NAME_BYTES: usize = 255;

/// One file a message is sent with, checked to be one this user staged.
#[derive(Clone, PartialEq)]
pub(crate) struct Attached {
    /// Absolute, with no links, in the staging directory.
    pub(crate) path: String,
    pub(crate) name: String,
    /// The image type the file's own bytes say, such as `image/png`; empty
    /// for anything else, whatever the desktop called it.
    pub(crate) media_type: String,
    pub(crate) size: u64,
    /// An image's bytes in base64, when it is small enough to go inline.
    pub(crate) inline: Option<String>,
}

impl fmt::Debug for Attached {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Attached")
            .field("path", &self.path)
            .field("media_type", &self.media_type)
            .field("size", &self.size)
            .field("inline", &self.inline.as_ref().map(String::len))
            .finish()
    }
}

impl Attached {
    pub(crate) fn is_image(&self) -> bool {
        !self.media_type.is_empty()
    }

    /// As the USER item carries it.
    pub(crate) fn wire(&self) -> v1::ChatAttachment {
        v1::ChatAttachment {
            path: self.path.clone(),
            name: self.name.clone(),
            media_type: self.media_type.clone(),
            size_bytes: self.size,
        }
    }
}

/// This user's upload staging directory, where the desktop's uploads land
/// (`filesystem/terminal_upload/staging.rs`), with its links resolved when it
/// exists.
pub(crate) fn staging_directory() -> Option<PathBuf> {
    static RESOLVED: OnceLock<PathBuf> = OnceLock::new();
    if let Some(resolved) = RESOLVED.get() {
        return Some(resolved.clone());
    }
    let home = PathBuf::from(std::env::var_os("HOME")?);
    if !home.is_absolute() {
        return None;
    }
    let path = home.join(".cache/muxflow/uploads");
    // Kept once it exists: until the first upload there is nothing to resolve.
    match std::fs::canonicalize(&path) {
        Ok(resolved) => Some(RESOLVED.get_or_init(|| resolved).clone()),
        Err(_) => Some(path),
    }
}

/// Checks what SEND names and reads what goes inline. Blocks: run it off the
/// async runtime.
pub(crate) fn admit(wanted: Vec<v1::ChatAttachment>) -> Result<Vec<Attached>, ChatError> {
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let staging = staging_directory().ok_or_else(|| {
        ChatError::new(
            "chat_attachment_missing",
            "The host has no upload folder, so nothing can be attached.",
        )
    })?;
    admit_in(&staging, wanted)
}

pub(crate) fn admit_in(
    staging: &Path,
    wanted: Vec<v1::ChatAttachment>,
) -> Result<Vec<Attached>, ChatError> {
    if wanted.len() > MAX_ATTACHMENTS {
        return Err(ChatError::new(
            "chat_invalid_request",
            format!("A message takes at most {MAX_ATTACHMENTS} attachments."),
        ));
    }
    // A staging directory that is not there holds nothing to attach.
    let named = staging;
    let staging = std::fs::canonicalize(staging).unwrap_or_else(|_| staging.to_owned());
    let mut inline_left = MAX_INLINE_TOTAL_BYTES;
    let mut admitted = Vec::with_capacity(wanted.len());
    for attachment in wanted {
        let mut attached = admit_one(named, &staging, &attachment)?;
        if attached.is_image()
            && encoded_len(attached.size) <= MAX_INLINE_IMAGE_BYTES.min(inline_left)
        {
            let bytes = read_whole(Path::new(&attached.path), attached.size)
                .map_err(|_| missing(&attached.name))?;
            let encoded = base64(&bytes);
            inline_left -= encoded.len();
            attached.inline = Some(encoded);
        }
        admitted.push(attached);
    }
    Ok(admitted)
}

/// `named` is the staging directory as the upload named it, `staging` the
/// same with its links resolved.
fn admit_one(
    named: &Path,
    staging: &Path,
    attachment: &v1::ChatAttachment,
) -> Result<Attached, ChatError> {
    let path = Path::new(&attachment.path);
    let mut name = if attachment.name.trim().is_empty() {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        attachment.name.clone()
    };
    name.truncate(name.floor_char_boundary(MAX_NAME_BYTES));
    // Named in the staging directory before anything is looked up, so a
    // path elsewhere is refused whether or not it is there.
    let in_staging = |path: &Path| {
        let parent = path.parent();
        parent == Some(named) || parent == Some(staging)
    };
    let plain = path.components().all(|part| {
        matches!(
            part,
            std::path::Component::RootDir | std::path::Component::Normal(_)
        )
    });
    if !path.is_absolute() || attachment.path.contains('\0') || !plain || !in_staging(path) {
        return Err(refused(&name));
    }
    let resolved = match std::fs::canonicalize(path) {
        Ok(resolved) => resolved,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(missing(&name));
        }
        Err(_) => return Err(refused(&name)),
    };
    // Directly in the staging directory, and not one of its own records:
    // partial uploads, reservations and manifests all start with a dot.
    let staged = in_staging(&resolved)
        && resolved
            .file_name()
            .is_some_and(|file| !file.to_string_lossy().starts_with('.'));
    if !staged {
        return Err(refused(&name));
    }
    // Opened without following a link that replaced the file since, and
    // checked as what was opened.
    let mut file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&resolved)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => missing(&name),
            _ => refused(&name),
        })?;
    let metadata = file.metadata().map_err(|_| refused(&name))?;
    // SAFETY: geteuid has no preconditions and cannot fail.
    let owner = unsafe { libc::geteuid() };
    if !metadata.is_file() || metadata.uid() != owner {
        return Err(refused(&name));
    }
    let mut head = [0_u8; 12];
    let read = read_up_to(&mut file, &mut head).map_err(|_| refused(&name))?;
    Ok(Attached {
        path: resolved.to_string_lossy().into_owned(),
        name,
        media_type: image_type(&head[..read]).to_owned(),
        size: metadata.len(),
        inline: None,
    })
}

fn missing(name: &str) -> ChatError {
    ChatError::new(
        "chat_attachment_missing",
        format!("{name} is no longer on the host. Remove it and attach it again."),
    )
}

fn refused(name: &str) -> ChatError {
    ChatError::new(
        "chat_attachment_refused",
        format!("{name} was not uploaded through Muxflow, so it cannot be sent."),
    )
}

fn read_up_to(file: &mut File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..])? {
            0 => break,
            count => filled += count,
        }
    }
    Ok(filled)
}

/// The file as it was checked: one that changed size since is not it.
fn read_whole(path: &Path, size: u64) -> std::io::Result<Vec<u8>> {
    let file = File::options()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(size + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size {
        return Err(std::io::Error::other("the file changed while it was read"));
    }
    Ok(bytes)
}

/// The image types every provider takes, by their first bytes.
fn image_type(head: &[u8]) -> &'static str {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if head.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        "image/gif"
    } else if head.len() >= 12 && head.starts_with(b"RIFF") && &head[8..12] == b"WEBP" {
        "image/webp"
    } else {
        ""
    }
}

/// An image type for a file named `name`, from its extension: for paths read
/// back from a message, where the file itself is not read.
pub(crate) fn image_type_of_name(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "",
    }
}

fn encoded_len(size: u64) -> usize {
    usize::try_from(size.div_ceil(3).saturating_mul(4)).unwrap_or(usize::MAX)
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(encoded_len(bytes.len() as u64));
    for chunk in bytes.chunks(3) {
        let triple = (u32::from(chunk[0]) << 16)
            | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        for index in 0..4 {
            if index <= chunk.len() {
                out.push(char::from(
                    ALPHABET[(triple >> (18 - 6 * index)) as usize & 63],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The message's text with the files that do not go inline named under it,
/// one path to a line, as the agent is to read them with its own tools.
pub(crate) fn with_paths<'a>(text: &str, paths: impl IntoIterator<Item = &'a str>) -> String {
    let lines: Vec<&str> = paths.into_iter().collect();
    if lines.is_empty() {
        return text.to_owned();
    }
    let lines = lines.join("\n");
    if text.trim().is_empty() {
        lines
    } else {
        format!("{text}\n\n{lines}")
    }
}

/// A message read back from the agent's record: the staged paths
/// [`with_paths`] put under its text become attachments again.
pub(crate) fn split_paths(text: &str) -> (String, Vec<v1::ChatAttachment>) {
    match staging_directory() {
        Some(staging) => split_paths_in(text, &staging),
        None => (text.to_owned(), Vec::new()),
    }
}

pub(crate) fn split_paths_in(text: &str, staging: &Path) -> (String, Vec<v1::ChatAttachment>) {
    let staged = |line: &str| {
        let path = Path::new(line);
        line == line.trim()
            && path.parent() == Some(staging)
            && path
                .file_name()
                .is_some_and(|name| !name.to_string_lossy().starts_with('.'))
    };
    let lines: Vec<&str> = text.lines().collect();
    let kept = lines
        .iter()
        .rposition(|line| !staged(line))
        .map_or(0, |last| last + 1);
    // Paths count only as the block of their own that ends the message.
    if kept == lines.len() || (kept > 0 && !lines[kept - 1].trim().is_empty()) {
        return (text.to_owned(), Vec::new());
    }
    let attachments = lines[kept..]
        .iter()
        .map(|line| {
            let name = Path::new(line)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            v1::ChatAttachment {
                path: (*line).to_owned(),
                media_type: image_type_of_name(&name).to_owned(),
                name,
                size_bytes: 0,
            }
        })
        .collect();
    (lines[..kept].join("\n").trim_end().to_owned(), attachments)
}

/// An image the agent's record holds inline, which has no name or path left.
pub(crate) fn inline_image(media_type: &str, data_len: usize) -> v1::ChatAttachment {
    v1::ChatAttachment {
        path: String::new(),
        name: "Image".to_owned(),
        media_type: media_type.to_owned(),
        size_bytes: (data_len as u64 / 4) * 3,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    fn wanted(path: &Path) -> v1::ChatAttachment {
        v1::ChatAttachment {
            path: path.to_string_lossy().into_owned(),
            name: path.file_name().unwrap().to_string_lossy().into_owned(),
            media_type: "image/png".into(),
            size_bytes: 1,
        }
    }

    #[test]
    fn encodes_base64_as_the_standard_alphabet_with_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
        assert_eq!(encoded_len(4), 8);
    }

    #[test]
    fn a_small_staged_image_goes_inline_and_a_large_one_or_a_file_does_not() {
        let staging = tempfile::tempdir().unwrap();
        let small = staging.path().join("shot.png");
        std::fs::write(&small, PNG).unwrap();
        let large = staging.path().join("big.png");
        let mut bytes = PNG.to_vec();
        bytes.resize(MAX_INLINE_IMAGE_BYTES, 0);
        std::fs::write(&large, &bytes).unwrap();
        let log = staging.path().join("app.log");
        std::fs::write(&log, b"hello").unwrap();

        let admitted = admit_in(
            staging.path(),
            vec![wanted(&small), wanted(&large), wanted(&log)],
        )
        .unwrap();
        assert_eq!(admitted[0].media_type, "image/png");
        assert_eq!(admitted[0].inline.as_deref(), Some(base64(PNG).as_str()));
        assert_eq!(admitted[1].media_type, "image/png");
        assert_eq!(admitted[1].inline, None, "over 5 MB encoded");
        // The desktop called it an image; its bytes say otherwise.
        assert_eq!(admitted[2].media_type, "");
        assert_eq!(admitted[2].inline, None);
        assert_eq!(admitted[2].size, 5);
        assert_eq!(admitted[2].name, "app.log");
        let resolved = std::fs::canonicalize(&log).unwrap();
        assert_eq!(admitted[2].path, resolved.to_string_lossy());
    }

    #[test]
    fn a_staged_file_that_is_gone_fails_with_its_name() {
        let staging = tempfile::tempdir().unwrap();
        let error = admit_in(
            staging.path(),
            vec![wanted(&staging.path().join("gone.png"))],
        )
        .unwrap_err();
        assert_eq!(error.code, "chat_attachment_missing");
        assert_eq!(
            error.message,
            "gone.png is no longer on the host. Remove it and attach it again."
        );
    }

    #[test]
    fn nothing_outside_the_staging_directory_is_read() {
        let staging = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let secret = elsewhere.path().join("secret.png");
        std::fs::write(&secret, PNG).unwrap();
        let nested = staging.path().join("dir");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(nested.join("a.png"), PNG).unwrap();
        let link = staging.path().join("link.png");
        symlink(&secret, &link).unwrap();
        let partial = staging.path().join(".tmux-agent-upload-x.partial");
        std::fs::write(&partial, PNG).unwrap();
        let dotted = staging.path().join("dir/../dir/a.png");
        let nowhere = elsewhere.path().join("not-there.png");
        for path in [
            secret.clone(),
            nested.join("a.png"),
            link,
            partial,
            nested.clone(),
            dotted,
            // Refused, not missing: an answer must not say what is there.
            nowhere,
            PathBuf::from("relative.png"),
        ] {
            let error = admit_in(staging.path(), vec![wanted(&path)]).unwrap_err();
            assert_eq!(error.code, "chat_attachment_refused", "{path:?}");
        }
    }

    #[test]
    fn a_message_takes_at_most_ten() {
        let staging = tempfile::tempdir().unwrap();
        let file = staging.path().join("a.txt");
        std::fs::write(&file, b"a").unwrap();
        let error = admit_in(staging.path(), vec![wanted(&file); 11]).unwrap_err();
        assert_eq!(error.message, "A message takes at most 10 attachments.");
        assert_eq!(
            admit_in(staging.path(), vec![wanted(&file); 10])
                .unwrap()
                .len(),
            10
        );
    }

    #[test]
    fn inline_images_stop_at_the_message_bound() {
        let staging = tempfile::tempdir().unwrap();
        let image = staging.path().join("shot.png");
        let mut bytes = PNG.to_vec();
        bytes.resize(3 * 1024 * 1024, 0);
        std::fs::write(&image, &bytes).unwrap();
        let admitted = admit_in(staging.path(), vec![wanted(&image); 10]).unwrap();
        let inline: usize = admitted
            .iter()
            .filter_map(|attached| attached.inline.as_ref().map(String::len))
            .sum();
        assert!(inline <= MAX_INLINE_TOTAL_BYTES);
        // 4 MiB encoded each: one fits the 6 MiB budget, the rest go by path.
        assert_eq!(admitted.iter().filter(|a| a.inline.is_some()).count(), 1);
        assert!(admitted[1..].iter().all(|a| a.inline.is_none()));
    }

    #[test]
    fn staged_paths_under_a_message_read_back_as_its_attachments() {
        let staging = Path::new("/home/u/.cache/muxflow/uploads");
        let sent = with_paths(
            "Look at these.",
            [
                "/home/u/.cache/muxflow/uploads/app.log",
                "/home/u/.cache/muxflow/uploads/big.png",
            ],
        );
        assert_eq!(
            sent,
            "Look at these.\n\n/home/u/.cache/muxflow/uploads/app.log\n/home/u/.cache/muxflow/uploads/big.png"
        );
        let (text, attachments) = split_paths_in(&sent, staging);
        assert_eq!(text, "Look at these.");
        let names: Vec<(&str, &str)> = attachments
            .iter()
            .map(|a| (a.name.as_str(), a.media_type.as_str()))
            .collect();
        assert_eq!(names, [("app.log", ""), ("big.png", "image/png")]);
        // Only paths: the whole message is attachments.
        let (text, attachments) = split_paths_in("/home/u/.cache/muxflow/uploads/a.txt", staging);
        assert_eq!((text.as_str(), attachments.len()), ("", 1));
        // A path somewhere else, or one inside the text, stays text.
        for kept in [
            "See /home/u/.cache/muxflow/uploads/a.txt",
            "Look\n/etc/passwd",
            "Look\n/home/u/.cache/muxflow/uploads/a.txt\nand this",
        ] {
            assert_eq!(split_paths_in(kept, staging), (kept.to_owned(), Vec::new()));
        }
        assert_eq!(with_paths("Just text.", []), "Just text.");
    }
}
