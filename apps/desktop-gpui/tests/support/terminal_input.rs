//! Shared terminal input and frame fixtures.

use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc::Sender};

use futures::FutureExt;
use futures::future::BoxFuture;
use gpui_kit::{font, px};
use muxflow_gpui::{
    connection_ui::backend::{EngineChannel, TerminalBackend},
    terminal::cell_metrics::CellMetrics,
};
use serde_json::Value;
use terminal_engine::{EngineOutput, HubLimits};

/// One send, as `(pane, bytes, pasted)`.
pub type Sent = (String, Vec<u8>, bool);

#[derive(Clone, Default)]
pub struct Fake(Arc<Mutex<Vec<Sent>>>);

impl Fake {
    pub fn take(&self) -> Vec<Sent> {
        std::mem::take(&mut self.0.lock().unwrap())
    }
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

    fn send_input(&self, _: String, pane_id: String, bytes: Vec<u8>) -> Result<(), String> {
        self.0.lock().unwrap().push((pane_id, bytes, false));
        Ok(())
    }

    fn send_paste(&self, _: String, pane_id: String, text: Vec<u8>) -> Result<(), String> {
        self.0.lock().unwrap().push((pane_id, text, true));
        Ok(())
    }

    fn record_incident(&self, _: &str, _: Value) {}

    fn set_terminal_colors(&self, _: String, _: String) -> Result<(), String> {
        Ok(())
    }

    fn tmux_action(&self, _: String, _: Value) -> BoxFuture<'static, Result<Value, String>> {
        async { Ok(Value::Null) }.boxed()
    }

    fn start_upload(
        &self,
        _: muxflow_gpui::terminal::integration::transfers::UploadRequest,
        _: Arc<dyn desktop_core::platform::EventSink>,
    ) -> Result<String, String> {
        Err("no uploads here".into())
    }

    fn cancel_upload(&self, _: String) -> Result<Value, String> {
        Ok(Value::Null)
    }
}

pub fn frame(kind: u8, label: &str, sequence: u64, payload: &[u8]) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend_from_slice(&(label.len() as u16).to_be_bytes());
    bytes.extend_from_slice(label.as_bytes());
    bytes.extend_from_slice(&sequence.to_be_bytes());
    bytes.extend_from_slice(payload);
    bytes
}

/// A seed (kind 1) or an output (kind 2) for `pane`.
pub fn screen_frame(kind: u8, pane: &str, generation: u64, text: &[u8]) -> Vec<u8> {
    let mut payload = generation.to_be_bytes().to_vec();
    payload.extend_from_slice(text);
    frame(kind, pane, generation, &payload)
}

pub fn metrics() -> Rc<CellMetrics> {
    Rc::new(CellMetrics {
        font: font(muxflow_gpui::fonts::MONO),
        font_size: px(13.),
        cell_width: px(8.),
        cell_height: px(17.),
        line_width: px(1.),
        scrollbar_width: px(14.),
    })
}
