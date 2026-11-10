//! A terminal upload batch through GPUI's test platform: what a remote host
//! pastes after its preflight and upload, a staged screenshot pasted raw, a
//! failure that pastes nothing, and a batch abandoned when the pane's
//! connection changes under it. The fake backend answers on the test thread
//! and the batch polls on a timer, so nothing wakes a GPUI task from outside.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc::Sender};
use std::time::Duration;

use desktop_core::app_state::ThemePreference;
use desktop_core::platform::{EventBody, EventSink};
use futures::FutureExt;
use futures::future::BoxFuture;
use gpui_kit::{
    AnyWindowHandle, App, AppContext, Entity, TestAppContext, Window, WindowOptions, font, px,
};
use muxflow_gpui::{
    connection_ui::backend::{EngineChannel, TerminalBackend},
    terminal::{
        TerminalLink, TerminalPane,
        cell_metrics::CellMetrics,
        integration::{
            colors,
            transfers::{TransferScope, UploadCollisionPolicy, UploadRequest},
            upload::{self, ScopeOf, Source, Uploads},
        },
    },
    theme,
};
use serde_json::{Value, json};
use terminal_engine::{EngineOutput, HubLimits};

/// What the host does with an upload once it is started.
#[derive(Clone, Copy, Default, PartialEq)]
enum Outcome {
    #[default]
    Complete,
    Fail,
    /// Stays running until the test says otherwise.
    Hold,
}

struct Started {
    id: String,
    preflight: bool,
    collision: UploadCollisionPolicy,
    image_png: bool,
    name: String,
    sink: Arc<dyn EventSink>,
}

#[derive(Default)]
struct Calls {
    input: Vec<Vec<u8>>,
    started: Vec<Started>,
    cancelled: Vec<String>,
    outcome: Outcome,
    colors: Vec<(String, String)>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Calls>>);

fn event(id: &str, state: &str, extra: Value) -> EventBody {
    let mut value = json!({
        "transferId": id,
        "sourcePath": "/local/source",
        "state": state,
        "serverIdentity": "socket:remote",
        "connectionEpoch": "4",
    });
    value
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    EventBody::Json(value.to_string())
}

impl TerminalBackend for Fake {
    fn spawn_engine(&self, outputs: Sender<EngineOutput>) -> Box<dyn EngineChannel> {
        Box::new(terminal_engine::spawn(HubLimits::default(), outputs))
    }

    fn request_seed(&self, _: String, _: String) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn request_history(
        &self,
        _: String,
        _: String,
        _: u32,
        _: u32,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn set_pane_visibility(
        &self,
        _: String,
        _: String,
        _: bool,
        _: bool,
        _: u64,
        _: u64,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn resize_client(
        &self,
        _: String,
        _: u16,
        _: u16,
        _: String,
        _: String,
    ) -> BoxFuture<'static, Result<(), String>> {
        async { Ok(()) }.boxed()
    }

    fn send_input(&self, _: String, _: String, bytes: Vec<u8>) -> Result<(), String> {
        self.0.lock().unwrap().input.push(bytes);
        Ok(())
    }

    fn record_incident(&self, _: &str, _: Value) {}

    fn set_terminal_colors(&self, foreground: String, background: String) -> Result<(), String> {
        self.0.lock().unwrap().colors.push((foreground, background));
        Ok(())
    }

    fn tmux_action(&self, _: String, _: Value) -> BoxFuture<'static, Result<Value, String>> {
        async { Ok(Value::Null) }.boxed()
    }

    /// Answers on the caller's thread: a preflight completes at once, and an
    /// upload runs, then ends as the test asked.
    fn start_upload(
        &self,
        request: UploadRequest,
        events: Arc<dyn EventSink>,
    ) -> Result<String, String> {
        let mut calls = self.0.lock().unwrap();
        let id = format!("t{}", calls.started.len());
        let name = request.destination_name.clone();
        if request.preflight {
            let size = json!({"sizeBytes": "12", "sourceKind": "regularFile", "readable": true});
            events.send(event(&id, "completed", size))?;
        } else {
            events.send(event(&id, "running", json!({"transferredBytes": "1"})))?;
            match calls.outcome {
                Outcome::Complete => events.send(event(
                    &id,
                    "completed",
                    json!({
                        "destination": format!("/remote/staging/{name}"),
                        "blake3": "af1349b9",
                        "outcome": "published",
                        "cleanupError": "",
                    }),
                ))?,
                Outcome::Fail => {
                    events.send(event(&id, "failed", json!({"error": "disk full"})))?
                }
                Outcome::Hold => {}
            }
        }
        calls.started.push(Started {
            id: id.clone(),
            preflight: request.preflight,
            collision: request.collision,
            image_png: request.image_png,
            name,
            sink: events,
        });
        Ok(id)
    }

    fn cancel_upload(&self, transfer_id: String) -> Result<Value, String> {
        let mut calls = self.0.lock().unwrap();
        calls.cancelled.push(transfer_id.clone());
        if let Some(started) = calls
            .started
            .iter()
            .find(|started| started.id == transfer_id)
        {
            started
                .sink
                .send(event(&transfer_id, "cancelled", json!({})))?;
        }
        Ok(json!({"disposition": "cancelled"}))
    }
}

fn remote_scope() -> TransferScope {
    TransferScope {
        client_id: "client-1".into(),
        profile_id: "remote".into(),
        server_identity: "socket:remote".into(),
        terminal_epoch: 4,
        pane_id: "%1".into(),
        local: false,
    }
}

/// The scope a batch sees, changeable mid-batch.
fn scope_cell() -> (Rc<RefCell<Option<TransferScope>>>, ScopeOf) {
    let cell = Rc::new(RefCell::new(Some(remote_scope())));
    let read = cell.clone();
    (cell, Rc::new(move |_, _| read.borrow().clone()))
}

fn metrics() -> Rc<CellMetrics> {
    Rc::new(CellMetrics {
        font: font("JetBrains Mono"),
        font_size: px(13.),
        cell_width: px(8.),
        cell_height: px(17.),
        line_width: px(1.),
        scrollbar_width: px(14.),
    })
}

fn open_pane(fake: &Fake, cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<TerminalPane>) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::apply(
            ThemePreference::Dark,
            1.0,
            gpui_kit::WindowAppearance::Dark,
            cx,
        );
        let backend: Arc<dyn TerminalBackend> = Arc::new(fake.clone());
        let link = cx.new(|cx| TerminalLink::new("remote".into(), backend, cx));
        link.update(cx, |link, _| link.set_client(Some("client-1".into()), 4));
        gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
            let probes = link.read(cx).probes();
            cx.new(|cx| TerminalPane::new("%1".into(), link, metrics(), probes, window, cx))
        })
        .expect("open a test window")
    })
}

fn with_window<R>(
    handle: AnyWindowHandle,
    cx: &mut TestAppContext,
    action: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    cx.update_window(handle, |_, window, cx| action(window, cx))
        .unwrap()
}

/// Lets the batch's timer run until nothing is left to do.
fn settle(cx: &mut TestAppContext) {
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

fn pasted(fake: &Fake) -> Vec<String> {
    let calls = fake.0.lock().unwrap();
    calls
        .input
        .iter()
        .map(|bytes| String::from_utf8(bytes.clone()).unwrap())
        .collect()
}

fn busy(pane: &Entity<TerminalPane>, cx: &mut TestAppContext) -> bool {
    pane.read_with(cx, |pane, _| pane.integration.busy)
}

#[gpui_kit::test]
fn a_remote_file_is_checked_uploaded_and_its_verified_path_pasted(cx: &mut TestAppContext) {
    let fake = Fake::default();
    let (handle, pane) = open_pane(&fake, cx);
    let (_, scope_of) = scope_cell();
    with_window(handle, cx, |window, cx| {
        let source = Source::Paths(vec!["/local/my report.pdf".into()]);
        upload::accept(&pane, source, scope_of, window, cx);
    });
    assert!(busy(&pane, cx), "one batch at a time");
    settle(cx);
    assert_eq!(pasted(&fake), ["'/remote/staging/my report.pdf'"]);
    {
        let calls = fake.0.lock().unwrap();
        // A rename preflight only asks; the upload uses the reviewed policy.
        assert_eq!(calls.started.len(), 2);
        assert!(calls.started[0].preflight);
        assert!(matches!(
            calls.started[0].collision,
            UploadCollisionPolicy::Rename
        ));
        assert!(!calls.started[1].preflight);
        assert!(matches!(
            calls.started[1].collision,
            UploadCollisionPolicy::Fail
        ));
        assert_eq!(calls.started[1].name, "my report.pdf");
    }
    assert!(!busy(&pane, cx));
    let records = cx.update(|cx| cx.default_global::<Uploads>().0.records.len());
    assert_eq!(records, 0, "a delivered upload leaves the list");
}

#[gpui_kit::test]
fn a_screenshot_on_a_remote_host_is_pasted_as_its_bare_staged_path(cx: &mut TestAppContext) {
    let fake = Fake::default();
    let (handle, pane) = open_pane(&fake, cx);
    let (_, scope_of) = scope_cell();
    let name = "clipboard-0f8fad5b-d9cb-469f-a165-70867728950e.png";
    with_window(handle, cx, |window, cx| {
        let source = Source::StagedImage {
            path: format!("/local/staging/{name}"),
            name: name.into(),
        };
        upload::accept(&pane, source, scope_of, window, cx);
    });
    settle(cx);
    // Raw, not quoted: agents recognize an attachment only by its bare path.
    assert_eq!(pasted(&fake), [format!("/remote/staging/{name}")]);
    let calls = fake.0.lock().unwrap();
    assert!(calls.started.iter().all(|started| started.image_png));
}

#[gpui_kit::test]
fn a_failed_upload_pastes_nothing_and_stays_on_the_list(cx: &mut TestAppContext) {
    let fake = Fake::default();
    fake.0.lock().unwrap().outcome = Outcome::Fail;
    let (handle, pane) = open_pane(&fake, cx);
    let (_, scope_of) = scope_cell();
    with_window(handle, cx, |window, cx| {
        upload::accept(
            &pane,
            Source::Paths(vec!["/local/a".into()]),
            scope_of,
            window,
            cx,
        );
    });
    settle(cx);
    assert!(pasted(&fake).is_empty());
    assert!(!busy(&pane, cx));
    let failed = cx.update(|cx| {
        cx.default_global::<Uploads>()
            .0
            .records
            .iter()
            .any(|record| record.progress.error.as_deref() == Some("disk full"))
    });
    assert!(failed);
}

#[gpui_kit::test]
fn a_batch_whose_connection_changes_cancels_and_pastes_nothing(cx: &mut TestAppContext) {
    let fake = Fake::default();
    fake.0.lock().unwrap().outcome = Outcome::Hold;
    let (handle, pane) = open_pane(&fake, cx);
    let (scope, scope_of) = scope_cell();
    with_window(handle, cx, |window, cx| {
        upload::accept(
            &pane,
            Source::Paths(vec!["/local/a".into()]),
            scope_of,
            window,
            cx,
        );
    });
    settle(cx);
    assert!(busy(&pane, cx), "the upload is still running");
    // The host reconnected: a new epoch is another connection.
    scope.borrow_mut().as_mut().unwrap().terminal_epoch = 5;
    settle(cx);
    let cancelled = fake.0.lock().unwrap().cancelled.clone();
    assert_eq!(cancelled, ["t1"]);
    assert!(pasted(&fake).is_empty());
    assert!(!busy(&pane, cx));
}

#[gpui_kit::test]
fn a_paste_needs_a_live_connection(cx: &mut TestAppContext) {
    let fake = Fake::default();
    let (handle, pane) = open_pane(&fake, cx);
    let (scope, scope_of) = scope_cell();
    *scope.borrow_mut() = None;
    with_window(handle, cx, |window, cx| {
        upload::accept(
            &pane,
            Source::Paths(vec!["/local/a".into()]),
            scope_of,
            window,
            cx,
        );
    });
    settle(cx);
    assert!(!busy(&pane, cx));
    assert!(fake.0.lock().unwrap().started.is_empty());
}

#[gpui_kit::test]
fn a_local_file_is_pasted_where_it_is_once_it_reads_as_a_file(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("it's here.txt");
    std::fs::write(&file, b"x").unwrap();
    let file = file.to_str().unwrap().to_owned();
    let fake = Fake::default();
    let (handle, pane) = open_pane(&fake, cx);
    let (scope, scope_of) = scope_cell();
    scope.borrow_mut().as_mut().unwrap().local = true;
    with_window(handle, cx, |window, cx| {
        let paths = vec![file.clone(), dir.path().to_str().unwrap().to_owned()];
        // A directory is refused, and with it the whole paste.
        upload::accept(&pane, Source::Paths(paths), scope_of.clone(), window, cx);
    });
    settle(cx);
    assert!(pasted(&fake).is_empty());
    with_window(handle, cx, |window, cx| {
        upload::accept(
            &pane,
            Source::Paths(vec![file.clone()]),
            scope_of,
            window,
            cx,
        );
    });
    settle(cx);
    let escaped = format!("'{}'", file.replace('\'', r#"'"'"'"#));
    assert_eq!(pasted(&fake), [escaped]);
    assert!(fake.0.lock().unwrap().started.is_empty(), "nothing uploads");
}

#[gpui_kit::test]
fn the_host_hears_the_terminal_colors_at_start_and_on_every_theme_change(cx: &mut TestAppContext) {
    let fake = Fake::default();
    let _ = open_pane(&fake, cx);
    let backend: Arc<dyn TerminalBackend> = Arc::new(fake.clone());
    cx.update(|cx| colors::start(backend, cx));
    let dark = cx.update(|cx| {
        let palette = theme::palette(cx);
        (colors::hex(palette.term_fg), colors::hex(palette.term_bg))
    });
    cx.update(|cx| {
        theme::apply(
            ThemePreference::Light,
            1.0,
            gpui_kit::WindowAppearance::Light,
            cx,
        );
    });
    cx.run_until_parked();
    let light = cx.update(|cx| {
        let palette = theme::palette(cx);
        (colors::hex(palette.term_fg), colors::hex(palette.term_bg))
    });
    assert_ne!(dark, light);
    assert_eq!(fake.0.lock().unwrap().colors, [dark, light]);
}
