#[cfg(not(target_os = "macos"))]
use clipboard_rs::ClipboardContext;
use serde_json::Value;
#[cfg(not(target_os = "macos"))]
use std::sync::{Mutex, OnceLock};

const MAX_NATIVE_TEXT_BYTES: usize = tmux_control::MAX_INPUT_REQUEST_BYTES;

/// The one clipboard context this process ever opens.
///
/// `ClipboardContext::new` is not a cheap handle: on X11 it opens two
/// connections to the display server and spawns a thread parked in
/// `wait_for_event` so the selection stays owned, and the type has no `Drop`, so
/// nothing is ever given back. Copy-on-select makes a copy a per-mouse-up event,
/// so a context per operation walked into Xorg's `MaxClients` (~128) and killed
/// the clipboard for the rest of the session. clipboard-rs is written for the
/// opposite lifetime — its own examples build one context and drive every
/// operation through it, and each `Clipboard` method takes `&self` and does its
/// own round trip — so read and write share this one.
///
/// Under Wayland the context holds nothing: each operation opens and closes its
/// own compositor connection, and a copy's text is served by a thread that ends
/// once another copy replaces it — unless a paste of it is stuck mid-transfer,
/// which keeps that one thread until the pasting app lets go. There it only
/// records which backend was chosen, and Wayland reads bypass it
/// (`read_linux_clipboard`).
#[cfg(not(target_os = "macos"))]
static CLIPBOARD: OnceLock<Mutex<Option<ClipboardContext>>> = OnceLock::new();

/// Run `action` against the shared context, opening it on first use.
///
/// Creation failure and a poisoned lock are both reported to the caller: a
/// clipboard that cannot be opened is an error the renderer can show, not a
/// reason to take the process down. The context is left uncreated on failure so
/// a later copy retries rather than inheriting one bad moment forever.
#[cfg(not(target_os = "macos"))]
fn with_clipboard<T>(
    action: impl FnOnce(&ClipboardContext) -> Result<T, String>,
) -> Result<T, String> {
    let mut slot = CLIPBOARD
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| "native clipboard lock poisoned".to_string())?;
    if slot.is_none() {
        *slot = Some(
            ClipboardContext::new().map_err(|error| format!("open native clipboard: {error}"))?,
        );
    }
    let clipboard = slot.as_ref().ok_or("native clipboard is unavailable")?;
    action(clipboard)
}

#[cfg(not(target_os = "macos"))]
pub async fn write_native_terminal_clipboard(text: String) -> Result<(), String> {
    let text = validated_native_clipboard_write(text)?;
    crate::platform::spawn_blocking(move || {
        use clipboard_rs::Clipboard;
        with_clipboard(|clipboard| {
            clipboard
                .set_text(text)
                .map_err(|error| format!("write native clipboard text: {error}"))
        })
    })
    .await
    .map_err(|error| format!("native clipboard worker failed: {error}"))?
}

/// AppKit's pasteboard belongs to the main thread, so the application calls
/// this there. Fails if it was not.
#[cfg(target_os = "macos")]
pub fn write_macos_clipboard_on_main_thread(text: String) -> Result<(), String> {
    let text = validated_native_clipboard_write(text)?;
    let _main_thread = objc2::MainThreadMarker::new()
        .ok_or("native clipboard write did not run on the main thread")?;
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    use objc2_foundation::NSString;

    autoreleasepool(|_| {
        let pasteboard = NSPasteboard::generalPasteboard();
        pasteboard.clearContents();
        if pasteboard.setString_forType(&NSString::from_str(&text), unsafe {
            NSPasteboardTypeString
        }) {
            Ok(())
        } else {
            Err("write native clipboard text: macOS refused the pasteboard write".into())
        }
    })
}

fn validated_native_clipboard_write(text: String) -> Result<String, String> {
    if text.is_empty() {
        return Err("refusing to replace the native clipboard with empty text".into());
    }
    if text.len() > MAX_NATIVE_TEXT_BYTES {
        return Err(format!(
            "native clipboard text exceeds the {MAX_NATIVE_TEXT_BYTES}-byte bound"
        ));
    }
    Ok(text)
}

pub async fn read_native_terminal_clipboard() -> Result<Option<Value>, String> {
    #[cfg(target_os = "linux")]
    {
        crate::platform::spawn_blocking(read_linux_clipboard)
            .await
            .map_err(|error| format!("native clipboard worker failed: {error}"))?
    }
    #[cfg(target_os = "macos")]
    {
        crate::platform::spawn_blocking(read_macos_clipboard)
            .await
            .map_err(|error| format!("native clipboard worker failed: {error}"))?
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    Ok(None)
}

/// How long a Wayland paste waits for the app that owns the clipboard to hand
/// its content over. X11 reads carry clipboard-rs's own 500 ms bound.
#[cfg(target_os = "linux")]
const WAYLAND_READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);

/// Room for a `text/uri-list` of thousands of copied files.
#[cfg(target_os = "linux")]
const MAX_NATIVE_URI_LIST_BYTES: usize = 1024 * 1024;

/// The staging bound (`clipboard_staging.rs`), enforced here before the whole
/// image is in memory rather than after.
#[cfg(target_os = "linux")]
const MAX_NATIVE_PNG_BYTES: usize = 25 * 1024 * 1024;

#[cfg(target_os = "linux")]
fn read_linux_clipboard() -> Result<Option<Value>, String> {
    // Under Wayland, clipboard-rs (its `wayland` feature) talks to the
    // compositor through a data-control protocol, which wlroots compositors,
    // KDE and niri offer. Without it — GNOME — or without a Wayland display, it
    // uses X11 through XWayland, whose clipboard only reaches native Wayland apps
    // as far as the compositor bridges it.
    //
    // Wayland reads are done here rather than through clipboard-rs, whose
    // Wayland read waits on the owning app with no deadline — one frozen app
    // would hold the shared lock, and with it every later copy, for good.
    let wayland =
        with_clipboard(|clipboard| Ok(matches!(clipboard, ClipboardContext::Wayland(_))))?;
    if wayland {
        read_wayland_clipboard()
    } else {
        with_clipboard(read_from_linux_clipboard)
    }
}

/// Files first, then `choose_paste`, as `read_from_linux_clipboard` does.
#[cfg(target_os = "linux")]
fn read_wayland_clipboard() -> Result<Option<Value>, String> {
    use serde_json::json;
    use wl_clipboard_rs::paste::{ClipboardType, Error, MimeType, Seat, get_mime_types};

    let formats = match get_mime_types(ClipboardType::Regular, Seat::Unspecified) {
        Ok(formats) => formats,
        Err(Error::ClipboardEmpty | Error::NoMimeType) => return Ok(None),
        Err(error) => return Err(format!("inspect native clipboard formats: {error}")),
    };

    if formats.contains("text/uri-list")
        && let Some(list) = read_wayland_offer(
            MimeType::Specific("text/uri-list"),
            MAX_NATIVE_URI_LIST_BYTES,
        )?
    {
        let uris = file_uris(&String::from_utf8_lossy(&list));
        if !uris.is_empty() {
            return Ok(Some(json!({ "kind": "files", "uris": uris })));
        }
    }

    let png_format = formats
        .iter()
        .find(|format| format.eq_ignore_ascii_case("image/png"));
    let paste = choose_paste(
        &formats,
        || match png_format {
            Some(format) => read_wayland_offer(MimeType::Specific(format), MAX_NATIVE_PNG_BYTES),
            None => Ok(None),
        },
        || {
            read_wayland_offer(MimeType::Text, MAX_NATIVE_TEXT_BYTES)?
                .map(|text| {
                    String::from_utf8(text).map_err(|_| "native clipboard text is not UTF-8".into())
                })
                .transpose()
        },
    )?;
    paste_payload(paste)
}

/// One offer's bytes, or `None` when the clipboard emptied or stopped offering
/// that type since it was inspected.
#[cfg(target_os = "linux")]
fn read_wayland_offer(
    format: wl_clipboard_rs::paste::MimeType<'_>,
    limit: usize,
) -> Result<Option<Vec<u8>>, String> {
    use wl_clipboard_rs::paste::{ClipboardType, Error, Seat, get_contents};

    let (mut pipe, _) = match get_contents(ClipboardType::Regular, Seat::Unspecified, format) {
        Ok(contents) => contents,
        Err(Error::ClipboardEmpty | Error::NoMimeType) => return Ok(None),
        Err(error) => return Err(format!("read native clipboard: {error}")),
    };
    read_bounded(&mut pipe, limit, WAYLAND_READ_DEADLINE).map(Some)
}

/// Read `source` to its end, refusing more than `limit` bytes and a writer that
/// has not finished by `deadline`.
///
/// The descriptor is made non-blocking, so a writer that stalls mid-transfer
/// cannot park the read past the deadline either.
#[cfg(target_os = "linux")]
fn read_bounded(
    source: &mut (impl std::io::Read + std::os::fd::AsRawFd),
    limit: usize,
    deadline: std::time::Duration,
) -> Result<Vec<u8>, String> {
    use std::io::ErrorKind;

    let fd = source.as_raw_fd();
    // SAFETY: `fd` is owned by `source`, which outlives this call.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(format!(
            "read native clipboard: {}",
            std::io::Error::last_os_error()
        ));
    }
    let end = std::time::Instant::now() + deadline;
    let mut bytes = Vec::new();
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let remaining = end.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(format!(
                "the app that owns the clipboard did not hand it over within {} s",
                deadline.as_secs_f32()
            ));
        }
        let mut ready = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout = i32::try_from(remaining.as_millis())
            .unwrap_or(i32::MAX)
            .max(1);
        // SAFETY: one valid `pollfd` for the duration of the call.
        if unsafe { libc::poll(&mut ready, 1, timeout) } < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(format!("read native clipboard: {error}"));
        }
        match source.read(&mut chunk) {
            Ok(0) => return Ok(bytes),
            Ok(read) if bytes.len() + read > limit => {
                return Err(format!(
                    "native clipboard content exceeds the {limit}-byte bound"
                ));
            }
            Ok(read) => bytes.extend_from_slice(&chunk[..read]),
            Err(error)
                if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            Err(error) => return Err(format!("read native clipboard: {error}")),
        }
    }
}

/// The `file://` entries of a `text/uri-list`, which marks comments with `#`
/// and ends lines with CRLF.
#[cfg(any(target_os = "linux", test))]
fn file_uris(list: &str) -> Vec<String> {
    list.lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| line.starts_with("file://"))
        .map(str::to_owned)
        .collect()
}

#[cfg(target_os = "linux")]
fn read_from_linux_clipboard(clipboard: &ClipboardContext) -> Result<Option<Value>, String> {
    use clipboard_rs::{Clipboard, ContentFormat};
    use serde_json::json;

    if clipboard.has(ContentFormat::Files) {
        let uris = clipboard
            .get_files()
            .map_err(|error| format!("read native clipboard files: {error}"))?;
        if !uris.is_empty() {
            return Ok(Some(json!({ "kind": "files", "uris": uris })));
        }
    }

    let formats = clipboard
        .available_formats()
        .map_err(|error| format!("inspect native clipboard formats: {error}"))?;
    let png_format = formats.iter().find(|format| {
        format.eq_ignore_ascii_case("image/png") || format.eq_ignore_ascii_case("png")
    });
    let paste = choose_paste(
        &formats,
        || match png_format {
            Some(format) => clipboard
                .get_buffer(format)
                .map(Some)
                .map_err(|error| format!("read native clipboard PNG: {error}")),
            None => Ok(None),
        },
        || {
            if !clipboard.has(ContentFormat::Text) {
                return Ok(None);
            }
            clipboard
                .get_text()
                .map(Some)
                .map_err(|error| format!("read native clipboard text: {error}"))
        },
    )?;
    paste_payload(paste)
}

#[cfg(target_os = "macos")]
fn read_macos_clipboard() -> Result<Option<Value>, String> {
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeString};
    use serde_json::json;

    let pasteboard = NSPasteboard::generalPasteboard();
    if let Some(items) = pasteboard.pasteboardItems() {
        let mut uris = Vec::new();
        for item in items.iter() {
            let Some(uri) = item.stringForType(unsafe { NSPasteboardTypeFileURL }) else {
                uris.clear();
                break;
            };
            let Some(uri) = file_path_url(&uri.to_string()) else {
                uris.clear();
                break;
            };
            uris.push(uri);
        }
        if !uris.is_empty() {
            return Ok(Some(json!({ "kind": "files", "uris": uris })));
        }
    }

    let formats: Vec<String> = pasteboard
        .types()
        .map(|types| types.iter().map(|format| format.to_string()).collect())
        .unwrap_or_default();
    let paste = choose_paste(
        &formats,
        || macos_clipboard_png(&pasteboard),
        || {
            Ok(pasteboard
                .stringForType(unsafe { NSPasteboardTypeString })
                .map(|text| text.to_string()))
        },
    )?;
    paste_payload(paste)
}

/// The pasteboard's image as PNG bytes: its PNG flavor, else its TIFF flavor
/// re-encoded. `None` when neither holds any bytes — Numbers offers both
/// flavors, empty, next to the cells' text.
#[cfg(target_os = "macos")]
fn macos_clipboard_png(
    pasteboard: &objc2_app_kit::NSPasteboard,
) -> Result<Option<Vec<u8>>, String> {
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSPasteboardTypePNG, NSPasteboardTypeTIFF,
    };
    use objc2_foundation::NSDictionary;

    const MAX_NATIVE_IMAGE_BYTES: usize = 32 * 1024 * 1024;

    if let Some(png) = pasteboard
        .dataForType(unsafe { NSPasteboardTypePNG })
        .filter(|png| !png.is_empty())
    {
        if png.length() > MAX_NATIVE_IMAGE_BYTES {
            return Err("native clipboard image exceeds 32 MiB decode bound".into());
        }
        return Ok(Some(png.to_vec()));
    }

    let Some(tiff) = pasteboard
        .dataForType(unsafe { NSPasteboardTypeTIFF })
        .filter(|tiff| !tiff.is_empty())
    else {
        return Ok(None);
    };
    if tiff.length() > MAX_NATIVE_IMAGE_BYTES {
        return Err("native clipboard TIFF exceeds 32 MiB decode bound".into());
    }
    let image = NSBitmapImageRep::imageRepWithData(&tiff)
        .ok_or("native clipboard TIFF could not be decoded")?;
    let width = u64::try_from(image.pixelsWide()).unwrap_or(u64::MAX);
    let height = u64::try_from(image.pixelsHigh()).unwrap_or(u64::MAX);
    if width == 0
        || height == 0
        || width > 8_192
        || height > 8_192
        || width.saturating_mul(height) > 16_777_216
    {
        return Err("clipboard image exceeds the 8192px or 16,777,216-pixel limit".into());
    }
    let properties = NSDictionary::new();
    let png = unsafe {
        image.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties)
    }
    .ok_or("native clipboard TIFF could not be encoded as PNG")?;
    let bytes = png.to_vec();
    if bytes.len() > MAX_NATIVE_IMAGE_BYTES {
        return Err("encoded native clipboard PNG exceeds 32 MiB bridge bound".into());
    }
    Ok(Some(bytes))
}

/// What a paste takes from the clipboard once copied files are ruled out.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
enum Paste {
    /// PNG bytes, ready to stage.
    Image(Vec<u8>),
    Text(String),
}

/// Flavors only a document or spreadsheet app writes. With one present, an
/// image on the clipboard is a rendering of the copied text rather than the
/// thing the user copied. HTML and RTFD are deliberately absent: a browser's
/// Copy Image writes an `<img>` tag as HTML next to the picture, and Safari's
/// writes RTFD with the image's URL as plain text.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
const DOCUMENT_FORMATS: [&str; 6] = [
    "public.rtf",
    "com.adobe.pdf",
    "text/rtf",
    "text/richtext",
    "application/rtf",
    "application/pdf",
];

/// Choose between the clipboard's image and its text.
///
/// A copy from a document or spreadsheet app (Numbers, Excel, Word,
/// LibreOffice) carries the selection's text together with a picture of it,
/// and the user means the text. Anything else that holds an image, such as a
/// screenshot or a browser's Copy Image, pastes the image. An empty image
/// counts as none: Numbers offers 0-byte PNG and TIFF flavors.
///
/// `read_image` yields the image as PNG and `read_text` the plain text. Each
/// is called at most once, and only when the choice needs it.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn choose_paste(
    formats: impl IntoIterator<Item = impl AsRef<str>>,
    read_image: impl FnOnce() -> Result<Option<Vec<u8>>, String>,
    read_text: impl FnOnce() -> Result<Option<String>, String>,
) -> Result<Option<Paste>, String> {
    let document = formats.into_iter().any(|format| {
        DOCUMENT_FORMATS
            .iter()
            .any(|document| format.as_ref().eq_ignore_ascii_case(document))
    });
    let image = |read: Result<Option<Vec<u8>>, String>| {
        read.map(|png| png.filter(|png| !png.is_empty()).map(Paste::Image))
    };
    if document {
        let text = read_text()?;
        // U+FFFC stands in for an embedded object: an image copied out of a
        // document on its own comes with only that as its text.
        let has_words = text
            .as_deref()
            .is_some_and(|text| text.chars().any(|c| !c.is_whitespace() && c != '\u{FFFC}'));
        if has_words {
            return Ok(text.map(Paste::Text));
        }
        return Ok(image(read_image())?.or(text.map(Paste::Text)));
    }
    match image(read_image())? {
        Some(image) => Ok(Some(image)),
        None => Ok(read_text()?.map(Paste::Text)),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn paste_payload(paste: Option<Paste>) -> Result<Option<Value>, String> {
    match paste {
        Some(Paste::Image(png)) => {
            let staged = super::clipboard_staging::stage_clipboard_png(&png)?;
            Ok(Some(
                serde_json::json!({ "kind": "image", "staged": staged }),
            ))
        }
        Some(Paste::Text(text)) => native_clipboard_text(text),
        None => Ok(None),
    }
}

/// The `file://` URL of the thing a pasteboard entry actually points at, as a
/// path the rest of the app can open.
///
/// Finder's Copy does **not** write a path URL. It writes a *file reference*
/// URL — `file:///.file/id=6571367.424488125` — and that string is what
/// `public.file-url` yields verbatim. Forwarding it meant the upload path tried
/// `open("/.file/id=6571367.424488125")`, where `/.file` is a synthetic node
/// rather than a directory, so every Finder-copied file was refused with
/// `upload source is unavailable or unsafe: Not a directory (os error 20)`.
/// Nothing downstream can undo this: only Foundation knows how to resolve a
/// file reference, so it is resolved here, at the one place the pasteboard is
/// read.
///
/// `filePathURL` returns a path URL unchanged and resolves a reference URL
/// against the filesystem, so both flavours arrive as the same thing. `None`
/// means the entry did not name a file this process can reach, and the caller
/// treats that as "the clipboard holds no files" rather than passing an
/// unopenable path down the upload path.
#[cfg(target_os = "macos")]
fn file_path_url(raw: &str) -> Option<String> {
    use objc2_foundation::{NSString, NSURL};

    if !raw.starts_with("file://") || raw.contains(['\r', '\n', '\0']) {
        return None;
    }
    let resolved = NSURL::URLWithString(&NSString::from_str(raw))?
        .filePathURL()?
        .absoluteString()?
        .to_string();
    // Re-checked rather than assumed: this is the string the upload path turns
    // into a filesystem path, and it comes back out of Foundation, not out of
    // the check above.
    if !resolved.starts_with("file:///") || resolved.contains(['\r', '\n', '\0']) {
        return None;
    }
    Some(resolved)
}

/// Bound a native clipboard text read to the same size the host accepts for a
/// single terminal input request, so an oversized paste is refused with a
/// message here instead of being rejected further down the bridge.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
fn native_clipboard_text(text: String) -> Result<Option<Value>, String> {
    if text.is_empty() {
        return Ok(None);
    }
    if text.len() > MAX_NATIVE_TEXT_BYTES {
        return Err(format!(
            "native clipboard text exceeds the {MAX_NATIVE_TEXT_BYTES}-byte terminal input bound"
        ));
    }
    Ok(Some(serde_json::json!({ "kind": "text", "text": text })))
}

#[cfg(test)]
mod tests {
    use super::{native_clipboard_text, validated_native_clipboard_write};

    #[test]
    fn native_clipboard_writes_are_nonempty_and_bounded() {
        assert_eq!(
            validated_native_clipboard_write("copied\nverbatim".into()).unwrap(),
            "copied\nverbatim"
        );
        assert!(validated_native_clipboard_write(String::new()).is_err());
        assert!(
            validated_native_clipboard_write("a".repeat(tmux_control::MAX_INPUT_REQUEST_BYTES + 1))
                .is_err()
        );
    }

    #[test]
    fn empty_clipboard_text_is_not_a_payload() {
        assert_eq!(native_clipboard_text(String::new()).unwrap(), None);
    }

    #[test]
    fn clipboard_text_is_returned_verbatim() {
        let payload = native_clipboard_text("echo hello\nworld".into())
            .unwrap()
            .expect("text payload");
        assert_eq!(payload["kind"], "text");
        assert_eq!(payload["text"], "echo hello\nworld");
    }

    #[test]
    fn oversized_clipboard_text_is_refused() {
        let oversized = "a".repeat(tmux_control::MAX_INPUT_REQUEST_BYTES + 1);
        let error = native_clipboard_text(oversized).unwrap_err();
        assert!(error.contains("terminal input bound"), "{error}");
    }

    mod choosing_a_paste {
        use super::super::{Paste, choose_paste};

        const PNG: &[u8] = b"\x89PNG\r\n\x1a\nrest";

        fn text(text: &str) -> impl FnOnce() -> Result<Option<String>, String> {
            let text = text.to_owned();
            move || Ok(Some(text))
        }

        fn image(bytes: &[u8]) -> impl FnOnce() -> Result<Option<Vec<u8>>, String> {
            let bytes = bytes.to_vec();
            move || Ok(Some(bytes))
        }

        /// The flavors Numbers put on the pasteboard for an 11-cell range, as
        /// captured on a Mac (iWork state markers left out): the PNG and TIFF
        /// are 0 bytes.
        const NUMBERS_RANGE: &[&str] = &[
            "com.apple.flat-rtfd",
            "NeXT RTFD pasteboard type",
            "com.adobe.pdf",
            "Apple PDF pasteboard type",
            "public.utf16-external-plain-text",
            "public.utf8-plain-text",
            "NSStringPboardType",
            "public.png",
            "Apple PNG pasteboard type",
            "com.apple.iWork.TSPDescription",
            "com.apple.iWork.TSPNativeData",
            "public.rtf",
            "public.html",
            "public.tiff",
            "NeXT TIFF v4.0 pasteboard type",
        ];

        #[test]
        fn a_numbers_range_pastes_its_cells_text() {
            let paste = choose_paste(
                NUMBERS_RANGE,
                image(&[]),
                text("Checksum Internal\nSummation"),
            );
            assert_eq!(
                paste.unwrap(),
                Some(Paste::Text("Checksum Internal\nSummation".into()))
            );
        }

        /// Excel, Word and LibreOffice render the selection as a real image;
        /// the text still wins, and the image is never read.
        #[test]
        fn a_document_copy_with_a_real_image_pastes_text() {
            for formats in [
                &[
                    "public.html",
                    "public.rtf",
                    "public.png",
                    "public.utf8-plain-text",
                ][..],
                &[
                    "text/html",
                    "TEXT/RTF",
                    "image/png",
                    "text/plain;charset=utf-8",
                ],
                &["com.adobe.pdf", "public.tiff", "public.utf8-plain-text"],
            ] {
                let paste = choose_paste(formats, || panic!("image read"), text("a\tb"));
                assert_eq!(
                    paste.unwrap(),
                    Some(Paste::Text("a\tb".into())),
                    "{formats:?}"
                );
            }
        }

        #[test]
        fn a_screenshot_pastes_as_an_image_without_reading_text() {
            let paste = choose_paste(["public.png"], image(PNG), || panic!("text read"));
            assert_eq!(paste.unwrap(), Some(Paste::Image(PNG.to_vec())));
        }

        /// A browser's Copy Image adds an `<img>` tag as HTML, and may add the
        /// URL as text. HTML alone does not make it a document.
        #[test]
        fn a_browser_image_copy_pastes_as_an_image() {
            let formats = ["public.html", "public.png", "public.utf8-plain-text"];
            let paste = choose_paste(formats, image(PNG), || panic!("text read"));
            assert_eq!(paste.unwrap(), Some(Paste::Image(PNG.to_vec())));
        }

        /// Safari's Copy Image writes RTFD, and the image's URL as plain
        /// text. RTFD does not make it a document.
        #[test]
        fn a_safari_image_copy_pastes_as_an_image() {
            let formats = [
                "public.tiff",
                "public.url",
                "public.utf8-plain-text",
                "com.apple.flat-rtfd",
            ];
            let paste = choose_paste(formats, image(PNG), || panic!("text read"));
            assert_eq!(paste.unwrap(), Some(Paste::Image(PNG.to_vec())));
        }

        /// Preview writes a TIFF and a PDF of the selection but no text.
        #[test]
        fn a_preview_image_copy_pastes_as_an_image() {
            let paste = choose_paste(["public.tiff", "com.adobe.pdf"], image(PNG), || Ok(None));
            assert_eq!(paste.unwrap(), Some(Paste::Image(PNG.to_vec())));
        }

        /// An image copied alone out of a document carries rich text whose
        /// only plain text is the object placeholder.
        #[test]
        fn an_image_copied_alone_from_a_document_pastes_as_an_image() {
            let formats = [
                "com.apple.flat-rtfd",
                "public.rtf",
                "public.tiff",
                "public.utf8-plain-text",
            ];
            let paste = choose_paste(formats, image(PNG), text("\u{FFFC}\n"));
            assert_eq!(paste.unwrap(), Some(Paste::Image(PNG.to_vec())));
        }

        /// The Numbers bug without the document flavors: an empty image offer
        /// is no image, on macOS, Wayland and X11 alike.
        #[test]
        fn an_empty_image_falls_through_to_text() {
            for formats in [
                ["public.png", "public.utf8-plain-text"],
                ["image/png", "text/plain"],
            ] {
                let paste = choose_paste(formats, image(&[]), text("cells"));
                assert_eq!(paste.unwrap(), Some(Paste::Text("cells".into())));
            }
        }

        #[test]
        fn an_empty_image_and_no_text_is_nothing() {
            let paste = choose_paste(["image/png"], image(&[]), || Ok(None));
            assert_eq!(paste.unwrap(), None);
        }

        #[test]
        fn a_failed_image_read_is_reported_not_skipped() {
            let paste = choose_paste(
                ["image/png", "text/plain"],
                || Err("read native clipboard PNG: gone".into()),
                || panic!("text read"),
            );
            assert_eq!(paste.unwrap_err(), "read native clipboard PNG: gone");
        }
    }

    #[test]
    fn a_uri_list_yields_only_its_file_entries() {
        assert_eq!(
            super::file_uris("# copied\r\nfile:///a%20b.txt\r\nhttps://x.test\r\n\r\nfile:///c\n"),
            ["file:///a%20b.txt", "file:///c"]
        );
    }

    #[cfg(target_os = "linux")]
    mod bounded_reads {
        use super::super::read_bounded;
        use std::io::Write;
        use std::os::unix::net::UnixStream;
        use std::time::{Duration, Instant};

        #[test]
        fn a_finished_writer_is_read_to_its_end() {
            let (mut reader, mut writer) = UnixStream::pair().expect("socket pair");
            writer.write_all(b"pasted").expect("write");
            drop(writer);
            assert_eq!(
                read_bounded(&mut reader, 64, Duration::from_secs(2)).unwrap(),
                b"pasted"
            );
        }

        /// The owning app answered but never finished: the read gives up
        /// instead of holding the clipboard lock, and every copy behind it,
        /// for as long as that app stays stuck.
        #[test]
        fn a_writer_that_never_finishes_runs_into_the_deadline() {
            let (mut reader, mut writer) = UnixStream::pair().expect("socket pair");
            writer.write_all(b"partial").expect("write");
            let started = Instant::now();
            let error = read_bounded(&mut reader, 64, Duration::from_millis(100)).unwrap_err();
            assert!(error.contains("did not hand it over"), "{error}");
            assert!(started.elapsed() < Duration::from_secs(2));
        }

        #[test]
        fn content_past_the_bound_is_refused() {
            let (mut reader, mut writer) = UnixStream::pair().expect("socket pair");
            writer.write_all(&[b'a'; 65]).expect("write");
            drop(writer);
            let error = read_bounded(&mut reader, 64, Duration::from_secs(2)).unwrap_err();
            assert!(error.contains("64-byte bound"), "{error}");
        }
    }

    #[cfg(target_os = "macos")]
    mod macos_file_urls {
        use super::super::file_path_url;
        use objc2_foundation::{NSString, NSURL};
        use std::fs;

        /// A name with everything a `file:` URL has to escape, so the round trip
        /// is exercised rather than asserted. Foundation does the encoding on
        /// the way in and the decoding on the way out — a hand-rolled codec here
        /// would only be testing itself.
        #[test]
        fn a_path_url_survives_its_percent_encoding_both_ways() {
            let directory = std::env::temp_dir().join(format!("ade-clip-{}", std::process::id()));
            fs::create_dir_all(&directory).expect("scratch directory");
            let file = directory.join("a file #1 100% ?x.txt");
            fs::write(&file, b"x").expect("scratch file");

            let raw =
                NSURL::fileURLWithPath(&NSString::from_str(file.to_str().expect("utf-8 path")))
                    .absoluteString()
                    .expect("a file URL")
                    .to_string();
            assert!(
                raw.contains("%23"),
                "the fixture must exercise escaping: {raw}"
            );

            let resolved = file_path_url(&raw).expect("a real file resolves");
            let path = NSURL::URLWithString(&NSString::from_str(&resolved))
                .expect("a URL")
                .path()
                .expect("a filesystem path")
                .to_string();
            assert_eq!(
                fs::canonicalize(path).unwrap(),
                fs::canonicalize(&file).unwrap()
            );

            fs::remove_dir_all(&directory).ok();
        }

        /// The regression, from the side Finder actually exercises: the URL a
        /// Copy in Finder puts on the pasteboard is a *file reference* URL, and
        /// what the upload path needs back is the path it stands for. Built
        /// through `fileReferenceURL` rather than typed out, because the id in
        /// one belongs to a real inode on the machine running the test.
        #[test]
        fn a_finder_file_reference_url_resolves_to_the_file_it_stands_for() {
            let directory = std::env::temp_dir().join(format!("ade-ref-{}", std::process::id()));
            fs::create_dir_all(&directory).expect("scratch directory");
            let file = directory.join("finder copy.txt");
            fs::write(&file, b"x").expect("scratch file");

            let reference =
                NSURL::fileURLWithPath(&NSString::from_str(file.to_str().expect("utf-8 path")))
                    .fileReferenceURL()
                    .expect("a file reference URL")
                    .absoluteString()
                    .expect("a file URL")
                    .to_string();
            assert!(
                reference.contains("/.file/id="),
                "the fixture must be a reference URL: {reference}"
            );

            let resolved = file_path_url(&reference).expect("a reference URL resolves");
            assert!(
                !resolved.contains("/.file/id="),
                "a reference URL must not be forwarded: {resolved}"
            );
            let path = NSURL::URLWithString(&NSString::from_str(&resolved))
                .expect("a URL")
                .path()
                .expect("a filesystem path")
                .to_string();
            assert_eq!(
                fs::canonicalize(path).unwrap(),
                fs::canonicalize(&file).unwrap()
            );

            fs::remove_dir_all(&directory).ok();
        }

        /// The other half: a reference URL that stands for nothing is refused
        /// rather than handed on as `/.file/id=…`, which no `open(2)` can follow.
        #[test]
        fn an_unresolvable_file_reference_url_is_refused_not_forwarded() {
            assert_eq!(file_path_url("file:///.file/id=1.1"), None);
        }

        #[test]
        fn a_non_file_url_is_refused() {
            assert_eq!(file_path_url("https://example.test/x"), None);
            assert_eq!(file_path_url("file:///tmp/a\nb"), None);
        }
    }
}
