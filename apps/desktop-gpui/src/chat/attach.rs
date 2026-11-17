//! Files pasted or dropped into a chat, and the chips that show them (S6,
//! docs/design/chat-polish-plan.md §3.1).
//!
//! A chat is an [`UploadTarget`]: each file goes through the terminal's upload
//! batch into the host's staging directory, one batch at a time, and the
//! verified host path is what the message names. The host attaches only files
//! from that directory, so a file goes there on this machine's own host too;
//! there the copy is quick, and its chip shows no bar.
//!
//! Each file is its own batch, so one that fails is retried or removed alone.
//! Progress is the upload's record in [`Uploads`], read by its transfer.

use std::{path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use desktop_core::connection::{
    ConnectionSpec, chat::ChatAttachment, files::native_clipboard::read_native_terminal_clipboard,
};
use desktop_core::platform;
use gpui_kit::TestSupportExt as _;
use gpui_kit::{
    AnyElement, App, Context, ExternalPaths, FontWeight, InteractiveElement, IntoElement,
    ObjectFit, ParentElement, Role, SharedString, StatefulInteractiveElement, Styled,
    StyledImage as _, Window,
    component::{h_flex, tooltip::Tooltip, v_flex},
    div, img,
    prelude::FluentBuilder,
    relative,
};
use serde_json::Value;

use crate::{
    connection_ui::backend::TerminalBackend,
    terminal::integration::{
        transfers::{InternalPathDrag, NativeClipboard, TransferScope, parse_file_uri_list},
        upload::{self, Delivery, Source, UploadTarget, Uploads},
    },
    theme::{Palette, metrics::ui},
};

use super::{
    icon::{IconName, icon},
    pane::ChatPane,
};

/// The most files one message carries; the host holds the same bound.
pub(super) const MAX_ATTACHMENTS: usize = 10;
/// The largest image, the upload path's own limit for one.
pub(super) const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;
/// How long an uploaded chip shows its check.
const UPLOADED_FOR: Duration = Duration::from_millis(1500);
/// How often a clipboard read on the runtime is checked for its answer.
const READ_POLL: Duration = Duration::from_millis(20);
/// The group the pane's drop hint answers to.
pub(super) const DROP_GROUP: &str = "chat-pane-drop";
/// Marks a chat's uploads in [`Uploads`]: the chip shows them, so the
/// terminal's upload list leaves them out.
pub const CHAT_UPLOAD_PREFIX: &str = "chat:";

/// Where a chip's file is.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Upload {
    /// Waiting for the batch before it.
    Queued,
    /// Its batch runs: the transfer it is at, once one has started.
    Uploading(Option<String>),
    /// On the host.
    Ready,
    /// The upload failed, and why.
    Failed(String),
}

/// One file in the composer, or in a message being sent.
#[derive(Clone, Debug)]
pub(super) struct Attachment {
    pub id: u64,
    pub name: String,
    /// The file on this machine; empty for one that came from the host, as
    /// an edited message's do.
    pub local: String,
    /// The clipboard's staged PNG, uploaded as a screenshot.
    pub clipboard: bool,
    /// An image type, such as `image/png`; empty for any other file.
    pub media_type: String,
    pub size: u64,
    /// Where it is on the host, once it is there.
    pub host: Option<String>,
    pub upload: Upload,
}

impl Attachment {
    pub fn is_image(&self) -> bool {
        !self.media_type.is_empty()
    }

    /// As SEND names it: only one that is on the host.
    pub fn wire(&self) -> Option<ChatAttachment> {
        Some(ChatAttachment {
            path: self.host.clone()?,
            name: self.name.clone(),
            media_type: self.media_type.clone(),
            size_bytes: self.size,
        })
    }

    /// One of a message the host holds, ready to be sent again.
    pub fn from_host(id: u64, attachment: &ChatAttachment) -> Option<Self> {
        if attachment.path.is_empty() {
            return None;
        }
        Some(Self {
            id,
            name: attachment.name.clone(),
            local: String::new(),
            clipboard: false,
            media_type: attachment.media_type.clone(),
            size: attachment.size_bytes,
            host: Some(attachment.path.clone()),
            upload: Upload::Ready,
        })
    }
}

/// An image type for a file named `name`, from its extension. The host reads
/// the file's own bytes before it hands one over as an image.
pub(super) fn media_type_of(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension.to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "",
    }
}

fn basename(path: &str) -> String {
    crate::terminal::integration::transfers::basename(path)
}

/// `1.4 MB`, `860 KB`: a size as a chip says it, in binary units.
pub(super) fn size_label(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{} KB", (bytes + KB / 2) / KB)
    } else {
        let tenths = (u128::from(bytes) * 10 + u128::from(MB) / 2) / u128::from(MB);
        format!("{}.{} MB", tenths / 10, tenths % 10)
    }
}

/// What a test reads in place of the native clipboard, as
/// `read_native_terminal_clipboard` answers.
#[cfg(test)]
pub(super) struct ClipboardRead(pub Rc<dyn Fn() -> Result<Option<Value>, String>>);

#[cfg(test)]
impl gpui_kit::Global for ClipboardRead {}

/// How a chat reads its connection for an upload. A seam, as the terminal's
/// is, so a test can say the host is a remote one.
pub(super) fn live_scope() -> upload::ScopeOf<ChatPane> {
    Rc::new(|pane: &ChatPane, cx: &App| pane.transfer_scope(cx))
}

impl UploadTarget for ChatPane {
    fn busy(&self) -> bool {
        self.batch_running
    }

    fn set_busy(&mut self, busy: bool, cx: &mut Context<Self>) {
        self.batch_running = busy;
        if busy {
            return;
        }
        // The batch ended one way or another: its chip has said how, so
        // the next file goes.
        self.uploading = None;
        if let Some(window) = self.upload_window {
            let pane = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, cx| {
                    pane.update(cx, |pane, cx| pane.pump_uploads(window, cx))
                        .ok();
                });
            });
        }
        cx.notify();
    }

    fn backend(&self, cx: &App) -> Arc<dyn TerminalBackend> {
        self.connection_backend(cx)
    }

    /// Staged on this machine's host too: the host attaches nothing from
    /// outside its staging directory.
    fn in_place(&self) -> bool {
        false
    }

    /// A staged file's name is never seen: two of one name are both kept.
    fn reviews_collisions(&self) -> bool {
        false
    }

    fn started(&mut self, _: &str, transfer_id: &str, cx: &mut Context<Self>) {
        if let Some(attachment) = self.uploading_attachment() {
            attachment.upload = Upload::Uploading(Some(transfer_id.to_owned()));
            cx.notify();
        }
    }

    fn deliver(&mut self, delivery: Delivery, cx: &mut Context<Self>) -> Result<(), String> {
        let Some((_, host)) = delivery.paths.into_iter().next() else {
            return Err("Upload completed without a verified destination.".into());
        };
        let local = (self.upload_scope)(self, cx).is_some_and(|scope| scope.local);
        let Some(attachment) = self.uploading_attachment() else {
            // Removed while it uploaded: the file stays in the staging
            // directory, which forgets it after seven days.
            return Ok(());
        };
        attachment.host = Some(host.clone());
        attachment.upload = Upload::Ready;
        let (id, preview) = (attachment.id, attachment.local.clone());
        if !preview.is_empty() {
            self.previews.insert(host, preview);
        }
        // A moment's check where the bar was; this machine's own host shows
        // neither.
        if !local {
            let wait = cx.background_executor().timer(UPLOADED_FOR);
            let done = cx.spawn(async move |this, cx| {
                wait.await;
                this.update(cx, |this, cx| {
                    this.uploaded.remove(&id);
                    cx.notify();
                })
                .ok();
            });
            self.uploaded.insert(id, done);
        }
        cx.notify();
        Ok(())
    }

    fn stopped(&mut self, failure: Option<String>, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(attachment) = self.uploading_attachment() {
            attachment.upload =
                Upload::Failed(failure.unwrap_or_else(|| "The upload was stopped.".to_owned()));
            cx.notify();
        }
    }
}

impl ChatPane {
    fn uploading_attachment(&mut self) -> Option<&mut Attachment> {
        let id = self.uploading?;
        self.attachments
            .iter_mut()
            .find(|attachment| attachment.id == id)
    }

    /// The connection an upload into this chat binds to, now: the one the
    /// chat is open on.
    pub(super) fn transfer_scope(&self, cx: &App) -> Option<TransferScope> {
        let (client_id, terminal_epoch) = self.live_link()?;
        let link = self.controller().read(cx).link(&self.profile_id)?;
        Some(TransferScope {
            client_id,
            profile_id: self.profile_id.clone(),
            server_identity: self.server_identity.clone(),
            terminal_epoch,
            pane_id: format!("{CHAT_UPLOAD_PREFIX}{}", self.pane_id),
            local: matches!(link.connection, ConnectionSpec::Local),
        })
    }

    fn connection_backend(&self, cx: &App) -> Arc<dyn TerminalBackend> {
        self.controller().read(cx).backend()
    }

    /// Adds files from this machine to the composer, as many as fit, and
    /// starts their uploads. Says why when one is left out.
    pub(super) fn attach_paths(
        &mut self,
        paths: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut refusal = None;
        for path in paths {
            let name = basename(&path);
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => {
                    refusal.get_or_insert(format!("{name} is a folder. Attach the files in it."));
                }
                Ok(metadata) => {
                    let media_type = media_type_of(&name);
                    if let Err(why) =
                        self.add_attachment(path, name, false, media_type, metadata.len())
                    {
                        refusal.get_or_insert(why);
                    }
                }
                Err(_) => {
                    refusal.get_or_insert(format!("{name} cannot be read."));
                }
            }
        }
        self.attached(refusal, window, cx);
    }

    /// Adds the image the clipboard held, staged as a PNG.
    fn attach_clipboard_image(
        &mut self,
        path: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let size = std::fs::metadata(&path).map_or(0, |metadata| metadata.len());
        let refusal = self
            .add_attachment(path, name, true, "image/png", size)
            .err();
        self.attached(refusal, window, cx);
    }

    fn add_attachment(
        &mut self,
        local: String,
        name: String,
        clipboard: bool,
        media_type: &str,
        size: u64,
    ) -> Result<(), String> {
        if self.attachments.len() >= MAX_ATTACHMENTS {
            return Err(format!(
                "A message takes at most {MAX_ATTACHMENTS} attachments."
            ));
        }
        if !media_type.is_empty() && size > MAX_IMAGE_BYTES {
            return Err(format!("{name} is over 25 MB, the most an image can be."));
        }
        self.next_attachment += 1;
        self.attachments.push(Attachment {
            id: self.next_attachment,
            name,
            local,
            clipboard,
            media_type: media_type.to_owned(),
            size,
            host: None,
            upload: Upload::Queued,
        });
        Ok(())
    }

    fn attached(&mut self, refusal: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.seen(cx);
        match refusal {
            Some(why) => self.notice = Some(why.into()),
            None => self.notice = None,
        }
        self.pump_uploads(window, cx);
        cx.notify();
    }

    /// Starts the next queued file's batch, unless one runs.
    pub(super) fn pump_uploads(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.uploading.is_some() || self.batch_running {
            return;
        }
        let Some(index) = self
            .attachments
            .iter()
            .position(|attachment| attachment.upload == Upload::Queued)
        else {
            return;
        };
        let scope = (self.upload_scope)(self, cx);
        let next = &mut self.attachments[index];
        if scope.is_none() {
            next.upload = Upload::Failed("The host is not connected.".into());
            cx.notify();
            // The rest wait for a retry too: nothing can go now.
            return self.pump_uploads(window, cx);
        }
        next.upload = Upload::Uploading(None);
        let source = if next.clipboard {
            Source::StagedImage {
                path: next.local.clone(),
                name: next.name.clone(),
            }
        } else {
            Source::Paths(vec![next.local.clone()])
        };
        self.uploading = Some(next.id);
        self.upload_window = Some(window.window_handle());
        let pane = cx.entity();
        let scope_of = self.upload_scope.clone();
        // Outside this update: the batch reads and updates the pane itself.
        window.defer(cx, move |window, cx| {
            upload::accept(&pane, source, scope_of, window, cx);
            pane.update(cx, |pane, cx| {
                // Refused before it began: nothing will say how it ended.
                if !pane.batch_running && pane.uploading.is_some() {
                    let failure = Some("The upload could not start.".to_owned());
                    UploadTarget::stopped(pane, failure, window, cx);
                    pane.set_busy(false, cx);
                }
            });
        });
        cx.notify();
    }

    /// Takes a file out of the composer, stopping its upload.
    pub(super) fn remove_attachment(&mut self, id: u64, cx: &mut Context<Self>) {
        self.seen(cx);
        let Some(index) = self.attachments.iter().position(|a| a.id == id) else {
            return;
        };
        let removed = self.attachments.remove(index);
        match &removed.upload {
            Upload::Uploading(Some(transfer)) => {
                let _ = self.connection_backend(cx).cancel_upload(transfer.clone());
            }
            Upload::Failed(_) => self.forget_record(&removed, cx),
            _ => {}
        }
        // One whose batch runs leaves `uploading` as it is: the batch still
        // ends, finds no chip to tell, and lets the next file go.
        self.uploaded.remove(&id);
        cx.notify();
    }

    /// Uploads a failed file again.
    pub(super) fn retry_attachment(
        &mut self,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.seen(cx);
        let Some(attachment) = self.attachments.iter_mut().find(|a| a.id == id) else {
            return;
        };
        if !matches!(attachment.upload, Upload::Failed(_)) {
            return;
        }
        let failed = attachment.clone();
        attachment.upload = Upload::Queued;
        self.forget_record(&failed, cx);
        self.notice = None;
        self.pump_uploads(window, cx);
        cx.notify();
    }

    /// A failed upload's record is the chip's to show; once it is retried or
    /// removed the record goes.
    fn forget_record(&self, attachment: &Attachment, cx: &mut App) {
        let ids: Vec<String> = cx
            .try_global::<Uploads>()
            .map(|uploads| {
                uploads
                    .0
                    .records
                    .iter()
                    .filter(|record| {
                        record.pane_id.starts_with(CHAT_UPLOAD_PREFIX)
                            && record.progress.source_path == attachment.local
                    })
                    .map(|record| record.progress.transfer_id.clone())
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            cx.default_global::<Uploads>().0.dismiss(&id);
        }
    }

    /// How far a chip's upload is, as a share, while it runs.
    fn progress(&self, attachment: &Attachment, cx: &App) -> Option<f32> {
        let Upload::Uploading(transfer) = &attachment.upload else {
            return None;
        };
        let tenths = transfer.as_ref().and_then(|transfer| {
            let uploads = cx.try_global::<Uploads>()?;
            let record = uploads
                .0
                .records
                .iter()
                .find(|record| record.progress.transfer_id == *transfer)?;
            record.progress.progress_tenths()
        });
        Some(tenths.unwrap_or(0) as f32 / 1000.)
    }

    /// Why the composer's files cannot go yet, if they cannot.
    pub(super) fn attachments_refusal(&self) -> Option<&'static str> {
        let failed = self
            .attachments
            .iter()
            .any(|attachment| matches!(attachment.upload, Upload::Failed(_)));
        if failed {
            return Some("An attachment failed to upload. Retry or remove it.");
        }
        let moving = self
            .attachments
            .iter()
            .any(|attachment| attachment.upload != Upload::Ready);
        moving.then_some("The attachments are still uploading.")
    }

    /// Whether an upload still runs or waits: Send says "Uploading…".
    pub(super) fn uploading_any(&self) -> bool {
        self.attachments
            .iter()
            .any(|attachment| matches!(attachment.upload, Upload::Queued | Upload::Uploading(_)))
    }

    // Paste and drop.

    /// Port of the terminal's paste for the composer: copied files and an
    /// image become attachments, and text is inserted as the field would.
    pub(super) fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.seen(cx);
        #[cfg(test)]
        if let Some(read) = cx.try_global::<ClipboardRead>() {
            let answer = (read.0)();
            return self.pasted(answer, window, cx);
        }
        // Read on the runtime and polled, as the terminal reads it.
        let read = platform::spawn(read_native_terminal_clipboard());
        cx.spawn_in(window, async move |this, cx| {
            while !read.is_finished() {
                cx.background_executor().timer(READ_POLL).await;
            }
            let answer = match read.await {
                Ok(answer) => answer,
                Err(error) => Err(format!("native clipboard read failed: {error}")),
            };
            this.update_in(cx, |this, window, cx| this.pasted(answer, window, cx))
                .ok();
        })
        .detach();
    }

    fn pasted(
        &mut self,
        answer: Result<Option<Value>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let native = answer.and_then(|answer| answer.map(NativeClipboard::from_wire).transpose());
        let text = match native {
            Ok(Some(NativeClipboard::Files { uris })) => {
                match parse_file_uri_list(&uris.join("\n")) {
                    Ok(paths) => self.attach_paths(paths, window, cx),
                    Err(error) => self.refused(&error, cx),
                }
                return;
            }
            Ok(Some(NativeClipboard::Image { staged })) => {
                return self.attach_clipboard_image(staged.path, staged.name, window, cx);
            }
            Ok(Some(NativeClipboard::Text { text })) => text,
            // Nothing the native reader knows: what GPUI's own clipboard holds.
            Ok(None) => cx
                .read_from_clipboard()
                .and_then(|item| item.text())
                .unwrap_or_default(),
            Err(error) => return self.refused(&error, cx),
        };
        if !text.is_empty() {
            self.insert_text(&text, window, cx);
        }
    }

    /// Text in place of the composer's selection, as a paste puts it.
    fn insert_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_owned();
        self.composer.update(cx, |composer, cx| {
            composer.replace(text, window, cx);
        });
        self.slash_active = 0;
        self.slash_dismissed = None;
        cx.notify();
    }

    /// Files dropped on the pane from outside the app.
    pub(super) fn dropped(
        &mut self,
        paths: &ExternalPaths,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths: Vec<String> = paths
            .paths()
            .iter()
            .filter_map(|path| path.to_str().map(str::to_owned))
            .collect();
        if paths.is_empty() {
            return self.refused("The dropped files have no paths this app can read.", cx);
        }
        self.attach_paths(paths, window, cx);
        window.focus(&self.input_focus(cx), cx);
    }

    /// A file dragged from inside the app is on the host already: its path
    /// goes into the message, for the agent to read as it reads any path.
    pub(super) fn dropped_from_app(
        &mut self,
        drag: &InternalPathDrag,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.seen(cx);
        let target = Some((self.profile_id.as_str(), self.server_identity.as_str()));
        // Checked as a terminal checks it; the path goes in as it is.
        match drag.shell_text(target) {
            Ok(_) => {
                let text = format!("{} ", drag.path);
                self.insert_text(&text, window, cx);
                window.focus(&self.input_focus(cx), cx);
            }
            Err(error) => self.refused(&error, cx),
        }
    }

    // Drawing.

    /// `.atts`: the composer's chips, in a row above the text.
    pub(super) fn composer_chips(&self, p: &Palette, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.attachments.is_empty() {
            return None;
        }
        let chips = self
            .attachments
            .iter()
            .map(|attachment| self.composer_chip(attachment, p, cx));
        Some(
            h_flex()
                .id("chat-attachments")
                .test_support()
                .role(Role::List)
                .aria_label("Attachments")
                .flex_wrap()
                .gap(ui(8.))
                .px(ui(10.))
                .pt(ui(10.))
                .children(chips)
                .into_any_element(),
        )
    }

    fn composer_chip(
        &self,
        attachment: &Attachment,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = attachment.id;
        let local = (self.upload_scope)(self, cx).is_none_or(|scope| scope.local);
        let failed = match &attachment.upload {
            Upload::Failed(why) => Some(why.clone()),
            _ => None,
        };
        let progress = self.progress(attachment, cx).filter(|_| !local);
        let just_uploaded = self.uploaded.contains_key(&id);
        let (accent, danger, faint, dim) = (p.accent, p.danger, p.chrome_faint, p.chrome_dim);
        let link = |key: &str, label: &'static str, ink| {
            div()
                .id(SharedString::from(format!("chat-attachment-{key}-{id}")))
                .test_support()
                .role(Role::Button)
                .aria_label(SharedString::from(format!("{label} {}", attachment.name)))
                .ml(ui(6.))
                .cursor_pointer()
                .tab_index(0)
                .text_color(ink)
                .child(label)
        };
        let detail: AnyElement =
            match &failed {
                Some(_) => h_flex()
                    .text_color(danger)
                    .child("Upload failed")
                    .child(link("retry", "Retry", accent).on_click(cx.listener(
                        move |this, _, window, cx| this.retry_attachment(id, window, cx),
                    )))
                    .child(link("remove", "Remove", dim).on_click(
                        cx.listener(move |this, _, _, cx| this.remove_attachment(id, cx)),
                    ))
                    .into_any_element(),
                None => h_flex()
                    .gap(ui(4.))
                    .when(just_uploaded, |row| {
                        row.child(icon(IconName::Check, 12., p.ok))
                    })
                    .child(size_label(attachment.size))
                    .into_any_element(),
            };
        let moving = matches!(attachment.upload, Upload::Queued | Upload::Uploading(_));
        let state = match (&failed, moving, just_uploaded) {
            (Some(why), _, _) => format!("upload failed: {why}"),
            (None, true, _) => "uploading".to_owned(),
            (None, false, true) => "uploaded".to_owned(),
            _ => size_label(attachment.size),
        };
        let remove = (failed.is_none()).then(|| {
            let (hover, strong) = (p.chrome_hover, p.chrome_ink_strong);
            div()
                .id(SharedString::from(format!("chat-attachment-x-{id}")))
                .test_support()
                .role(Role::Button)
                .aria_label(SharedString::from(format!("Remove {}", attachment.name)))
                .absolute()
                .top(ui(3.))
                .right(ui(3.))
                .size(ui(18.))
                .rounded_full()
                .bg(p.chrome_bg)
                .border_1()
                .border_color(p.chrome_border)
                .flex()
                .items_center()
                .justify_center()
                .text_color(p.chrome_dim)
                .cursor_pointer()
                .tab_index(0)
                // `.att:hover .rm`, and on focus.
                .opacity(0.)
                .group_hover(chip_group(id), |style| style.opacity(1.))
                .focus_visible(move |style| style.opacity(1.).bg(hover).text_color(strong))
                .tooltip(|window, cx| Tooltip::new("Remove").build(window, cx))
                .child(icon(IconName::X, 11., p.chrome_dim))
                .on_click(cx.listener(move |this, _, _, cx| this.remove_attachment(id, cx)))
        });
        let bar = progress.map(|share| {
            div()
                .id(SharedString::from(format!("chat-attachment-progress-{id}")))
                .test_support()
                .absolute()
                .left_0()
                .bottom_0()
                .h(ui(2.))
                .w(relative(share))
                .bg(accent)
        });
        chip(
            format!("chat-attachment-{id}"),
            attachment,
            self.preview_of(attachment),
            state,
            detail,
            failed.is_some(),
            p,
        )
        .when(failed.is_some(), |chip| chip.max_w(ui(260.)))
        .text_color(faint)
        .children(remove)
        .children(bar)
        .test_support()
        .into_any_element()
    }

    /// The file a chip's thumbnail is drawn from, when this machine has it.
    fn preview_of(&self, attachment: &Attachment) -> Option<String> {
        if !attachment.is_image() {
            return None;
        }
        if !attachment.local.is_empty() {
            return Some(attachment.local.clone());
        }
        let host = attachment.host.as_ref()?;
        self.previews.get(host).cloned()
    }

    /// `.u .atts`: a sent message's chips, above its text. One opens its file
    /// in the app.
    pub(super) fn message_chips(
        &self,
        key: &str,
        attachments: &[ChatAttachment],
        p: &Palette,
    ) -> Option<AnyElement> {
        if attachments.is_empty() {
            return None;
        }
        let pane_id = self.pane_id.to_string();
        let chips = attachments.iter().enumerate().map(|(index, sent)| {
            let shown = Attachment {
                id: index as u64,
                name: sent.name.clone(),
                local: String::new(),
                clipboard: false,
                media_type: sent.media_type.clone(),
                size: sent.size_bytes,
                host: (!sent.path.is_empty()).then(|| sent.path.clone()),
                upload: Upload::Ready,
            };
            let size = if sent.size_bytes > 0 {
                size_label(sent.size_bytes)
            } else {
                String::new()
            };
            let detail = div().child(size.clone()).into_any_element();
            let path = sent.path.clone();
            let pane_id = pane_id.clone();
            chip(
                format!("chat-sent-{key}-{index}"),
                &shown,
                self.preview_of(&shown),
                size,
                detail,
                false,
                p,
            )
            .bg(p.chrome_bg)
            .text_color(p.chrome_faint)
            .when(!path.is_empty(), |chip| {
                chip.cursor_pointer().on_click(move |_, _, cx: &mut App| {
                    crate::editor::open_terminal_path(&pane_id, &path, None, cx);
                })
            })
            .test_support()
            .into_any_element()
        });
        Some(
            h_flex()
                .flex_wrap()
                .justify_end()
                .gap(ui(8.))
                .pb(ui(8.))
                .children(chips)
                .into_any_element(),
        )
    }
}

fn chip_group(id: u64) -> SharedString {
    SharedString::from(format!("chat-attachment-group-{id}"))
}

/// `.att`: a thumbnail or a file mark, the name, and a line under it.
fn chip(
    id: String,
    attachment: &Attachment,
    preview: Option<String>,
    state: String,
    detail: AnyElement,
    failed: bool,
    p: &Palette,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let label = SharedString::from(format!("{}, {state}", attachment.name));
    let mark = match preview {
        Some(path) => div()
            .flex_none()
            .size(ui(44.))
            .rounded(ui(6.))
            .overflow_hidden()
            .border_1()
            .border_color(p.chrome_hairline)
            .child(
                img(PathBuf::from(path))
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            ),
        None => div()
            .flex_none()
            .size(ui(44.))
            .rounded(ui(6.))
            .bg(p.chrome_bg)
            .border_1()
            .border_color(p.chrome_hairline)
            .flex()
            .items_center()
            .justify_center()
            .child(icon(
                if attachment.is_image() {
                    IconName::Image
                } else {
                    IconName::File
                },
                16.,
                p.chrome_dim,
            )),
    };
    let group = chip_group(attachment.id);
    h_flex()
        .id(SharedString::from(id))
        .role(Role::Group)
        .aria_label(label)
        .group(group)
        .relative()
        .flex_none()
        .gap(ui(9.))
        .h(ui(54.))
        .pl(ui(4.))
        .pr(ui(12.))
        .py(ui(4.))
        .min_w_0()
        .max_w(ui(164.))
        .overflow_hidden()
        .border_1()
        .rounded(ui(9.))
        .map(|chip| {
            if failed {
                chip.border_color(p.danger).bg(p.danger_wash)
            } else {
                chip.border_color(p.chrome_border).bg(p.chrome_raised)
            }
        })
        .child(mark)
        .child(
            v_flex()
                .min_w_0()
                .text_size(ui(12.))
                .line_height(relative(1.35))
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(p.chrome_ink_strong)
                        .child(SharedString::from(attachment.name.clone())),
                )
                .child(div().text_size(ui(11.)).whitespace_nowrap().child(detail)),
        )
}

/// `.drop`: over the whole pane while files are dragged over it, as the
/// terminal's drop hint is.
pub(super) fn drop_hint(p: &Palette) -> AnyElement {
    div()
        .id("chat-drop-hint")
        .role(Role::Status)
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .invisible()
        .bg(p.accent_wash)
        .border_2()
        .border_color(p.accent)
        .group_drag_over::<ExternalPaths>(DROP_GROUP, |style| style.visible())
        .group_drag_over::<InternalPathDrag>(DROP_GROUP, |style| style.visible())
        .child(
            h_flex()
                .gap(ui(7.))
                .px(ui(12.))
                .py(ui(5.))
                .rounded_full()
                .border_1()
                .border_color(p.accent)
                .bg(p.chrome_bg)
                .shadow_lg()
                .text_size(ui(13.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.accent)
                .child(icon(IconName::Paperclip, 16., p.accent))
                .child("Drop to attach"),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_is_known_by_its_extension_and_a_size_reads_as_a_chip_says_it() {
        assert_eq!(media_type_of("card-overlap.PNG"), "image/png");
        assert_eq!(media_type_of("photo.jpeg"), "image/jpeg");
        assert_eq!(media_type_of("muxflow-gpui.log"), "");
        assert_eq!(media_type_of("README"), "");
        assert_eq!(size_label(1_468_006), "1.4 MB");
        assert_eq!(size_label(880_640), "860 KB");
        assert_eq!(size_label(2_202_009), "2.1 MB");
        assert_eq!(size_label(12), "12 B");
    }
}
