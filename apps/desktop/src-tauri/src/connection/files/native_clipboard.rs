use serde_json::{Value, json};

#[tauri::command]
pub async fn read_native_terminal_clipboard() -> Result<Option<Value>, String> {
    #[cfg(target_os = "linux")]
    {
        tauri::async_runtime::spawn_blocking(read_linux_clipboard)
            .await
            .map_err(|error| format!("native clipboard worker failed: {error}"))?
    }
    #[cfg(not(target_os = "linux"))]
    Ok(None)
}

#[cfg(target_os = "linux")]
fn read_linux_clipboard() -> Result<Option<Value>, String> {
    use clipboard_rs::{Clipboard, ClipboardContext, ContentFormat};

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
    let Some(png_format) = png_format else {
        return Ok(None);
    };
    let png = clipboard
        .get_buffer(png_format)
        .map_err(|error| format!("read native clipboard PNG: {error}"))?;
    let staged = super::clipboard_staging::stage_clipboard_png(&png)?;
    Ok(Some(json!({ "kind": "image", "staged": staged })))
}
