//! The panel and its editor against a scripted helper whose answers the test
//! releases in the order it chooses. Port of `AutomationsPanel.test.tsx`.

use std::{cell::RefCell, collections::HashSet, rc::Rc, time::Duration};

use chrono::NaiveDate;
use desktop_core::connection::automation::AutomationCommand;
use futures::{
    channel::oneshot,
    future::{BoxFuture, FutureExt},
};
use gpui_kit::{AppContext, Context, Entity, TestAppContext};
use serde_json::{Value, json};

use super::{
    model::AutomationRun,
    panel::{AutomationTransport, AutomationsPanel, PanelEvent, PanelHost},
};

type Reply = oneshot::Sender<Result<Value, String>>;

/// A helper that holds every request until the test answers it.
#[derive(Default)]
struct Helper {
    waiting: RefCell<Vec<Reply>>,
    seen: RefCell<Vec<AutomationCommand>>,
}

impl Helper {
    fn actions(&self) -> Vec<String> {
        self.seen
            .borrow()
            .iter()
            .map(|command| command.action.clone())
            .collect()
    }

    fn last(&self) -> AutomationCommand {
        self.seen.borrow().last().cloned().unwrap()
    }

    /// Answers the oldest outstanding request.
    fn answer(&self, cx: &mut TestAppContext, answer: Result<Value, String>) {
        let reply = self.waiting.borrow_mut().remove(0);
        reply.send(answer).ok();
        cx.run_until_parked();
    }

    /// Answers the newest outstanding request first.
    fn answer_newest(&self, cx: &mut TestAppContext, answer: Result<Value, String>) {
        let reply = self.waiting.borrow_mut().pop().unwrap();
        reply.send(answer).ok();
        cx.run_until_parked();
    }
}

struct Transport(Rc<Helper>);

impl AutomationTransport for Transport {
    fn request(
        &self,
        _: String,
        command: AutomationCommand,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let (reply, answer) = oneshot::channel();
        self.0.seen.borrow_mut().push(command);
        self.0.waiting.borrow_mut().push(reply);
        async move { answer.await.unwrap_or_else(|_| Err("dropped".into())) }.boxed()
    }
}

fn definition(id: &str, enabled: bool) -> Value {
    json!({ "id": id, "name": "Fix CI", "enabled": enabled, "schedule": "0 9 * * 1-5", "cwd": "~/dev",
        "agent": "codex", "command": "", "prompt": "go", "keepTabs": 5, "maxRuntimeMinutes": 120 })
}

fn listing(generation: &str, automations: Vec<Value>) -> Value {
    json!({ "automations": automations, "fileError": "", "generation": generation, "stateDir": "/state" })
}

fn summary(id: &str, enabled: bool) -> Value {
    json!({ "definition": definition(id, enabled), "definitionError": "", "nextRun": "2026-10-14T09:00",
        "recentRuns": [], "commandLine": "codex --approve-for-me" })
}

fn host(profile: &str, client: Option<&str>) -> PanelHost {
    PanelHost {
        profile_id: profile.into(),
        label: format!("{profile} host"),
        client_id: client.map(str::to_owned),
        server_identity: Some("server-a".into()),
        panes: HashSet::from(["%4".to_owned()]),
        start_directory: None,
    }
}

type Opened = Rc<RefCell<Vec<(String, String, String)>>>;

struct Harness {
    helper: Rc<Helper>,
    panel: Entity<AutomationsPanel>,
    messages: Rc<RefCell<Vec<String>>>,
    opened: Opened,
}

impl Harness {
    fn new(cx: &mut TestAppContext, hosts: Vec<PanelHost>) -> Self {
        let helper = Rc::new(Helper::default());
        let today = NaiveDate::from_ymd_opt(2026, 10, 13).unwrap();
        let panel = cx.new(|_| {
            AutomationsPanel::new(
                Rc::new(Transport(helper.clone())),
                Rc::new(move || (100_000, today)),
            )
        });
        let messages = Rc::new(RefCell::new(Vec::new()));
        let opened: Opened = Rc::default();
        cx.update(|cx| {
            let (messages, opened) = (messages.clone(), opened.clone());
            cx.subscribe(&panel, move |_, event, _| match event {
                PanelEvent::Message(message) => messages.borrow_mut().push(message.clone()),
                PanelEvent::OpenPane {
                    profile_id,
                    pane_id,
                    source,
                } => {
                    opened
                        .borrow_mut()
                        .push((profile_id.clone(), pane_id.clone(), source.clone()))
                }
            })
            .detach();
        });
        let harness = Self {
            helper,
            panel,
            messages,
            opened,
        };
        harness.update(cx, |panel, cx| panel.set_hosts(hosts, "local".into(), cx));
        harness
    }

    /// A connected local host listing one enabled automation.
    fn listed(cx: &mut TestAppContext) -> Self {
        let harness = Self::new(cx, vec![host("local", Some("client"))]);
        harness
            .helper
            .answer(cx, Ok(listing("g1", vec![summary("ci", true)])));
        harness
    }

    fn read<R>(&self, cx: &mut TestAppContext, read: impl FnOnce(&AutomationsPanel) -> R) -> R {
        self.panel.read_with(cx, |panel, _| read(panel))
    }

    fn update(
        &self,
        cx: &mut TestAppContext,
        update: impl FnOnce(&mut AutomationsPanel, &mut Context<AutomationsPanel>),
    ) {
        self.panel.update(cx, update);
        cx.run_until_parked();
    }

    fn editor_error(&self, cx: &mut TestAppContext) -> Option<String> {
        self.read(cx, |panel| {
            panel
                .editor
                .as_ref()
                .and_then(|editor| editor.error.clone())
        })
    }

    fn generation(&self, cx: &mut TestAppContext) -> String {
        self.read(cx, |panel| panel.listing().unwrap().generation.clone())
    }
}

#[gpui_kit::test]
fn lists_a_connected_host_and_asks_a_disconnected_one_nothing(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    assert_eq!(harness.helper.actions(), ["list"]);
    assert_eq!(
        harness.read(cx, |panel| panel
            .listing()
            .map(|listing| listing.automations.len())),
        Some(1)
    );

    let offline = Harness::new(cx, vec![host("local", None)]);
    assert!(offline.helper.actions().is_empty());
}

#[gpui_kit::test]
fn pauses_through_a_save_over_the_listed_version(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| panel.toggle("ci", cx));
    let save = harness.helper.last();
    assert_eq!(
        (save.action.as_str(), save.base_generation.as_str()),
        ("save", "g1")
    );
    assert!(!save.definition.unwrap().enabled);
    harness
        .helper
        .answer(cx, Ok(listing("g2", vec![summary("ci", false)])));
    assert_eq!(harness.generation(cx), "g2");
}

#[gpui_kit::test]
fn starts_one_run_per_click_not_one_per_impatient_click(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| {
        panel.run_now("ci", cx);
        panel.run_now("ci", cx);
    });
    assert_eq!(harness.helper.actions(), ["list", "runNow"]);
    assert!(harness.read(cx, |panel| panel.starting("ci")));
    harness
        .helper
        .answer(cx, Ok(listing("g1", vec![summary("ci", true)])));
    assert!(!harness.read(cx, |panel| panel.starting("ci")));
    harness.update(cx, |panel, cx| panel.run_now("ci", cx));
    assert_eq!(harness.helper.actions(), ["list", "runNow", "runNow"]);
}

#[gpui_kit::test]
fn reloads_when_the_file_changed_under_a_save(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| panel.toggle("ci", cx));
    harness
        .helper
        .answer(cx, Err("stale_generation: the file changed".into()));
    assert_eq!(
        *harness.messages.borrow(),
        ["automations.json changed on the host; the list is reloaded."]
    );
    assert_eq!(harness.helper.actions(), ["list", "save", "list"]);
    assert_eq!(harness.read(cx, |panel| panel.error.clone()), None);
}

#[gpui_kit::test]
fn a_refusal_reads_without_its_code(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| panel.toggle("ci", cx));
    harness
        .helper
        .answer(cx, Err("invalid_definition: bad cron".into()));
    assert_eq!(
        harness.read(cx, |panel| panel.error.clone()).as_deref(),
        Some("bad cron")
    );
}

#[gpui_kit::test]
fn lists_again_when_its_host_says_automations_changed_and_only_its_host(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| panel.automation_changed("elsewhere", cx));
    assert_eq!(harness.helper.actions(), ["list"]);
    harness.update(cx, |panel, cx| panel.automation_changed("local", cx));
    assert_eq!(harness.helper.actions(), ["list", "list"]);
}

#[gpui_kit::test]
fn keeps_only_the_newest_list_when_an_older_one_answers_late(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| {
        panel.reload(cx);
        panel.reload(cx);
    });
    harness.helper.answer_newest(cx, Ok(listing("new", vec![])));
    harness.helper.answer(cx, Ok(listing("old", vec![])));
    assert_eq!(harness.generation(cx), "new");
}

#[gpui_kit::test]
fn another_host_has_its_own_list_and_closes_the_editor(cx: &mut TestAppContext) {
    let harness = Harness::new(
        cx,
        vec![host("local", Some("client")), host("box", Some("client-b"))],
    );
    harness
        .helper
        .answer(cx, Ok(listing("g1", vec![summary("ci", true)])));
    harness.update(cx, |panel, cx| {
        panel.select(Some("ci".into()), cx);
        panel.open_editor(Some("ci"), cx);
    });
    harness.update(cx, |panel, cx| panel.choose("box".into(), cx));
    assert!(harness.read(cx, |panel| panel.editor.is_none()
        && panel.selected.is_none()
        && panel.listing().is_none()));
    assert_eq!(harness.helper.last().action, "list");
}

#[gpui_kit::test]
fn opens_a_runs_tab_only_while_it_is_the_same_tab(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    let run = |status: &str, pane: &str, server: &str| AutomationRun {
        number: 3,
        status: status.into(),
        pane_id: pane.into(),
        server_identity: server.into(),
        ..AutomationRun::default()
    };
    assert!(harness.read(cx, |panel| panel.openable(&run("done", "%4", "server-a"))));
    // A finished run on a tmux server since replaced: its pane id may be reused.
    assert!(!harness.read(cx, |panel| panel.openable(&run("done", "%4", "server-old"))));
    assert!(harness.read(cx, |panel| panel.openable(&run(
        "working",
        "%4",
        "server-old"
    ))));
    assert!(!harness.read(cx, |panel| {
        panel.openable(&run("working", "%9", "server-a"))
    }));

    harness.update(cx, |panel, cx| {
        panel.select(Some("ci".into()), cx);
        panel.open_run(&run("done", "%4", "server-a"), cx);
    });
    assert_eq!(
        *harness.opened.borrow(),
        [("local".to_owned(), "%4".to_owned(), "Fix CI #3".to_owned())]
    );
}

#[gpui_kit::test]
fn the_editor_asks_for_what_is_missing_instead_of_saving(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| panel.open_editor(None, cx));
    harness.update(cx, |panel, cx| panel.save(false, cx));
    assert_eq!(harness.editor_error(cx).as_deref(), Some("Add a name."));
    harness.update(cx, |panel, cx| {
        panel.update_draft(|draft| draft.name = "Fix CI".into(), cx)
    });
    assert_eq!(harness.editor_error(cx), None);
    harness.update(cx, |panel, cx| panel.save(false, cx));
    assert_eq!(harness.editor_error(cx).as_deref(), Some("Add a prompt."));
    assert!(!harness.helper.actions().contains(&"save".to_owned()));
}

#[gpui_kit::test]
fn saves_a_new_automation_under_a_unique_id_then_runs_it(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| {
        panel.open_editor(None, cx);
        panel.update_draft(
            |draft| {
                draft.name = " Fix CI ".into();
                draft.prompt = "go".into();
            },
            cx,
        );
    });
    harness.update(cx, |panel, cx| {
        panel.update_draft(|draft| draft.name = "ci".into(), cx);
    });
    assert_eq!(harness.read(cx, |panel| panel.editor_id()), "ci-2");
    harness.update(cx, |panel, cx| panel.save(true, cx));
    let save = harness.helper.last();
    let saved = save.definition.unwrap();
    assert_eq!(
        (saved.id.as_str(), save.base_generation.as_str()),
        ("ci-2", "g1")
    );
    assert!(harness.read(cx, |panel| panel.editor.as_ref().unwrap().busy));
    harness
        .helper
        .answer(cx, Ok(listing("g2", vec![summary("ci", true)])));
    assert_eq!(harness.helper.last().action, "runNow");
    assert_eq!(harness.helper.last().id, "ci-2");
    harness.helper.answer(cx, Ok(listing("g3", vec![])));
    assert!(harness.read(cx, |panel| panel.editor.is_none()));
    assert_eq!(harness.generation(cx), "g3");
}

#[gpui_kit::test]
fn trims_the_name_it_saves(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| {
        panel.open_editor(None, cx);
        panel.update_draft(
            |draft| {
                draft.name = " Fix CI ".into();
                draft.prompt = "go".into();
            },
            cx,
        );
        panel.save(false, cx);
    });
    let saved = harness.helper.last().definition.unwrap();
    assert_eq!(
        (saved.id.as_str(), saved.name.as_str()),
        ("fix-ci", "Fix CI")
    );
}

#[gpui_kit::test]
fn keeps_its_id_once_saved_so_a_retry_never_makes_a_twin(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| {
        panel.open_editor(None, cx);
        panel.update_draft(
            |draft| {
                draft.name = "Nightly".into();
                draft.prompt = "go".into();
            },
            cx,
        );
        panel.save(true, cx);
    });
    harness.helper.answer(cx, Ok(listing("g2", vec![])));
    harness
        .helper
        .answer(cx, Err("run_failed: the shell did not start".into()));
    assert_eq!(
        harness.editor_error(cx).as_deref(),
        Some("the shell did not start")
    );
    harness.update(cx, |panel, cx| {
        panel.update_draft(|draft| draft.name = "Renamed".into(), cx);
        panel.save(false, cx);
    });
    let retry = harness.helper.last();
    assert_eq!(retry.definition.unwrap().id, "nightly");
    assert_eq!(retry.base_generation, "g2");
}

#[gpui_kit::test]
fn sends_one_change_while_one_is_out(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| {
        panel.open_editor(Some("ci"), cx);
        panel.save(false, cx);
        // A repeated Enter, then Delete twice, while the save is out.
        panel.save(false, cx);
        panel.remove(cx);
        panel.remove(cx);
    });
    let changes = harness
        .helper
        .actions()
        .into_iter()
        .filter(|action| action != "list" && action != "previewSchedule")
        .collect::<Vec<_>>();
    assert_eq!(changes, ["save"]);
}

#[gpui_kit::test]
fn saves_over_a_newer_file_only_after_a_refusal_and_the_newer_listing(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| panel.open_editor(Some("ci"), cx));
    // A newer listing on its own does not move the base.
    harness.update(cx, |panel, cx| panel.reload(cx));
    harness
        .helper
        .answer(cx, Ok(listing("g2", vec![summary("ci", true)])));
    harness.update(cx, |panel, cx| panel.save(false, cx));
    assert_eq!(harness.helper.last().base_generation, "g1");

    harness
        .helper
        .answer(cx, Err("stale_generation: newer file".into()));
    assert_eq!(
        harness.editor_error(cx).as_deref(),
        Some(
            "automations.json changed on the host since this was opened. Save again to apply your edit over the newer file."
        )
    );
    assert_eq!(harness.helper.last().action, "list");
    harness
        .helper
        .answer(cx, Ok(listing("g3", vec![summary("ci", true)])));
    harness.update(cx, |panel, cx| panel.save(false, cx));
    assert_eq!(harness.helper.last().base_generation, "g3");
}

#[gpui_kit::test]
fn deletes_only_on_a_second_click(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| {
        panel.select(Some("ci".into()), cx);
        panel.open_editor(Some("ci"), cx);
        panel.remove(cx);
    });
    assert!(!harness.helper.actions().contains(&"delete".to_owned()));
    assert!(harness.read(cx, |panel| panel.editor.as_ref().unwrap().confirming_delete));
    harness.update(cx, |panel, cx| panel.remove(cx));
    let delete = harness.helper.last();
    assert_eq!(
        (
            delete.action.as_str(),
            delete.id.as_str(),
            delete.base_generation.as_str()
        ),
        ("delete", "ci", "g1")
    );
    harness.helper.answer(cx, Ok(listing("g2", vec![])));
    assert!(harness.read(cx, |panel| panel.editor.is_none()
        && panel.selected.is_none()));
}

#[gpui_kit::test]
fn starts_a_new_automation_in_the_hosts_start_directory(cx: &mut TestAppContext) {
    let mut configured = host("local", Some("client"));
    configured.start_directory = Some("~/src".into());
    let harness = Harness::new(cx, vec![configured]);
    harness
        .helper
        .answer(cx, Ok(listing("g1", vec![summary("ci", true)])));
    harness.update(cx, |panel, cx| panel.open_editor(None, cx));
    assert_eq!(
        harness.read(cx, |panel| panel.editor.as_ref().unwrap().draft.cwd.clone()),
        "~/src"
    );
    harness.update(cx, |panel, cx| panel.open_editor(Some("ci"), cx));
    assert_eq!(
        harness.read(cx, |panel| panel.editor.as_ref().unwrap().draft.cwd.clone()),
        "~/dev"
    );
}

#[gpui_kit::test]
fn previews_the_schedule_once_typing_stops(cx: &mut TestAppContext) {
    let harness = Harness::listed(cx);
    harness.update(cx, |panel, cx| panel.open_editor(None, cx));
    harness.update(cx, |panel, cx| {
        panel.update_draft(|draft| draft.schedule = "0 7 * * *".into(), cx)
    });
    // Nothing is asked before the debounce, and the edit restarted it.
    assert_eq!(harness.helper.actions(), ["list"]);
    cx.executor().advance_clock(Duration::from_millis(300));
    cx.run_until_parked();
    assert_eq!(harness.helper.actions(), ["list", "previewSchedule"]);
    assert_eq!(harness.helper.last().schedule, "0 7 * * *");
    harness.helper.answer(
        cx,
        Ok(json!({ "nextRuns": ["2026-10-14T07:00"], "scheduleError": "" })),
    );
    let preview = harness
        .read(cx, |panel| panel.editor.as_ref().unwrap().preview.clone())
        .unwrap();
    assert_eq!(preview.next_runs, ["2026-10-14T07:00"]);
}
