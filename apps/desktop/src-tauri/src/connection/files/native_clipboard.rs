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
            let uri = uri.to_string();
            if !uri.starts_with("file://") || uri.contains(['\r', '\n', '\0']) {
                uris.clear();
                break;
            }
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
}
