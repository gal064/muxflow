use std::{
    io::Write,
    process::{ChildStdin, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc as std_mpsc,
    },
};

use anyhow::{Context, bail};
use tmux_control::PaneResourceStore;
use tokio::sync::mpsc;

use super::capabilities::tmux_command_table;
use super::startup::ProcessStartup;
use super::{
    ControlStreamReader, ControlWrite, FlowControl, OutputCredit, SequencerControl,
    TerminalAttachment, queue_capture, read_control_stream, validate_tmux_id,
    write_capture_request_resuming, write_terminal_color_reports,
};
use crate::service::snapshot::tmux_command;
use crate::service::topology_output_trigger::TopologyOutputTrigger;

pub(super) struct AttachmentRuntime {
    pub(super) event_tx: mpsc::Sender<SequencerControl>,
    pub(super) overflowed: Arc<AtomicBool>,
    pub(super) resources: Arc<Mutex<PaneResourceStore>>,
    pub(super) terminal_generation: Arc<AtomicU64>,
    pub(super) output_credit: Arc<OutputCredit>,
    pub(super) emission_order: Arc<Mutex<()>>,
    pub(super) topology_trigger: TopologyOutputTrigger,
}

impl TerminalAttachment {
    pub(super) fn start(
        session_id: &str,
        pane_ids: &[String],
        runtime: AttachmentRuntime,
    ) -> anyhow::Result<Self> {
        let reports_terminal_colors = control_color_reports_supported()?;
        Self::start_with_command(
            session_id,
            pane_ids,
            runtime,
            tmux_command(),
            reports_terminal_colors,
        )
    }

    pub(super) fn start_with_command(
        session_id: &str,
        pane_ids: &[String],
        runtime: AttachmentRuntime,
        mut command: Command,
        reports_terminal_colors: bool,
    ) -> anyhow::Result<Self> {
        let AttachmentRuntime {
            event_tx,
            overflowed,
            resources,
            terminal_generation,
            output_credit,
            emission_order,
            topology_trigger,
        } = runtime;
        validate_tmux_id(session_id, '$')?;
        if pane_ids.is_empty() {
            bail!("cannot attach without panes");
        }
        for pane_id in pane_ids {
            validate_tmux_id(pane_id, '%')?;
        }

        let child = command
            .args([
                // `-CC` asks tmux to disable terminal echo and performs a
                // tcgetattr probe on current tmux releases; a daemon pipe has
                // no controlling TTY. `-C` is the byte-identical control
                // protocol mode that works over local and SSH stdio.
                "-C",
                "attach-session",
                "-f",
                "pause-after=5,ignore-size",
                "-t",
                session_id,
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start tmux control client")?;
        let stopped = Arc::new(AtomicBool::new(false));
        let mut startup = ProcessStartup::new(child, Arc::clone(&stopped));
        let child = startup.child();
        let stdout = child
            .lock()
            .unwrap()
            .stdout
            .take()
            .context("tmux control stdout unavailable")?;
        let stdin = child
            .lock()
            .unwrap()
            .stdin
            .take()
            .context("tmux control stdin unavailable")?;
        let stdin = Arc::new(Mutex::new(stdin));
        {
            let mut writer = stdin.lock().unwrap();
            for pane_id in pane_ids {
                if reports_terminal_colors {
                    write_terminal_color_reports(&mut *writer, pane_id)?;
                }
                queue_capture(&mut *writer, pane_id)?;
            }
            writer.flush()?;
        }

        let (stream_tx, stream_rx) = std_mpsc::channel();
        let reader_stopped = Arc::clone(&stopped);
        let reader_stop_signal = Arc::clone(&stopped);
        let reader_panes = pane_ids.to_vec();
        let flow = Arc::new(FlowControl::default());
        let reader_flow = Arc::clone(&flow);
        let reader_output_credit = Arc::clone(&output_credit);
        let reader_emission_order = Arc::clone(&emission_order);
        let reader_writer = spawn_control_writer(session_id, Arc::clone(&stdin), &mut startup)?;
        startup.spawn(
            2,
            std::thread::Builder::new().name(format!("host-tmux-control-{session_id}")),
            move || {
                read_control_stream(ControlStreamReader {
                    stdout,
                    writer: reader_writer,
                    pane_ids: reader_panes,
                    event_tx,
                    overflowed,
                    resources,
                    terminal_generation,
                    stopped: reader_stop_signal,
                    controls: stream_rx,
                    flow: reader_flow,
                    output_credit: reader_output_credit,
                    emission_order: reader_emission_order,
                    topology_trigger,
                });
                reader_stopped.store(true, Ordering::Release);
            },
        )?;
        let (child, workers) = startup.commit();
        Ok(Self {
            pane_ids: pane_ids.iter().cloned().collect(),
            stdin,
            child,
            stopped,
            stream_tx,
            flow,
            output_credit,
            workers,
            last_size: None,
            reports_terminal_colors,
        })
    }
}

fn control_color_reports_supported() -> anyhow::Result<bool> {
    Ok(tmux_supports_control_color_reports(tmux_command_table()?))
}

pub(super) fn tmux_supports_control_color_reports(output: &[u8]) -> bool {
    String::from_utf8_lossy(output)
        .lines()
        .any(|line| line.starts_with("refresh-client ") && line.contains("[-r pane:report]"))
}

/// Serialises reader-requested writes onto a thread that is allowed to block.
///
/// This lane is unbounded because each producer is independently rate-limited:
/// a paused pane produces at most one resume per flow-control episode, while a
/// discarded seed or parse error can request only one replacement snapshot.
fn spawn_control_writer(
    session_id: &str,
    stdin: Arc<Mutex<ChildStdin>>,
    startup: &mut ProcessStartup,
) -> anyhow::Result<std_mpsc::Sender<ControlWrite>> {
    let (sender, receiver) = std_mpsc::channel::<ControlWrite>();
    startup.spawn(
        1,
        std::thread::Builder::new().name(format!("host-tmux-writer-{session_id}")),
        move || {
            while let Ok(write) = receiver.recv() {
                if let Some(delay) = write.delay {
                    std::thread::sleep(delay);
                }
                let _ = write_capture_request_resuming(&stdin, &write.pane_id, write.resume_first);
            }
        },
    )?;
    Ok(sender)
}
