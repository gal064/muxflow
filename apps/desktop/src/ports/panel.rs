//! The Ports panel's state: the host it is on, the forwards the native side
//! owns, and the newest detection. Port of the logic of `PortsPanel.tsx` and
//! `usePortForwards.ts`; the view is in [`super::view`].
//!
//! Type a port, and the host's port of that number answers on this machine's.
//! Forwards are not remembered across launches; detection makes re-forwarding
//! one click.

use std::{
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use desktop_core::{
    connection::{
        ConnectionSpec,
        ports::{
            DetectedPort, PortForward, PortForwards, ports_detect, ports_forward, ports_list,
            ports_stop, ports_stop_host,
        },
    },
    platform::{EventBody, EventSink},
};
use futures::future::BoxFuture;
use gpui_kit::{Context, Entity, Subscription, Task};

use super::model::{Detection, PortsHost, chosen_host, forward_ports, local_url, orphaned_hosts};

/// How the panel reaches the native side's forwards.
pub trait PortsTransport {
    fn forward(&self, connection: ConnectionSpec, remote: u16, local: u16) -> Result<(), String>;
    fn stop(&self, profile_id: &str, remote: u16);
    fn stop_host(&self, profile_id: &str);
    fn list(&self) -> Vec<PortForward>;
    fn detect(
        &self,
        connection: ConnectionSpec,
    ) -> BoxFuture<'static, Result<Vec<DetectedPort>, String>>;
    /// Whether the forwards changed since this was last asked. Polled on a
    /// timer: the native threads that change them are not GPUI's.
    fn changed(&self) -> bool;
}

/// The native side's change event, as a flag the panel polls.
#[derive(Default)]
struct Changed(AtomicBool);

impl EventSink for Changed {
    fn send(&self, _: EventBody) -> Result<(), String> {
        self.0.store(true, Ordering::Release);
        Ok(())
    }
}

/// The forwards `desktop-core` owns, with the flag its changes raise.
#[derive(Default)]
pub struct NativePorts {
    forwards: Arc<PortForwards>,
    changed: Arc<Changed>,
}

impl NativePorts {
    /// Ends every forward. The guard pipe would do this when the process
    /// exits; doing it at quit keeps a graceful exit from depending on that.
    pub fn stop_all(&self) {
        self.forwards.stop_all();
    }
}

impl PortsTransport for NativePorts {
    fn forward(&self, connection: ConnectionSpec, remote: u16, local: u16) -> Result<(), String> {
        ports_forward(
            self.changed.clone(),
            &self.forwards,
            connection,
            remote,
            local,
        )
    }

    fn stop(&self, profile_id: &str, remote: u16) {
        ports_stop(
            self.changed.clone(),
            &self.forwards,
            profile_id.to_owned(),
            remote,
        );
    }

    fn stop_host(&self, profile_id: &str) {
        ports_stop_host(self.changed.clone(), &self.forwards, profile_id.to_owned());
    }

    fn list(&self) -> Vec<PortForward> {
        ports_list(&self.forwards)
    }

    fn detect(
        &self,
        connection: ConnectionSpec,
    ) -> BoxFuture<'static, Result<Vec<DetectedPort>, String>> {
        Box::pin(ports_detect(connection))
    }

    fn changed(&self) -> bool {
        self.changed.0.swap(false, Ordering::AcqRel)
    }
}

/// How often the native side is asked whether a forward changed.
const POLL: Duration = Duration::from_millis(200);
/// The same while the window is hidden: the list only feeds the panel, and an
/// orphaned forward can wait a second to be stopped.
const POLL_HIDDEN: Duration = Duration::from_secs(1);

pub struct PortsPanel {
    transport: Rc<dyn PortsTransport>,
    hosts: Vec<PortsHost>,
    active_profile: String,
    chosen: Option<String>,
    forwards: Vec<PortForward>,
    pub error: Option<String>,
    detection: Option<Detection>,
    /// Only the newest detection may land: an older one, from a host switched
    /// away from, would be filtered out anyway, but a refresh must not be
    /// overwritten by the request it replaced.
    detect_epoch: u64,
    _detect: Option<Task<()>>,
    /// The poll, and its check when the window is shown again.
    _poll: Option<(Task<()>, Subscription)>,
    /// The inputs, made with the first frame: they need a window.
    pub(super) fields: Option<super::view::Fields>,
}

impl PortsPanel {
    pub fn new(transport: Rc<dyn PortsTransport>) -> Self {
        let forwards = transport.list();
        Self {
            transport,
            hosts: Vec::new(),
            active_profile: String::new(),
            chosen: None,
            forwards,
            error: None,
            detection: None,
            detect_epoch: 0,
            _detect: None,
            _poll: None,
            fields: None,
        }
    }

    /// Follows the native side's change flag for as long as the panel lives.
    pub fn start_polling(&mut self, cx: &mut Context<Self>) {
        let poll = cx.spawn(async move |this, cx| {
            let mut wait = POLL;
            loop {
                cx.background_executor().timer(wait).await;
                let next = this.update(cx, |this, cx| {
                    this.check(cx);
                    if crate::ui::visibility::shown(cx) {
                        POLL
                    } else {
                        POLL_HIDDEN
                    }
                });
                let Ok(next) = next else {
                    break;
                };
                wait = next;
            }
        });
        let shown = crate::ui::visibility::on_shown(cx, Self::check);
        self._poll = Some((poll, shown));
    }

    fn check(&mut self, cx: &mut Context<Self>) {
        if self.transport.changed() {
            self.refresh(cx);
        }
    }

    pub fn hosts(&self) -> &[PortsHost] {
        &self.hosts
    }

    pub fn active_profile(&self) -> &str {
        &self.active_profile
    }

    pub fn host(&self) -> Option<&PortsHost> {
        chosen_host(&self.hosts, self.chosen.as_deref(), &self.active_profile)
    }

    /// The forwards of the host the panel is on.
    pub fn host_forwards(&self) -> Vec<PortForward> {
        let Some(host) = self.host() else {
            return Vec::new();
        };
        self.forwards
            .iter()
            .filter(|forward| forward.profile_id == host.profile_id)
            .cloned()
            .collect()
    }

    pub fn suggestions(&self) -> Vec<DetectedPort> {
        self.host().map_or_else(Vec::new, |host| {
            super::model::suggestions(&self.forwards, &host.profile_id, self.detection.as_ref())
        })
    }

    /// The shown SSH hosts, and which one is active. Detection runs again when
    /// that moves the panel to another host.
    pub fn set_hosts(&mut self, hosts: Vec<PortsHost>, active: String, cx: &mut Context<Self>) {
        let before = self.host().map(|host| host.profile_id.clone());
        self.hosts = hosts;
        self.active_profile = active;
        self.stop_orphans();
        if self.host().map(|host| host.profile_id.clone()) != before {
            self.detect(cx);
        }
        cx.notify();
    }

    pub fn choose(&mut self, profile_id: String, cx: &mut Context<Self>) {
        let before = self.host().map(|host| host.profile_id.clone());
        self.chosen = Some(profile_id);
        if self.host().map(|host| host.profile_id.clone()) != before {
            self.detect(cx);
        }
        cx.notify();
    }

    /// Every forward the native side owns, as it says now.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.forwards = self.transport.list();
        self.stop_orphans();
        cx.notify();
    }

    /// A host that is no longer shown keeps no forwards, whichever tab is
    /// open: nothing on screen could stop them.
    fn stop_orphans(&mut self) {
        let shown: Vec<String> = self
            .hosts
            .iter()
            .map(|host| host.profile_id.clone())
            .collect();
        for profile_id in orphaned_hosts(&self.forwards, &shown, true) {
            self.transport.stop_host(&profile_id);
        }
    }

    /// Asks the host what is listening. A host without `ss`, or a failed
    /// attempt, gets no suggestions; asking again retries, and typing a port
    /// always works.
    pub fn detect(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.host().cloned() else {
            self._detect = None;
            return;
        };
        self.detect_epoch += 1;
        let epoch = self.detect_epoch;
        let answer = self.transport.detect(host.connection.clone());
        self._detect = Some(cx.spawn(async move |this, cx| {
            let ports = answer.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                if this.detect_epoch == epoch {
                    this.detection = Some(Detection {
                        profile_id: host.profile_id,
                        ports,
                    });
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Forwards the typed ports. True when the fields can be cleared.
    pub fn submit(&mut self, remote: &str, local: &str, cx: &mut Context<Self>) -> bool {
        match forward_ports(remote, local) {
            Ok((remote, local)) => self.forward(remote, local, cx),
            Err(message) => {
                self.error = Some(message.to_owned());
                cx.notify();
                false
            }
        }
    }

    pub fn forward(&mut self, remote: u16, local: u16, cx: &mut Context<Self>) -> bool {
        self.error = None;
        let Some(host) = self.host().cloned() else {
            return false;
        };
        let result = self.transport.forward(host.connection, remote, local);
        // The native side answers before its change event is polled: show the
        // starting forward now.
        self.forwards = self.transport.list();
        let ok = match result {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(error);
                false
            }
        };
        cx.notify();
        ok
    }

    pub fn stop(&mut self, remote: u16, cx: &mut Context<Self>) {
        if let Some(host) = self.host() {
            let profile_id = host.profile_id.clone();
            self.transport.stop(&profile_id, remote);
            self.forwards = self.transport.list();
            cx.notify();
        }
    }

    /// Hands a forward's address to the machine's browser.
    pub fn open(&mut self, local_port: u16, cx: &mut Context<Self>) {
        if let Err(error) =
            desktop_core::external_links::open_external_link(local_url(local_port), true)
        {
            self.error = Some(error);
            cx.notify();
        }
    }
}

/// The panel, once the app has one.
pub struct Ports(pub Entity<PortsPanel>);

impl gpui_kit::Global for Ports {}
