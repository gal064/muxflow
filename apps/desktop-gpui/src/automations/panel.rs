//! The Automations panel's state: the host it lists, the newest listing, the
//! selected automation and the open editor. Port of the logic of
//! `AutomationsPanel.tsx` and `AutomationEditor.tsx`; the view is in
//! [`super::view`].
//!
//! Every answer from the helper is a listing, and only the newest request may
//! land: an older one, from a host switched away from, would overwrite it.

use std::{collections::HashSet, rc::Rc, sync::Arc, time::Duration};

use chrono::NaiveDate;
use desktop_core::connection::automation::AutomationCommand;
use futures::future::BoxFuture;
use gpui_kit::{Context, EventEmitter, Task};
use serde_json::Value;

use crate::connection_ui::backend::TerminalBackend;

use super::model::{
    AutomationDefinition, AutomationListing, AutomationRun, AutomationSummary, Missing,
    SchedulePreview, blank_definition, id_from_name, is_stale_generation, missing, refusal_text,
    unfinished,
};

/// How the panel reaches a host's helper.
pub trait AutomationTransport {
    fn request(
        &self,
        client_id: String,
        command: AutomationCommand,
    ) -> BoxFuture<'static, Result<Value, String>>;
}

pub struct BackendTransport(pub Arc<dyn TerminalBackend>);

impl AutomationTransport for BackendTransport {
    fn request(
        &self,
        client_id: String,
        command: AutomationCommand,
    ) -> BoxFuture<'static, Result<Value, String>> {
        self.0.automation_request(client_id, command)
    }
}

/// One shown host, as the panel needs it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PanelHost {
    pub profile_id: String,
    pub label: String,
    /// The live bridge's client, only while the host is connected.
    pub client_id: Option<String>,
    /// The tmux server the host is showing now, once it has said.
    pub server_identity: Option<String>,
    /// The panes open on that server now.
    pub panes: HashSet<String>,
    /// Settings' "Start in directory" for new workspaces on this host, if set.
    pub start_directory: Option<String>,
}

pub enum PanelEvent {
    Message(String),
    /// Brings a run's pane on screen, on whichever host it is.
    OpenPane {
        profile_id: String,
        pane_id: String,
        source: String,
    },
}

/// Milliseconds since the epoch, and the local calendar day.
pub type Clock = Rc<dyn Fn() -> (u64, NaiveDate)>;

pub fn system_clock() -> Clock {
    Rc::new(|| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64);
        (now, chrono::Local::now().date_naive())
    })
}

/// How long the schedule waits for typing to stop before asking the host.
const PREVIEW_DEBOUNCE: Duration = Duration::from_millis(300);
/// How often an unfinished run's age moves.
const TICK: Duration = Duration::from_secs(30);

pub struct Editor {
    epoch: u64,
    /// The host it was opened on; it closes when the panel moves to another.
    pub profile_id: String,
    /// Absent for a new automation.
    pub summary: Option<AutomationSummary>,
    pub draft: AutomationDefinition,
    pub preview: Option<SchedulePreview>,
    pub error: Option<String>,
    pub busy: bool,
    pub confirming_delete: bool,
    /// The version of the file this draft was made against. It moves only
    /// when the user has seen a refusal and saves again over the newer file,
    /// so an edit made on the host meanwhile is never overwritten silently.
    pub base_generation: String,
    /// Set to the refused version; the next different listing becomes the base.
    refused: Option<String>,
    /// Fixed by the first save, so retrying "Save and run now" never makes a twin.
    saved_id: Option<String>,
    /// The field the footer's message is about.
    pub flagged: Option<Missing>,
    preview_task: Option<Task<()>>,
}

impl Editor {
    pub fn is_new(&self) -> bool {
        self.summary.is_none()
    }

    pub fn invalid(&self, field: Missing) -> bool {
        self.flagged == Some(field) && missing(&self.draft) == Some(field)
    }
}

pub struct AutomationsPanel {
    transport: Rc<dyn AutomationTransport>,
    clock: Clock,
    hosts: Vec<PanelHost>,
    active_profile: String,
    chosen: Option<String>,
    loaded: Option<(String, AutomationListing)>,
    pub error: Option<String>,
    pub selected: Option<String>,
    starting: HashSet<String>,
    latest: u64,
    /// The host and client the list was last asked for.
    asked: Option<(String, Option<String>)>,
    pub editor: Option<Editor>,
    editor_epoch: u64,
    tick: Option<Task<()>>,
}

impl EventEmitter<PanelEvent> for AutomationsPanel {}

impl AutomationsPanel {
    pub fn new(transport: Rc<dyn AutomationTransport>, clock: Clock) -> Self {
        Self {
            transport,
            clock,
            hosts: Vec::new(),
            active_profile: String::new(),
            chosen: None,
            loaded: None,
            error: None,
            selected: None,
            starting: HashSet::new(),
            latest: 0,
            asked: None,
            editor: None,
            editor_epoch: 0,
            tick: None,
        }
    }

    pub fn now(&self) -> (u64, NaiveDate) {
        (self.clock)()
    }

    pub fn hosts(&self) -> &[PanelHost] {
        &self.hosts
    }

    /// The host the chooser names, else the one on screen, else the first.
    pub fn host(&self) -> Option<&PanelHost> {
        let find = |id: &str| self.hosts.iter().find(|host| host.profile_id == id);
        self.chosen
            .as_deref()
            .and_then(find)
            .or_else(|| find(&self.active_profile))
            .or_else(|| self.hosts.first())
    }

    fn client_id(&self) -> Option<String> {
        self.host().and_then(|host| host.client_id.clone())
    }

    /// The host's listing, never another host's.
    pub fn listing(&self) -> Option<&AutomationListing> {
        let host = self.host()?;
        self.loaded
            .as_ref()
            .filter(|(profile, _)| *profile == host.profile_id)
            .map(|(_, listing)| listing)
    }

    pub fn selected_summary(&self) -> Option<&AutomationSummary> {
        let id = self.selected.as_deref()?;
        self.listing()?
            .automations
            .iter()
            .find(|summary| summary.definition.id == id)
    }

    /// A file that cannot be read is shown, not edited.
    pub fn read_only(&self) -> bool {
        self.listing()
            .is_some_and(|listing| !listing.file_error.is_empty())
    }

    pub fn starting(&self, id: &str) -> bool {
        self.starting.contains(id)
    }

    /// A finished run's pane id may have been reused by a later tmux server;
    /// an unfinished run's server is the one showing.
    pub fn openable(&self, run: &AutomationRun) -> bool {
        let Some(host) = self.host() else {
            return false;
        };
        !run.pane_id.is_empty()
            && host.panes.contains(&run.pane_id)
            && (unfinished(run)
                || host.server_identity.as_deref() == Some(run.server_identity.as_str()))
    }

    pub fn set_hosts(
        &mut self,
        hosts: Vec<PanelHost>,
        active_profile: String,
        cx: &mut Context<Self>,
    ) {
        if hosts == self.hosts && active_profile == self.active_profile {
            return;
        }
        let before = self.host().map(|host| host.profile_id.clone());
        self.hosts = hosts;
        self.active_profile = active_profile;
        self.host_changed(before, cx);
        cx.notify();
    }

    pub fn choose(&mut self, profile_id: String, cx: &mut Context<Self>) {
        let before = self.host().map(|host| host.profile_id.clone());
        self.chosen = Some(profile_id);
        self.host_changed(before, cx);
        cx.notify();
    }

    fn host_changed(&mut self, before: Option<String>, cx: &mut Context<Self>) {
        let now = self
            .host()
            .map(|host| (host.profile_id.clone(), host.client_id.clone()));
        // An editor belongs to the host it was opened on.
        if now.as_ref().map(|(profile, _)| profile) != before.as_ref() {
            self.selected = None;
            self.editor = None;
        }
        if now != self.asked {
            self.asked = now;
            self.reload(cx);
            // An editor kept through a reconnect asks about its schedule again.
            self.preview_schedule(cx);
        }
    }

    /// The host said its automations changed.
    pub fn automation_changed(&mut self, profile_id: &str, cx: &mut Context<Self>) {
        if self
            .host()
            .is_some_and(|host| host.profile_id == profile_id)
        {
            self.reload(cx);
        }
    }

    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(host) = self.host() else { return };
        let Some(client_id) = host.client_id.clone() else {
            return;
        };
        let profile_id = host.profile_id.clone();
        self.latest += 1;
        let request = self.latest;
        let answer = self.request(client_id, "list", AutomationCommand::default());
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |this, cx| {
                if request != this.latest {
                    return;
                }
                match answer {
                    Ok(listing) => {
                        this.error = None;
                        this.listed(profile_id, listing, cx);
                    }
                    Err(error) => this.error = Some(refusal_text(&error)),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn request(
        &self,
        client_id: String,
        action: &str,
        command: AutomationCommand,
    ) -> BoxFuture<'static, Result<AutomationListing, String>> {
        let answer = self.transport.request(
            client_id,
            AutomationCommand {
                action: action.to_owned(),
                ..command
            },
        );
        Box::pin(async move {
            let value = answer.await?;
            serde_json::from_value(value)
                .map_err(|error| format!("unreadable automation listing: {error}"))
        })
    }

    fn listed(&mut self, profile_id: String, listing: AutomationListing, cx: &mut Context<Self>) {
        // The refused draft rebases onto the first different listing.
        if let Some(editor) = self.editor.as_mut()
            && editor.profile_id == profile_id
            && editor
                .refused
                .as_ref()
                .is_some_and(|refused| *refused != listing.generation)
        {
            editor.base_generation = listing.generation.clone();
            editor.refused = None;
        }
        self.loaded = Some((profile_id, listing));
        self.arm_tick(cx);
    }

    /// Re-renders every half minute while some run is unfinished, so its age moves.
    fn arm_tick(&mut self, cx: &mut Context<Self>) {
        let active = self.listing().is_some_and(|listing| {
            listing
                .automations
                .iter()
                .any(|summary| summary.recent_runs.iter().any(unfinished))
        });
        if !active {
            self.tick = None;
        } else if self.tick.is_none() {
            self.tick = Some(cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(TICK).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        return;
                    }
                }
            }));
        }
    }

    /// One mutation; like a list, only the newest answer may land.
    fn act(
        &mut self,
        action: &str,
        command: AutomationCommand,
        cx: &mut Context<Self>,
        after: impl FnOnce(&mut Self) + 'static,
    ) {
        let (Some(client_id), Some(host)) = (self.client_id(), self.host()) else {
            return;
        };
        let from = host.profile_id.clone();
        self.error = None;
        self.latest += 1;
        let request = self.latest;
        let answer = self.request(client_id, action, command);
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |this, cx| {
                after(this);
                if request == this.latest {
                    match answer {
                        Ok(listing) => this.listed(from, listing, cx),
                        Err(error) if is_stale_generation(&error) => {
                            cx.emit(PanelEvent::Message(
                                "automations.json changed on the host; the list is reloaded."
                                    .into(),
                            ));
                            this.reload(cx);
                        }
                        Err(error) => this.error = Some(refusal_text(&error)),
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn summary(&self, id: &str) -> Option<AutomationSummary> {
        self.listing()?
            .automations
            .iter()
            .find(|summary| summary.definition.id == id)
            .cloned()
    }

    pub fn toggle(&mut self, id: &str, cx: &mut Context<Self>) {
        let (Some(summary), Some(listing)) = (self.summary(id), self.listing()) else {
            return;
        };
        if self.read_only() {
            return;
        }
        let command = AutomationCommand {
            definition: Some(AutomationDefinition {
                enabled: !summary.definition.enabled,
                ..summary.definition
            }),
            base_generation: listing.generation.clone(),
            ..AutomationCommand::default()
        };
        self.act("save", command, cx, |_| {});
    }

    /// A run started now skips the overlap check, so a second click must wait.
    pub fn run_now(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.client_id().is_none() || self.starting.contains(id) {
            return;
        }
        self.starting.insert(id.to_owned());
        let command = AutomationCommand {
            id: id.to_owned(),
            ..AutomationCommand::default()
        };
        let started = id.to_owned();
        self.act("runNow", command, cx, move |this| {
            this.starting.remove(&started);
        });
    }

    pub fn select(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.selected = id;
        cx.notify();
    }

    pub fn open_run(&mut self, run: &AutomationRun, cx: &mut Context<Self>) {
        let (Some(host), Some(summary)) = (self.host(), self.selected_summary()) else {
            return;
        };
        let event = PanelEvent::OpenPane {
            profile_id: host.profile_id.clone(),
            pane_id: run.pane_id.clone(),
            source: format!("{} #{}", summary.definition.name, run.number),
        };
        cx.emit(event);
    }

    // The editor.

    /// Opens the editor, for `id` or for a new automation.
    pub fn open_editor(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        let (Some(host), Some(listing)) = (self.host(), self.listing()) else {
            return;
        };
        let (profile_id, start, generation) = (
            host.profile_id.clone(),
            host.start_directory
                .clone()
                .filter(|directory| !directory.is_empty()),
            listing.generation.clone(),
        );
        let summary = id.and_then(|id| self.summary(id));
        let mut draft = match &summary {
            Some(summary) => summary.definition.clone(),
            None => blank_definition(),
        };
        // A new automation starts where new workspaces on this host do.
        if summary.is_none()
            && let Some(directory) = start
        {
            draft.cwd = directory;
        }
        self.editor_epoch += 1;
        self.editor = Some(Editor {
            epoch: self.editor_epoch,
            profile_id,
            summary,
            draft,
            preview: None,
            error: None,
            busy: false,
            confirming_delete: false,
            base_generation: generation,
            refused: None,
            saved_id: None,
            flagged: None,
            preview_task: None,
        });
        self.preview_schedule(cx);
        cx.notify();
    }

    pub fn close_editor(&mut self, cx: &mut Context<Self>) {
        if self.editor.take().is_some() {
            cx.notify();
        }
    }

    /// The id a save writes: fixed once saved, else the name's for a new one.
    pub fn editor_id(&self) -> String {
        let Some(editor) = &self.editor else {
            return String::new();
        };
        if let Some(saved) = &editor.saved_id {
            return saved.clone();
        }
        if !editor.is_new() {
            return editor.draft.id.clone();
        }
        let taken: Vec<String> = self
            .listing()
            .map(|listing| {
                listing
                    .automations
                    .iter()
                    .map(|summary| summary.definition.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        id_from_name(&editor.draft.name, &taken)
    }

    pub fn update_draft(
        &mut self,
        update: impl FnOnce(&mut AutomationDefinition),
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let schedule = editor.draft.schedule.clone();
        update(&mut editor.draft);
        // An answer about the draft before this edit no longer applies.
        editor.error = None;
        if editor.draft.schedule != schedule {
            self.preview_schedule(cx);
        }
        cx.notify();
    }

    /// The host reads the schedule, so what is shown is what will run.
    fn preview_schedule(&mut self, cx: &mut Context<Self>) {
        let client = self.client_id();
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let Some(client_id) = client else {
            editor.preview_task = None;
            return;
        };
        let schedule = editor.draft.schedule.clone();
        let transport = self.transport.clone();
        let epoch = editor.epoch;
        editor.preview_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PREVIEW_DEBOUNCE).await;
            let answer = transport
                .request(
                    client_id,
                    AutomationCommand {
                        action: "previewSchedule".into(),
                        schedule,
                        ..AutomationCommand::default()
                    },
                )
                .await
                .and_then(|value| {
                    serde_json::from_value::<SchedulePreview>(value)
                        .map_err(|error| error.to_string())
                });
            this.update(cx, |this, cx| {
                if let Some(editor) = this.editor.as_mut().filter(|editor| editor.epoch == epoch) {
                    editor.preview = answer.ok();
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn editor_mut(&mut self, epoch: u64) -> Option<&mut Editor> {
        self.editor.as_mut().filter(|editor| editor.epoch == epoch)
    }

    pub fn save(&mut self, run_after: bool, cx: &mut Context<Self>) {
        let id = self.editor_id();
        let client = self.client_id();
        let label = self
            .host()
            .map(|host| host.label.clone())
            .unwrap_or_default();
        // Enter and the buttons can repeat while a request is out.
        let Some(editor) = self.editor.as_mut().filter(|editor| !editor.busy) else {
            return;
        };
        if let Some(field) = missing(&editor.draft) {
            editor.flagged = Some(field);
            editor.error = Some(format!("Add {}.", field.text()));
            cx.notify();
            return;
        }
        let Some(client_id) = client else {
            editor.error = Some(format!("{label} is not connected."));
            cx.notify();
            return;
        };
        editor.busy = true;
        editor.error = None;
        let definition = AutomationDefinition {
            id: id.clone(),
            name: editor.draft.name.trim().to_owned(),
            ..editor.draft.clone()
        };
        let command = AutomationCommand {
            definition: Some(definition),
            base_generation: editor.base_generation.clone(),
            ..AutomationCommand::default()
        };
        let epoch = editor.epoch;
        let save = self.request(client_id.clone(), "save", command);
        let transport = self.transport.clone();
        cx.notify();
        cx.spawn(async move |this, cx| {
            let saved = save.await;
            let listing = match saved {
                Ok(listing) => listing,
                Err(error) => {
                    this.update(cx, |this, cx| this.failed(epoch, &error, cx))
                        .ok();
                    return;
                }
            };
            let run = this
                .update(cx, |this, _| {
                    if let Some(editor) = this.editor_mut(epoch) {
                        editor.saved_id = Some(id.clone());
                        editor.base_generation = listing.generation.clone();
                    }
                    run_after.then(|| {
                        let command = AutomationCommand {
                            action: "runNow".into(),
                            id: id.clone(),
                            ..AutomationCommand::default()
                        };
                        transport.request(client_id, command)
                    })
                })
                .ok()
                .flatten();
            let listing = match run {
                None => Ok(listing),
                Some(run) => run.await.and_then(|value| {
                    serde_json::from_value(value).map_err(|error| error.to_string())
                }),
            };
            this.update(cx, |this, cx| match listing {
                Ok(listing) => this.saved(epoch, listing, None, cx),
                Err(error) => this.failed(epoch, &error, cx),
            })
            .ok();
        })
        .detach();
    }

    /// Delete asks twice: the first click arms it.
    pub fn remove(&mut self, cx: &mut Context<Self>) {
        let client = self.client_id();
        let label = self
            .host()
            .map(|host| host.label.clone())
            .unwrap_or_default();
        // Enter and the buttons can repeat while a request is out.
        let Some(editor) = self.editor.as_mut().filter(|editor| !editor.busy) else {
            return;
        };
        if !editor.confirming_delete {
            editor.confirming_delete = true;
            cx.notify();
            return;
        }
        let Some(client_id) = client else {
            editor.error = Some(format!("{label} is not connected."));
            cx.notify();
            return;
        };
        editor.busy = true;
        let id = editor.draft.id.clone();
        let command = AutomationCommand {
            id: id.clone(),
            base_generation: editor.base_generation.clone(),
            ..AutomationCommand::default()
        };
        let epoch = editor.epoch;
        let removed = self.request(client_id, "delete", command);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let removed = removed.await;
            this.update(cx, |this, cx| match removed {
                Ok(listing) => this.saved(epoch, listing, Some(id), cx),
                Err(error) => this.failed(epoch, &error, cx),
            })
            .ok();
        })
        .detach();
    }

    fn saved(
        &mut self,
        epoch: u64,
        listing: AutomationListing,
        deleted: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editor_mut(epoch) else {
            return;
        };
        let profile = editor.profile_id.clone();
        self.listed(profile, listing, cx);
        if deleted.is_some() && deleted == self.selected {
            self.selected = None;
        }
        self.editor = None;
        cx.notify();
    }

    fn failed(&mut self, epoch: u64, error: &str, cx: &mut Context<Self>) {
        let Some(editor) = self.editor_mut(epoch) else {
            return;
        };
        editor.busy = false;
        if is_stale_generation(error) {
            editor.error = Some(
                "automations.json changed on the host since this was opened. Save again to apply your edit over the newer file."
                    .into(),
            );
            editor.refused = Some(editor.base_generation.clone());
            self.reload(cx);
        } else {
            editor.error = Some(refusal_text(error));
        }
        cx.notify();
    }
}
