use serde_json::Value;

#[tauri::command]
pub async fn read_native_terminal_clipboard() -> Result<Option<Value>, String> {
    #[cfg(target_os = "linux")]
    {
        tauri::async_runtime::spawn_blocking(read_linux_clipboard)
            .await
            .map_err(|error| format!("native clipboard worker failed: {error}"))?
    }
    #[cfg(target_os = "macos")]
    {
        tauri::async_runtime::spawn_blocking(read_macos_clipboard)
            .await
            .map_err(|error| format!("native clipboard worker failed: {error}"))?
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    Ok(None)
}

#[cfg(target_os = "linux")]
fn read_linux_clipboard() -> Result<Option<Value>, String> {
    use clipboard_rs::{Clipboard, ClipboardContext, ContentFormat};
    use serde_json::json;

    // clipboard-rs bounds X11 reads to 500 ms by default and selects its
    // Wayland implementation at runtime when a native display is available.
    let clipboard =
        ClipboardContext::new().map_err(|error| format!("open native clipboard: {error}"))?;

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
    if let Some(png_format) = png_format {
        let png = clipboard
            .get_buffer(png_format)
            .map_err(|error| format!("read native clipboard PNG: {error}"))?;
        let staged = super::clipboard_staging::stage_clipboard_png(&png)?;
        return Ok(Some(json!({ "kind": "image", "staged": staged })));
    }

    if !clipboard.has(ContentFormat::Text) {
        return Ok(None);
    }
    let text = clipboard
        .get_text()
        .map_err(|error| format!("read native clipboard text: {error}"))?;
    native_clipboard_text(text)
}

#[cfg(target_os = "macos")]
fn read_macos_clipboard() -> Result<Option<Value>, String> {
    use objc2_app_kit::{
        NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardTypeFileURL,
        NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardTypeTIFF,
    };
    use objc2_foundation::NSDictionary;
    use serde_json::json;

    const MAX_NATIVE_IMAGE_BYTES: usize = 32 * 1024 * 1024;

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

    if let Some(png) = pasteboard.dataForType(unsafe { NSPasteboardTypePNG }) {
        let bytes = png.to_vec();
        if bytes.len() > MAX_NATIVE_IMAGE_BYTES {
            return Err("native clipboard image exceeds 32 MiB decode bound".into());
        }
        let staged = super::clipboard_staging::stage_clipboard_png(&bytes)?;
        return Ok(Some(json!({ "kind": "image", "staged": staged })));
    }

    if let Some(tiff) = pasteboard.dataForType(unsafe { NSPasteboardTypeTIFF }) {
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
        let staged = super::clipboard_staging::stage_clipboard_png(&bytes)?;
        return Ok(Some(json!({ "kind": "image", "staged": staged })));
    }

    // Plain text is the last rung, so a copied file or image still wins. Without
    // it the webview would have to fall back to `navigator.clipboard`, which
    // WebKit refuses for content the page did not write itself — that refusal is
    // M10-E054: pasting from any other application silently did nothing.
    let Some(text) = pasteboard.stringForType(unsafe { NSPasteboardTypeString }) else {
        return Ok(None);
    };
    native_clipboard_text(text.to_string())
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
    const MAX_NATIVE_TEXT_BYTES: usize = tmux_control::MAX_INPUT_REQUEST_BYTES;

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
    use super::native_clipboard_text;

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

        /// The regression: a reference URL is either resolved to a real path or
        /// refused. It is never handed on as `/.file/id=…`, which no `open(2)`
        /// can follow.
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
