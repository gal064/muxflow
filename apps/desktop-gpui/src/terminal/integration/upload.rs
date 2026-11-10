//! Files and images pasted or dropped into a pane: port of the batch flow in
//! `TerminalTransferSurface.tsx`.
//!
//! On a local host a path is pasted as it is. On a remote one each file is
//! checked by a preflight, then uploaded, and only the verified destinations
//! are pasted. A batch is bound to the connection it started on; if the pane's
//! host, server or epoch changes, the batch stops and pastes nothing, because
//! its paths would name files on a host the pane no longer shows.
//!
//! `desktop-core` reports from its own threads, so events wait on a channel
//! this task drains on a timer, the same way the terminal link does.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

use desktop_core::connection::{
    ConnectionSpec, files::upload_manager::inspect_local_terminal_paths,
};
use desktop_core::platform::{EventBody, EventSink};
use gpui_kit::{
    App, AsyncWindowContext, Entity, Global, ParentElement, SharedString, Styled, WeakEntity,
    Window,
    component::{
        WindowExt,
        button::{Button, ButtonVariants},
        dialog::DialogEntrance,
        h_flex, v_flex,
    },
    div,
};

use crate::connection_ui::{ConnectionUi, backend::TerminalBackend};
use crate::notices;
use crate::theme::{
    self,
    metrics::{RADIUS_PANEL, TYPE_ROW, ui},
};

use super::super::pane::TerminalPane;
use super::transfers::{
    Preflight, Review, TransferEvent, TransferRecords, TransferScope, UploadCollisionPolicy,
    UploadRequest, basename, format_bytes, join_shell_escaped_paths, review_for,
    validate_agent_image_path,
};

/// How often a batch looks for new events and for its scope changing.
const POLL: Duration = Duration::from_millis(50);

/// The upload list every surface shows. A global, because a record outlives
/// the pane that started it.
#[derive(Default)]
pub struct Uploads(pub TransferRecords);

impl Global for Uploads {}

/// What a batch starts from.
pub enum Source {
    Paths(Vec<String>),
    /// An image the native clipboard staged as a PNG.
    StagedImage {
        path: String,
        name: String,
    },
}

/// Why a batch ended without pasting.
enum Stop {
    /// The pane or its connection changed: nothing to say.
    Silent,
    Failed(String),
}

impl From<String> for Stop {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

/// The pane's connection, as an upload binds to it.
pub fn scope_of(pane: &TerminalPane, cx: &App) -> Option<TransferScope> {
    let profile_id = pane.link().read(cx).profile_id();
    let controller = cx.try_global::<ConnectionUi>()?.0.read(cx);
    let host = controller.link(profile_id)?;
    Some(TransferScope {
        client_id: host.client_id.clone()?,
        profile_id: host.profile_id.clone(),
        server_identity: host.host_state.server_identity.clone()?,
        terminal_epoch: host.terminal_epoch,
        pane_id: pane.pane_id().to_string(),
        local: matches!(host.connection, ConnectionSpec::Local),
    })
}

/// How a batch reads the pane's current scope. A seam, so a test can run a
/// batch without a connection controller.
pub type ScopeOf = Rc<dyn Fn(&TerminalPane, &App) -> Option<TransferScope>>;

/// Starts a batch on `pane`. One batch at a time per pane, as in the Tauri app.
pub fn accept(
    pane: &Entity<TerminalPane>,
    source: Source,
    scope_of: ScopeOf,
    window: &mut Window,
    cx: &mut App,
) {
    let busy = pane.read(cx).integration.busy;
    let scope = scope_of(pane.read(cx), cx);
    let failure = match (&scope, busy) {
        (None, _) => Some("File paste requires a live terminal connection."),
        (Some(_), true) => Some("Wait for the current terminal transfer batch to finish."),
        _ => None,
    };
    if let Some(message) = failure {
        notices::notify(message, window, cx);
        return;
    }
    // A file list of comments alone names nothing to paste.
    if matches!(&source, Source::Paths(paths) if paths.is_empty()) {
        return;
    }
    let scope = scope.expect("checked above");
    let backend = pane.read(cx).link().read(cx).backend().clone();
    pane.update(cx, |pane, _| pane.integration.busy = true);
    let weak = pane.downgrade();
    window
        .spawn(cx, async move |cx| {
            let batch = Batch {
                pane: weak.clone(),
                backend,
                scope,
                scope_of,
            };
            let outcome = batch.run(source, cx).await;
            let _ = cx.update(|window, cx| {
                if let Err(Stop::Failed(message)) = outcome {
                    notices::notify(&message, window, cx);
                }
                let _ = weak.update(cx, |pane, _| pane.integration.busy = false);
            });
        })
        .detach();
}

struct Batch {
    pane: WeakEntity<TerminalPane>,
    backend: Arc<dyn TerminalBackend>,
    scope: TransferScope,
    scope_of: ScopeOf,
}

/// `desktop-core`'s events, queued for the batch's next poll.
struct EventQueue(Sender<EventBody>);

impl EventSink for EventQueue {
    fn send(&self, body: EventBody) -> Result<(), String> {
        self.0
            .send(body)
            .map_err(|_| "the terminal upload stopped listening".to_owned())
    }
}

impl Batch {
    async fn run(&self, source: Source, cx: &mut AsyncWindowContext) -> Result<(), Stop> {
        match source {
            Source::Paths(paths) if self.scope.local => {
                // Only readable regular files; the answer keeps the order asked.
                inspect_local_terminal_paths(paths.clone())?;
                self.paste(join_shell_escaped_paths(&paths)?, cx)
            }
            Source::StagedImage { path, .. } if self.scope.local => {
                self.paste(validate_agent_image_path(&path)?.to_owned(), cx)
            }
            Source::Paths(paths) => {
                let named = paths
                    .into_iter()
                    .map(|path| {
                        let name = basename(&path);
                        (path, name)
                    })
                    .collect();
                self.upload(named, false, cx).await
            }
            Source::StagedImage { path, name } => self.upload(vec![(path, name)], true, cx).await,
        }
    }

    /// Port of `acceptPaths` and `startRemoteBatch` for a remote host.
    async fn upload(
        &self,
        sources: Vec<(String, String)>,
        image_png: bool,
        cx: &mut AsyncWindowContext,
    ) -> Result<(), Stop> {
        let (sender, events) = channel();
        let sink: Arc<dyn EventSink> = Arc::new(EventQueue(sender));
        let mut items = Vec::new();
        let mut checked = Vec::new();
        // One at a time: a preflight opens its own short-lived connection and
        // must not compete with the two upload slots.
        for (path, name) in sources {
            let id = self.start(&path, &name, true, Default::default(), image_png, &sink)?;
            checked.push(id.clone());
            let done = self.wait(&events, &[id], cx).await?;
            items.push(Preflight::from_completed(&done[0], &name)?);
        }
        // The preflights have become uploads; their results are no news now.
        self.dismiss_delivered(&checked, cx);
        let policy = match review_for(&items) {
            Some(review) => self.ask(&review, &items, cx).await?,
            None => Policy::default(),
        };
        // The review may have outlived the connection it was asked about.
        if !self.still_current(cx) {
            return Err(Stop::Silent);
        }
        let mut ids = Vec::new();
        for item in &items {
            match self.start(
                &item.source_path,
                &item.name,
                false,
                policy,
                image_png,
                &sink,
            ) {
                Ok(id) => ids.push(id),
                // The ones already moving are stopped, and the batch waits for
                // them, so the list shows how each ended.
                Err(error) => {
                    for id in &ids {
                        let _ = self.backend.cancel_upload(id.clone());
                    }
                    let _ = self.wait(&events, &ids, cx).await;
                    return Err(Stop::Failed(error));
                }
            }
        }
        let done = self.wait(&events, &ids, cx).await?;
        let mut destinations = Vec::new();
        for event in &done {
            destinations.push(event.verified_destination().ok_or_else(|| {
                Stop::Failed("Upload completed without a verified destination.".into())
            })?);
        }
        let text = if image_png {
            destinations
                .iter()
                .map(|path| validate_agent_image_path(path).map(str::to_owned))
                .collect::<Result<Vec<_>, _>>()?
                .join(" ")
        } else {
            join_shell_escaped_paths(&destinations)?
        };
        self.paste(text, cx)?;
        // Delivered: the paths are in the pane, where the user is looking.
        self.dismiss_delivered(&ids, cx);
        Ok(())
    }

    fn start(
        &self,
        path: &str,
        name: &str,
        preflight: bool,
        policy: Policy,
        image_png: bool,
        sink: &Arc<dyn EventSink>,
    ) -> Result<String, String> {
        self.backend.start_upload(
            UploadRequest {
                scope: self.scope.clone(),
                source_path: path.to_owned(),
                destination_name: name.to_owned(),
                // A rename preflight is non-destructive and still says whether
                // the name is taken; the upload uses what the user reviewed.
                collision: if preflight {
                    UploadCollisionPolicy::Rename
                } else {
                    policy.collision
                },
                large_upload_confirmed: preflight || policy.large_confirmed,
                image_png,
                preflight,
            },
            sink.clone(),
        )
    }

    /// Waits until every transfer in `ids` has finished, recording progress as
    /// it comes. One failure cancels the rest, and the batch waits for them to
    /// finish too, so the list shows how each one ended.
    async fn wait(
        &self,
        events: &Receiver<EventBody>,
        ids: &[String],
        cx: &mut AsyncWindowContext,
    ) -> Result<Vec<TransferEvent>, Stop> {
        let mut latest: HashMap<String, TransferEvent> = HashMap::new();
        let mut failure: Option<Stop> = None;
        let mut cancelled = false;
        loop {
            for body in events.try_iter() {
                let EventBody::Json(json) = body else {
                    continue;
                };
                // An event this batch cannot read fails it like any other
                // failure: the rest are stopped and waited for.
                let event = match TransferEvent::parse(&json) {
                    Ok(event) => event,
                    Err(message) => {
                        failure.get_or_insert(Stop::Failed(message));
                        continue;
                    }
                };
                let stale = (!event.belongs_to(&self.scope)).then(|| {
                    "Host returned a terminal transfer event for a stale connection scope."
                        .to_owned()
                });
                if failure.is_none()
                    && let Some(message) = stale.or_else(|| event.failure())
                {
                    failure = Some(Stop::Failed(message));
                }
                let pane_id = self.scope.pane_id.clone();
                let recorded = event.clone();
                cx.update(|_, cx| {
                    cx.default_global::<Uploads>().0.record(&pane_id, recorded);
                })
                .map_err(|_| Stop::Silent)?;
                // A late progress frame never reopens a finished transfer.
                let settled = latest
                    .get(&event.transfer_id)
                    .is_some_and(|known| known.state.finished());
                if !settled {
                    latest.insert(event.transfer_id.clone(), event);
                }
            }
            if failure.is_none() && !self.still_current(cx) {
                failure = Some(Stop::Silent);
            }
            let finished = |id: &String| latest.get(id).is_some_and(|event| event.state.finished());
            if failure.is_some() && !cancelled {
                cancelled = true;
                for id in ids.iter().filter(|id| !finished(id)) {
                    let _ = self.backend.cancel_upload(id.clone());
                }
            }
            if ids.iter().all(finished) {
                return match failure {
                    Some(stop) => Err(stop),
                    None => Ok(ids.iter().map(|id| latest[id].clone()).collect()),
                };
            }
            cx.background_executor().timer(POLL).await;
        }
    }

    fn still_current(&self, cx: &mut AsyncWindowContext) -> bool {
        cx.update(|_, cx| {
            self.pane.upgrade().is_some_and(|pane| {
                (self.scope_of)(pane.read(cx), cx).as_ref() == Some(&self.scope)
            })
        })
        .unwrap_or(false)
    }

    fn paste(&self, text: String, cx: &mut AsyncWindowContext) -> Result<(), Stop> {
        if !self.still_current(cx) {
            return Err(Stop::Silent);
        }
        cx.update(|_, cx| {
            self.pane
                .update(cx, |pane, cx| super::paste_text(pane, text, cx))
                .map_err(|_| Stop::Silent)?
                .map_err(Stop::Failed)
        })
        .map_err(|_| Stop::Silent)?
    }

    fn dismiss_delivered(&self, ids: &[String], cx: &mut AsyncWindowContext) {
        let _ = cx.update(|_, cx| {
            cx.default_global::<Uploads>().0.dismiss_delivered(ids);
        });
    }

    /// Port of `UploadReviewDialog`. Keeping both names is the default, as
    /// there; "stop without overwriting" is left out because a batch stops at
    /// the first failure anyway, which is what Cancel already does.
    async fn ask(
        &self,
        review: &Review,
        items: &[Preflight],
        cx: &mut AsyncWindowContext,
    ) -> Result<Policy, Stop> {
        let choice: Rc<Cell<Option<UploadCollisionPolicy>>> = Rc::default();
        let detail = review_detail(review, items);
        let collisions = review.collisions;
        let picked = choice.clone();
        cx.update(|window, cx| {
            window.open_alert_dialog(cx, move |alert, window, cx| {
                let palette = theme::palette(cx);
                let pick = |id: &'static str, label: &'static str, policy| {
                    let picked = picked.clone();
                    Button::new(id).label(label).on_click(move |_, window, cx| {
                        picked.set(Some(policy));
                        window.close_dialog(cx);
                    })
                };
                let actions = if collisions {
                    vec![
                        pick(
                            "upload-replace",
                            "Replace after verification",
                            UploadCollisionPolicy::OverwriteConfirmed,
                        )
                        .danger(),
                        pick(
                            "upload-keep-both",
                            "Keep both",
                            UploadCollisionPolicy::Rename,
                        )
                        .primary(),
                    ]
                } else {
                    vec![
                        pick("upload-start", "Start upload", UploadCollisionPolicy::Fail).primary(),
                    ]
                };
                alert
                    .entrance(DialogEntrance::None)
                    .text_size(ui(TYPE_ROW))
                    .bg(palette.chrome_raised)
                    .border_color(palette.chrome_border)
                    .rounded(ui(RADIUS_PANEL))
                    .w(ui(560.).to_pixels(window.rem_size()))
                    .title("Review terminal upload")
                    .description(
                        v_flex()
                            .gap(ui(4.))
                            .children(detail.iter().map(|line| div().child(line.clone()))),
                    )
                    .footer(
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap(ui(8.))
                            .child(
                                Button::new("upload-cancel")
                                    .label("Cancel")
                                    .on_click(|_, window, cx| window.close_dialog(cx)),
                            )
                            .children(actions),
                    )
            });
        })
        .map_err(|_| Stop::Silent)?;
        loop {
            cx.background_executor().timer(POLL).await;
            // The batch's connection or pane went: the question is moot.
            if !self.still_current(cx) {
                let _ = cx.update(|window, cx| window.close_dialog(cx));
                return Err(Stop::Silent);
            }
            if let Some(collision) = choice.get() {
                return Ok(Policy {
                    collision,
                    large_confirmed: review.large,
                });
            }
            let open = cx
                .update(|window, cx| window.has_active_dialog(cx))
                .map_err(|_| Stop::Silent)?;
            // Cancelled: the user knows, as in the Tauri app nothing is said.
            if !open {
                return Err(Stop::Silent);
            }
        }
    }
}

/// What the user chose in review, or the defaults when nothing needed asking.
#[derive(Clone, Copy)]
struct Policy {
    collision: UploadCollisionPolicy,
    large_confirmed: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            collision: UploadCollisionPolicy::Fail,
            large_confirmed: false,
        }
    }
}

/// The review dialog's text, one line each.
fn review_detail(review: &Review, items: &[Preflight]) -> Vec<SharedString> {
    let count = items.len();
    let mut lines = vec![SharedString::from(format!(
        "{count} file{} will be staged and pasted only after verification.",
        if count == 1 { "" } else { "s" }
    ))];
    lines.extend(items.iter().map(|item| {
        SharedString::from(format!(
            "{} \u{b7} {}{}",
            item.name,
            format_bytes(item.size_bytes),
            if item.collision {
                " \u{b7} destination exists"
            } else {
                ""
            }
        ))
    }));
    if review.large {
        lines.push(
            "At least one upload is larger than 500 MiB. Starting it explicitly confirms the large transfer."
                .into(),
        );
    }
    if review.collisions {
        lines.push(
            "Replacing overwrites existing files only after byte-count and digest verification."
                .into(),
        );
    }
    lines
}
