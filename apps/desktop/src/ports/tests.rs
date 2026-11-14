//! The panel against a scripted native side. Port of `PortsPanel.test.tsx`.

use std::{cell::RefCell, rc::Rc};

use desktop_core::connection::{
    ConnectionSpec,
    ports::{DetectedPort, ForwardState, PortForward},
};
use futures::{
    channel::oneshot,
    future::{BoxFuture, FutureExt},
};
use gpui_kit::{AppContext, Context, Entity, TestAppContext};

use super::{
    model::{BAD_PORTS, PortsHost},
    panel::{PortsPanel, PortsTransport},
};

type Reply = oneshot::Sender<Result<Vec<DetectedPort>, String>>;

#[derive(Default)]
struct Native {
    forwards: RefCell<Vec<PortForward>>,
    forwarded: RefCell<Vec<(ConnectionSpec, u16, u16)>>,
    stopped: RefCell<Vec<(String, u16)>>,
    stopped_hosts: RefCell<Vec<String>>,
    detections: RefCell<Vec<(ConnectionSpec, Option<Reply>)>>,
    refuse: RefCell<Option<String>>,
}

impl Native {
    fn detected_for(&self) -> Vec<ConnectionSpec> {
        self.detections
            .borrow()
            .iter()
            .map(|(connection, _)| connection.clone())
            .collect()
    }

    /// Answers the detection asked `index`th.
    fn answer(&self, index: usize, answer: Result<Vec<DetectedPort>, String>) {
        let reply = self.detections.borrow_mut()[index].1.take().unwrap();
        reply.send(answer).ok();
    }
}

struct Transport(Rc<Native>);

impl PortsTransport for Transport {
    fn forward(&self, connection: ConnectionSpec, remote: u16, local: u16) -> Result<(), String> {
        if let Some(error) = self.0.refuse.borrow().clone() {
            return Err(error);
        }
        let ConnectionSpec::Ssh { profile_id, .. } = &connection else {
            return Err("port forwarding needs an SSH host".into());
        };
        self.0.forwards.borrow_mut().push(PortForward {
            profile_id: profile_id.clone(),
            remote_port: remote,
            local_port: local,
            state: ForwardState::Starting,
            error: None,
        });
        self.0
            .forwarded
            .borrow_mut()
            .push((connection, remote, local));
        Ok(())
    }

    fn stop(&self, profile_id: &str, remote: u16) {
        self.0
            .stopped
            .borrow_mut()
            .push((profile_id.into(), remote));
        self.0
            .forwards
            .borrow_mut()
            .retain(|forward| !(forward.profile_id == profile_id && forward.remote_port == remote));
    }

    fn stop_host(&self, profile_id: &str) {
        self.0.stopped_hosts.borrow_mut().push(profile_id.into());
        self.0
            .forwards
            .borrow_mut()
            .retain(|forward| forward.profile_id != profile_id);
    }

    fn list(&self) -> Vec<PortForward> {
        self.0.forwards.borrow().clone()
    }

    fn detect(
        &self,
        connection: ConnectionSpec,
    ) -> BoxFuture<'static, Result<Vec<DetectedPort>, String>> {
        let (reply, answer) = oneshot::channel();
        self.0
            .detections
            .borrow_mut()
            .push((connection, Some(reply)));
        async move { answer.await.unwrap_or_else(|_| Err("dropped".into())) }.boxed()
    }

    fn changed(&self) -> bool {
        false
    }
}

fn host(id: &str, label: &str) -> PortsHost {
    PortsHost {
        profile_id: id.into(),
        label: label.into(),
        connection: ConnectionSpec::Ssh {
            profile_id: id.into(),
            target: id.into(),
            config_path: None,
        },
    }
}

fn forward_of(profile: &str, port: u16) -> PortForward {
    PortForward {
        profile_id: profile.into(),
        remote_port: port,
        local_port: port,
        state: ForwardState::Active,
        error: None,
    }
}

fn detected(port: u16, process: Option<&str>) -> DetectedPort {
    DetectedPort {
        port,
        process: process.map(Into::into),
    }
}

struct Harness {
    native: Rc<Native>,
    panel: Entity<PortsPanel>,
}

impl Harness {
    fn new(cx: &mut TestAppContext, hosts: Vec<PortsHost>, active: &str) -> Self {
        let native = Rc::new(Native::default());
        let panel = cx.new(|_| PortsPanel::new(Rc::new(Transport(native.clone()))));
        let harness = Self { native, panel };
        harness.update(cx, |panel, cx| panel.set_hosts(hosts, active.into(), cx));
        harness
    }

    fn update(
        &self,
        cx: &mut TestAppContext,
        update: impl FnOnce(&mut PortsPanel, &mut Context<PortsPanel>),
    ) {
        self.panel.update(cx, update);
        cx.run_until_parked();
    }

    fn read<R>(&self, cx: &mut TestAppContext, read: impl FnOnce(&PortsPanel) -> R) -> R {
        self.panel.read_with(cx, |panel, _| read(panel))
    }

    fn submit(&self, cx: &mut TestAppContext, remote: &str, local: &str) -> bool {
        let (remote, local) = (remote.to_owned(), local.to_owned());
        let mut ok = false;
        self.update(cx, |panel, cx| ok = panel.submit(&remote, &local, cx));
        ok
    }

    fn error(&self, cx: &mut TestAppContext) -> Option<String> {
        self.read(cx, |panel| panel.error.clone())
    }
}

#[gpui_kit::test]
fn forwards_a_typed_port_to_the_same_local_port_unless_told_otherwise(cx: &mut TestAppContext) {
    let devbox = host("devbox", "Devbox");
    let harness = Harness::new(cx, vec![devbox.clone()], "devbox");

    assert!(harness.submit(cx, "3000", ""));
    assert!(harness.submit(cx, "5173", "8080"));
    assert_eq!(
        *harness.native.forwarded.borrow(),
        [
            (devbox.connection.clone(), 3000, 3000),
            (devbox.connection, 5173, 8080)
        ]
    );
    assert_eq!(harness.error(cx), None);

    // A port nobody could forward is said, not sent.
    assert!(!harness.submit(cx, "70000", ""));
    assert_eq!(harness.native.forwarded.borrow().len(), 2);
    assert_eq!(harness.error(cx).as_deref(), Some(BAD_PORTS));
    // The next try clears it.
    assert!(harness.submit(cx, "9000", ""));
    assert_eq!(harness.error(cx), None);
}

#[gpui_kit::test]
fn a_refused_forward_is_said_and_keeps_the_fields(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, vec![host("devbox", "Devbox")], "devbox");
    *harness.native.refuse.borrow_mut() = Some("port 3000 is already forwarded".into());
    assert!(!harness.submit(cx, "3000", ""));
    assert_eq!(
        harness.error(cx).as_deref(),
        Some("port 3000 is already forwarded")
    );
}

#[gpui_kit::test]
fn starts_on_the_active_host_and_suggests_what_it_has_not_forwarded(cx: &mut TestAppContext) {
    let (devbox, gpu) = (host("devbox", "Devbox"), host("gpu", "GPU"));
    let harness = Harness::new(cx, vec![devbox.clone(), gpu.clone()], "gpu");
    harness
        .native
        .forwards
        .borrow_mut()
        .push(forward_of("gpu", 3000));
    harness.update(cx, |panel, cx| panel.refresh(cx));
    assert_eq!(
        harness.native.detected_for(),
        std::slice::from_ref(&gpu.connection)
    );
    harness.native.answer(
        0,
        Ok(vec![detected(3000, Some("node")), detected(8000, None)]),
    );
    cx.run_until_parked();

    // 3000 is already forwarded on this host, so only 8000 is suggested.
    assert_eq!(
        harness.read(cx, |panel| panel.suggestions()),
        [detected(8000, None)]
    );
    harness.update(cx, |panel, cx| {
        panel.forward(8000, 8000, cx);
    });
    assert_eq!(
        harness.native.forwarded.borrow().last().cloned(),
        Some((gpu.connection, 8000, 8000))
    );
    assert!(harness.read(cx, |panel| panel.suggestions()).is_empty());

    harness.update(cx, |panel, cx| panel.choose("devbox".into(), cx));
    assert_eq!(
        harness.native.detected_for().last(),
        Some(&devbox.connection)
    );
    // The other host's detection says nothing about this one.
    assert!(harness.read(cx, |panel| panel.suggestions()).is_empty());
}

#[gpui_kit::test]
fn keeps_typing_and_retrying_open_when_detection_fails(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, vec![host("devbox", "Devbox")], "devbox");
    harness
        .native
        .answer(0, Err("control master not ready".into()));
    cx.run_until_parked();
    assert!(harness.read(cx, |panel| panel.suggestions()).is_empty());
    assert!(harness.submit(cx, "3000", ""));

    harness.update(cx, |panel, cx| panel.detect(cx));
    harness
        .native
        .answer(1, Ok(vec![detected(4000, Some("node"))]));
    cx.run_until_parked();
    assert_eq!(
        harness.read(cx, |panel| panel.suggestions()),
        [detected(4000, Some("node"))]
    );
}

#[gpui_kit::test]
fn only_the_newest_detection_lands(cx: &mut TestAppContext) {
    let harness = Harness::new(
        cx,
        vec![host("devbox", "Devbox"), host("gpu", "GPU")],
        "devbox",
    );
    harness.update(cx, |panel, cx| panel.choose("gpu".into(), cx));
    // Asked for devbox, then gpu; devbox's answer arrives last.
    harness.native.answer(1, Ok(vec![detected(8000, None)]));
    cx.run_until_parked();
    harness.native.answer(0, Ok(vec![detected(3000, None)]));
    cx.run_until_parked();
    assert_eq!(
        harness.read(cx, |panel| panel.suggestions()),
        [detected(8000, None)]
    );
}

#[gpui_kit::test]
fn stopping_ends_one_forward_and_the_list_follows(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, vec![host("devbox", "Devbox")], "devbox");
    assert!(harness.submit(cx, "3000", ""));
    assert!(harness.submit(cx, "4000", ""));
    harness.update(cx, |panel, cx| panel.stop(3000, cx));
    assert_eq!(
        *harness.native.stopped.borrow(),
        [("devbox".to_owned(), 3000)]
    );
    assert_eq!(
        harness
            .read(cx, |panel| panel.host_forwards())
            .iter()
            .map(|forward| forward.remote_port)
            .collect::<Vec<_>>(),
        [4000]
    );
}

#[gpui_kit::test]
fn ends_the_forwards_of_a_host_that_is_no_longer_shown(cx: &mut TestAppContext) {
    let (devbox, gpu) = (host("devbox", "Devbox"), host("gpu", "GPU"));
    let harness = Harness::new(cx, vec![devbox.clone(), gpu.clone()], "devbox");
    harness
        .native
        .forwards
        .borrow_mut()
        .extend([forward_of("devbox", 3000), forward_of("gpu", 8000)]);
    harness.update(cx, |panel, cx| panel.refresh(cx));
    assert!(harness.native.stopped_hosts.borrow().is_empty());

    harness.update(cx, |panel, cx| {
        panel.set_hosts(vec![devbox], "devbox".into(), cx)
    });
    assert_eq!(*harness.native.stopped_hosts.borrow(), ["gpu"]);
}

#[gpui_kit::test]
fn needs_an_ssh_host(cx: &mut TestAppContext) {
    let harness = Harness::new(cx, Vec::new(), "local");
    assert!(harness.read(cx, |panel| panel.host().is_none()));
    assert!(harness.native.detected_for().is_empty());
    assert!(!harness.submit(cx, "3000", ""));
}
