//! The Explorer's model: which tree it shows, the reads and watches that keep
//! it current, and the downloads of the live connection. Port of
//! `useWorkspaceFiles.ts`, `useActiveRoot.ts`, `directoryRequests.ts`,
//! `watchLeases.ts`, `useConnectionTransfers.ts` and the file half of
//! `useAppFileActions.ts`.
//!
//! Every read is a task, and dropping it stops the host's work too
//! ([`client::request`]): superseding, collapsing, deleting and leaving a scope
//! all come down to removing the task that was reading.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{Arc, mpsc},
    time::Duration,
};

use desktop_core::platform::{EventBody, EventSink};
use gpui_kit::{Context, EventEmitter, Task};
use serde_json::Value;

use crate::{
    editor::FileNotice,
    perf::PaintTicket,
    terminal::paint_gate::{PANE_PAINT_TIMEOUT, wait_for_pane_paint},
};

use super::{
    client::{self, DownloadStart, FileTransport, operation_id},
    downloads::{
        ActiveDownload, DownloadIntent, DownloadKind, DownloadList, DownloadStatus, decode_event,
        state_word,
    },
    listing::{
        Installed, ListingCache, Patched, TreeState, append_page, cache_scope, parent_path,
        patch_entry, remove_entry,
    },
    watches::Watches,
    wire::{
        ActiveRoot, DirectoryListing, FileMutation, FileScope, FileSelection, PaneRoute,
        WorkspaceEvent, command, decode_listing, decode_root, same_root,
    },
};

/// Pages one truncated listing may fetch back before it gives up.
const MAX_RESTORED_PAGES: usize = 8;
/// How long one directory's recovery waits before it is re-read. Only events a
/// held listing could not answer reach it; the first event of a burst starts
/// the wait, and the ones behind it do not push it back.
pub const RECOVERY_COALESCE: Duration = Duration::from_millis(150);
/// How often the active root is re-checked when nothing announced a change:
/// tmux announces no `cd` inside the pane already shown.
pub const ROOT_BACKSTOP: Duration = Duration::from_secs(15);
/// Unchanged probes after which the backstop slows down, and by how much. A
/// settled root is checked less often, never not at all.
pub const ROOT_STABLE_PROBES: u32 = 3;
pub const ROOT_SETTLED_MULTIPLIER: u32 = 8;
/// A refused watch waits this long, doubling on each further refusal.
pub const WATCH_RETRY_BASE: Duration = Duration::from_secs(1);
pub const WATCH_RETRY_MAX: Duration = Duration::from_secs(30);
/// How often a running download's events are collected. Polled, not woken: a
/// runtime thread must not wake a GPUI task.
const DOWNLOAD_POLL: Duration = Duration::from_millis(100);

const DISCONNECTED: &str = "File changes are unavailable while the host is disconnected.";
pub const DOWNLOAD_CANCELLED: &str =
    "Download cancelled because the active host or workspace changed.";

/// What a directory read is for. A page extends a listing, so it is nobody's
/// rival; a list and a restore both answer "what is here now", so the later
/// one wins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ReadKind {
    List,
    Page,
    Restore,
}

struct Read {
    id: u64,
    /// A person asked for it, so the header says "Refreshing…" until it ends.
    requested: bool,
    _task: Task<()>,
}

/// The active-root probe: one at a time, behind the pane's paint, with a
/// backstop timer that slows once the root has settled.
#[derive(Default)]
struct Probe {
    key: String,
    resolving: Option<Task<()>>,
    gate: Option<Task<()>>,
    backstop: Option<Task<()>>,
    unchanged: u32,
    ticks: u32,
}

/// Where a download was asked for. The Explorer acts on the live tree; a file
/// tab carries its own root capability, which `cd` does not invalidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadOrigin {
    Explorer,
    FileSurface,
    TabMenu,
}

pub enum ExplorerEvent {
    /// A line for the app's status channel.
    Status(String),
    /// A rename or move landed: open file tabs follow it.
    Relocated {
        host_profile_id: String,
        server_identity: String,
        root_path: String,
        source: String,
        destination: String,
    },
    /// A download changed; the status it announced may be owed an ending.
    DownloadChanged,
    /// What the host said about an open document's file.
    Notice {
        host: String,
        root_token: String,
        path: String,
        notice: FileNotice,
    },
}

/// What the Explorer is told about the app, recomputed whenever it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inputs {
    pub selection: Option<FileSelection>,
    pub scope: Option<FileScope>,
    pub route: Option<PaneRoute>,
    pub can_mutate: bool,
}

pub struct Explorer {
    transport: Rc<dyn FileTransport>,
    pub inputs: Inputs,
    selection_key: String,
    pub tree: TreeState,
    reads: HashMap<(String, ReadKind), Read>,
    next_id: u64,
    /// Recoveries waiting out their coalescing window, by root token and path.
    recoveries: HashMap<String, Task<()>>,
    cache: ListingCache,
    pub(super) watches: Watches,
    probe: Probe,
    /// Whether the window is visible or active; true until it says otherwise.
    foreground: bool,
    pub downloads: DownloadList,
    pub active_download: Option<ActiveDownload>,
    pub(super) picker_open: bool,
    /// The root a Quick Open search resolved, for the pane it resolved it in.
    pub(super) search_root: Option<(String, ActiveRoot)>,
    /// The view's state, kept here so a model change can keep the keyboard on
    /// the same row.
    pub(super) view: super::view::ViewState,
    /// Perf-log measurements (all empty unless the probe is on): expansions
    /// owed a paint by their listing, recoveries owed one by their re-read,
    /// and those the next frame of the tree completes.
    expand_paints: HashMap<String, PaintTicket>,
    recovery_paints: HashMap<String, PaintTicket>,
    pub(super) due_paints: Vec<PaintTicket>,
}

const EXPAND_PAINT: &[&str] = &[
    "explorer.expandToPaint",
    "workflow.explorer.directoryExpandPaint",
];
const EXTERNAL_CHANGE_PAINT: &[&str] = &["explorer.externalChangeToPaint"];
const ROOT_PAINT: &[&str] = &["workflow.explorer.rootPaint"];

impl EventEmitter<ExplorerEvent> for Explorer {}

impl Explorer {
    pub fn new(transport: Rc<dyn FileTransport>, cx: &mut Context<Self>) -> Self {
        Self {
            transport,
            inputs: Inputs::default(),
            selection_key: String::new(),
            tree: TreeState::default(),
            reads: HashMap::new(),
            next_id: 0,
            recoveries: HashMap::new(),
            cache: ListingCache::default(),
            watches: Watches::new(cx),
            probe: Probe::default(),
            foreground: true,
            downloads: DownloadList::default(),
            active_download: None,
            picker_open: false,
            search_root: None,
            view: super::view::ViewState::new(cx),
            expand_paints: HashMap::new(),
            recovery_paints: HashMap::new(),
            due_paints: Vec::new(),
        }
    }

    /// One pane's directory on one live connection: what a Quick Open
    /// search's root is resolved for.
    pub fn search_key(&self) -> Option<String> {
        let (scope, route) = (self.inputs.scope.as_ref()?, self.inputs.route.as_ref()?);
        Some(format!(
            "{}\0{}\0{}",
            scope.key(),
            route.window_id,
            route.cwd
        ))
    }

    pub fn transport(&self) -> Rc<dyn FileTransport> {
        self.transport.clone()
    }

    pub(super) fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn scope_key(&self) -> String {
        self.inputs
            .scope
            .as_ref()
            .map(FileScope::key)
            .unwrap_or_default()
    }

    /// Whether the tree on screen is this selection's own. False from the
    /// moment the selection moves until its root resolves: the previous tree
    /// stays painted, and nothing a person does acts on it.
    pub fn paints_selection(&self) -> bool {
        self.tree.scope_key == self.selection_key
    }

    /// The root the watch set, and anything acting on the tree, belongs to.
    pub fn active_root(&self) -> Option<&ActiveRoot> {
        self.tree.root.as_ref().filter(|_| self.paints_selection())
    }

    pub fn requested_reads(&self) -> usize {
        self.reads.values().filter(|read| read.requested).count()
    }

    pub fn reading(&self, path: &str, kind: ReadKind) -> bool {
        self.reads.contains_key(&(path.to_owned(), kind))
    }

    #[cfg(test)]
    pub fn in_flight(&self) -> usize {
        self.reads.len()
    }

    #[cfg(test)]
    pub fn watched(&self) -> Vec<String> {
        self.watches.watched()
    }

    #[cfg(test)]
    pub fn cached(&self) -> usize {
        self.cache.held()
    }

    // Inputs.

    /// Applies what the app now says. Port of the reset effects: a selection
    /// change keeps the tree painted and stops its work, a connection change
    /// stops the work and keeps the tree, and no selection blanks it.
    pub fn set_inputs(&mut self, inputs: Inputs, cx: &mut Context<Self>) {
        if inputs == self.inputs {
            return;
        }
        // A topology generation alone moves nothing on screen: the probe reads
        // it from the scope when it asks.
        let only_generation = {
            let mut same = inputs.clone();
            if let (Some(next), Some(held)) = (same.scope.as_mut(), self.inputs.scope.as_ref()) {
                next.generation = held.generation;
            }
            same == self.inputs
        };
        let selection_key = inputs
            .selection
            .as_ref()
            .map(FileSelection::key)
            .unwrap_or_default();
        let previous_scope = self.scope_key();
        let previous_cwd = self.inputs.route.as_ref().map(|route| route.cwd.clone());
        let selection_changed = selection_key != self.selection_key;
        let stale_before = !self.paints_selection();
        self.inputs = inputs;
        self.selection_key = selection_key;
        let scope_key = self.scope_key();
        let scope_changed = scope_key != previous_scope;
        if scope_changed {
            self.view.reset_for_root();
        }
        if selection_changed || scope_changed {
            self.abort_listing(|_| true);
            if self.selection_key.is_empty() {
                self.tree = TreeState::default();
            } else if selection_changed || !stale_before {
                self.tree.loading.clear();
                self.tree.error = None;
            }
        }
        let transfers = self
            .inputs
            .scope
            .as_ref()
            .map(FileScope::transfer_key)
            .unwrap_or_default();
        if self.downloads.set_connection(&transfers) {
            // The status line may be announcing one of them.
            cx.emit(ExplorerEvent::DownloadChanged);
        }
        if scope_changed {
            self.reset_probe(scope_key, cx);
        }
        // The pane's own working directory moved: the only sign of a `cd`. Not
        // across an absent value, which is connection churn.
        let cwd = self.inputs.route.as_ref().map(|route| route.cwd.clone());
        if !scope_changed && previous_cwd.is_some() && cwd.is_some() && previous_cwd != cwd {
            self.rearm(cx);
        }
        if !only_generation {
            self.changed(cx);
        }
    }

    /// Everything that follows a change to the tree: the watch set, the cache
    /// mirror, and a repaint.
    pub(super) fn changed(&mut self, cx: &mut Context<Self>) {
        self.sync_watches(false, cx);
        if let (Some(scope), Some(root)) = (self.inputs.scope.clone(), self.active_root().cloned())
        {
            // Only reachable directories are mirrored: those are the ones a
            // watch keeps correct.
            let key = cache_scope(&scope.client_id, &root);
            for directory in &self.watches.tree {
                if let Some(listing) = self.tree.listings.get(directory) {
                    self.cache.set(&key, &root.token, listing);
                }
            }
        }
        self.refresh_rows(cx);
        self.settle_expand_paints();
        cx.notify();
    }

    /// An expansion's paint is owed by whatever puts its rows on screen: a
    /// cached listing or the read it started. A collapse retires it.
    fn settle_expand_paints(&mut self) {
        if self.expand_paints.is_empty() {
            return;
        }
        let tree = &self.tree;
        self.expand_paints
            .retain(|path, _| tree.expanded.contains(path));
        let listed: Vec<String> = self
            .expand_paints
            .keys()
            .filter(|path| tree.listings.contains_key(*path))
            .cloned()
            .collect();
        for path in listed {
            if let Some(ticket) = self.expand_paints.remove(&path) {
                self.note_due(Some(ticket));
            }
        }
    }

    /// A measured change is on its way to the screen. Only a drawn tree
    /// paints it: a hidden one would publish the time it stayed hidden.
    fn note_due(&mut self, ticket: Option<PaintTicket>) {
        if self.view.shown {
            self.due_paints.extend(ticket);
        }
    }

    // Reads.

    /// Stops the reads and pending recoveries of every directory that matches,
    /// and gives their rows their wait state back.
    fn abort_listing(&mut self, matches: impl Fn(&str) -> bool) {
        self.abort_reads(&matches);
        let kept = |key: &String| {
            let path = key.split_once('\0').map_or(key.as_str(), |(_, path)| path);
            !matches(path)
        };
        self.recoveries.retain(|key, _| kept(key));
        self.recovery_paints.retain(|key, _| kept(key));
    }

    fn abort_reads(&mut self, matches: impl Fn(&str) -> bool) {
        let stopped: Vec<(String, ReadKind)> = self
            .reads
            .keys()
            .filter(|(path, _)| matches(path))
            .cloned()
            .collect();
        for key in stopped {
            self.reads.remove(&key);
            self.tree.loading.remove(&key.0);
        }
    }

    /// The one place a listing enters the tree, whether a watch bootstrap, a
    /// snapshot, a read or a restore brought it.
    pub(super) fn apply_listing(
        &mut self,
        root: &ActiveRoot,
        directory: &str,
        listing: DirectoryListing,
        append: bool,
        restored: bool,
        cx: &mut Context<Self>,
    ) {
        self.tree.loading.remove(directory);
        if listing.root_token != root.token {
            return;
        }
        let installed = if self.tree.on_directory(root, directory) {
            self.tree
                .install_listing(directory, listing, append, restored)
        } else {
            Installed::Done
        };
        if !append {
            // Whatever else is reading this directory describes it as it was.
            // A recovery still waiting is kept: the guard below defers to it.
            let directory = directory.to_owned();
            self.abort_reads(|path| path == directory);
        }
        if let Installed::Restore(entries) = installed {
            // A gap waiting to be listed outranks a restore: its list puts the
            // first page back, and this guard queues the restore from there.
            if !self.recoveries.contains_key(&recovery_key(root, directory)) {
                self.restore_pages(root.clone(), directory.to_owned(), entries, cx);
            }
        }
    }

    /// Reads one directory against the root that authorised it: a deferred
    /// caller must not list an old root's path against a new one.
    pub(super) fn load_directory(
        &mut self,
        root: &ActiveRoot,
        path: &str,
        page_token: Option<String>,
        requested: bool,
        fallback_error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(scope) = self.inputs.scope.clone() else {
            return;
        };
        if !same_root(self.tree.root.as_ref(), Some(root)) {
            return;
        }
        let kind = if page_token.is_some() {
            ReadKind::Page
        } else {
            self.reads.remove(&(path.to_owned(), ReadKind::Restore));
            ReadKind::List
        };
        self.tree.loading.insert(path.to_owned());
        self.tree.error = None;
        let id = self.next_id();
        let operation = operation_id();
        let request = client::request(
            self.transport.clone(),
            scope.client_id,
            operation.clone(),
            command::list(root, path, page_token.as_deref(), &operation),
        );
        let (root, owned) = (root.clone(), path.to_owned());
        let task = cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |this, cx| {
                this.listed(owned, kind, id, root, result, fallback_error, cx)
            })
            .ok();
        });
        self.reads.insert(
            (path.to_owned(), kind),
            Read {
                id,
                requested,
                _task: task,
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn listed(
        &mut self,
        path: String,
        kind: ReadKind,
        id: u64,
        root: ActiveRoot,
        result: Result<Value, String>,
        fallback_error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let key = (path.clone(), kind);
        if self.reads.get(&key).map(|read| read.id) != Some(id) {
            return;
        }
        self.reads.remove(&key);
        // A page of a longer listing is not the re-read a change asked for.
        let recovery_paint = (kind == ReadKind::List)
            .then(|| self.recovery_paints.remove(&recovery_key(&root, &path)))
            .flatten();
        match result.and_then(|value| decode_listing(value, &root.token)) {
            Ok(listing) if same_root(self.tree.root.as_ref(), Some(&root)) => {
                self.apply_listing(&root, &path, listing, kind == ReadKind::Page, false, cx);
                if self.tree.expanded.contains(&path) {
                    self.note_due(recovery_paint);
                }
            }
            Ok(_) => {
                self.tree.loading.remove(&path);
            }
            Err(error) => {
                self.tree.loading.remove(&path);
                if same_root(self.tree.root.as_ref(), Some(&root)) {
                    self.tree.error = Some(fallback_error.unwrap_or(error));
                }
            }
        }
        self.changed(cx);
    }

    /// Re-reads a directory a held listing could not patch, at most once per
    /// [`RECOVERY_COALESCE`].
    fn coalesce_recovery(&mut self, root: &ActiveRoot, path: &str, cx: &mut Context<Self>) {
        let key = recovery_key(root, path);
        if self.recoveries.contains_key(&key) {
            return;
        }
        if let Some(ticket) = PaintTicket::start(EXTERNAL_CHANGE_PAINT) {
            self.recovery_paints.insert(key.clone(), ticket);
        }
        let (root, path, owned_key) = (root.clone(), path.to_owned(), key.clone());
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RECOVERY_COALESCE).await;
            this.update(cx, |this, cx| {
                this.recoveries.remove(&owned_key);
                if !same_root(this.tree.root.as_ref(), Some(&root)) {
                    this.recovery_paints.remove(&owned_key);
                } else {
                    this.load_directory(&root, &path, None, false, None, cx);
                }
                this.changed(cx);
            })
            .ok();
        });
        self.recoveries.insert(key, task);
    }

    /// Fetches back the pages an authoritative first-page rescan replaced:
    /// until the listing is as long as it was, and never more than
    /// [`MAX_RESTORED_PAGES`]. Applied once, whole, and marked as a restore.
    fn restore_pages(
        &mut self,
        root: ActiveRoot,
        directory: String,
        entries: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(scope) = self.inputs.scope.clone() else {
            return;
        };
        self.reads.remove(&(directory.clone(), ReadKind::List));
        self.tree.loading.insert(directory.clone());
        let id = self.next_id();
        let transport = self.transport.clone();
        let key = (directory.clone(), ReadKind::Restore);
        let read = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let mut assembled: Option<DirectoryListing> = None;
            let mut failed = false;
            for _ in 0..MAX_RESTORED_PAGES {
                let token = match &assembled {
                    Some(held) if held.complete || held.next_page_token.is_none() => break,
                    Some(held) => held.next_page_token.clone(),
                    None => None,
                };
                let operation = operation_id();
                let answer = client::request(
                    transport.clone(),
                    scope.client_id.clone(),
                    operation.clone(),
                    command::list(&root, &directory, token.as_deref(), &operation),
                )
                .await
                .and_then(|value| decode_listing(value, &root.token));
                let Ok(next) = answer else {
                    failed = true;
                    break;
                };
                let grown = match assembled.take() {
                    Some(held) => append_page(&held, next),
                    None => next,
                };
                let done = grown.entries.len() >= entries;
                assembled = Some(grown);
                if done {
                    break;
                }
            }
            this.update(cx, |this, cx| {
                if this.reads.get(&read).map(|read| read.id) != Some(id) {
                    return;
                }
                this.reads.remove(&read);
                this.tree.loading.remove(&directory);
                // A failed restore keeps the rows the tree already had; the
                // next authoritative event asks again.
                if let Some(listing) = assembled.filter(|_| !failed) {
                    this.apply_listing(&root, &directory, listing, false, true, cx);
                }
                this.changed(cx);
            })
            .ok();
        });
        self.reads.insert(
            key,
            Read {
                id,
                requested: false,
                _task: task,
            },
        );
    }

    /// One page past a first page that did not finish the directory, and no
    /// further: speculating more would be the list storm all over again.
    pub(super) fn prefetch_next_page(
        &mut self,
        root: &ActiveRoot,
        directory: &str,
        listing: &DirectoryListing,
        cx: &mut Context<Self>,
    ) {
        if let Some(token) = listing
            .next_page_token
            .clone()
            .filter(|_| !listing.complete)
        {
            self.load_directory(root, directory, Some(token), false, None, cx);
        }
    }

    // Events.

    /// One mapped file-service event from the active host: for the open
    /// documents it concerns, then for the tree.
    pub fn apply_event(&mut self, event: WorkspaceEvent, cx: &mut Context<Self>) {
        match &event {
            WorkspaceEvent::RootChanged(announced) => {
                // A trigger, never an answer: the broadcast carries no caller
                // epoch, so the guarded probe decides.
                if !same_root(self.tree.root.as_ref(), Some(announced)) {
                    self.rearm(cx);
                }
                return;
            }
            WorkspaceEvent::DirectorySnapshot { listing, .. } => self.notify_listing(listing, cx),
            WorkspaceEvent::FileChanged {
                root_token,
                path,
                generation,
                operation_id,
                ..
            } => {
                let notice = FileNotice::Changed {
                    generation: generation.clone(),
                    operation_id: Some(operation_id.clone()).filter(|id| !id.is_empty()),
                };
                self.notify_file(root_token, path, notice, cx);
            }
            WorkspaceEvent::FileDeleted { root_token, path } => {
                self.notify_file(root_token, path, FileNotice::Deleted, cx);
            }
        }
        let Some(root) = self.tree.root.clone() else {
            return;
        };
        let cache_key = self
            .inputs
            .scope
            .as_ref()
            .map(|scope| cache_scope(&scope.client_id, &root));
        match event {
            WorkspaceEvent::DirectorySnapshot {
                root_token,
                listing,
            } if root_token == root.token => {
                if listing.recovered_from_overflow
                    && let Some(key) = &cache_key
                {
                    // The watcher lost events, perhaps below this directory too.
                    self.cache.invalidate_subtree(key, &listing.directory);
                }
                let directory = listing.directory.clone();
                self.apply_listing(&root, &directory, listing, false, false, cx);
            }
            WorkspaceEvent::FileChanged {
                root_token,
                path,
                entry,
                ..
            } if root_token == root.token => {
                self.apply_precise(&root, &parent_path(&path), cx, |listing| match &entry {
                    Some(entry) => patch_entry(listing, entry),
                    None => Patched::Recover(super::listing::Recovery::Unmappable),
                });
            }
            WorkspaceEvent::FileDeleted { root_token, path } if root_token == root.token => {
                if let Some(key) = &cache_key {
                    // A path recreated later is a different directory.
                    self.cache.invalidate_subtree(key, &path);
                }
                // Not the root: its expansion is what makes the tree exist.
                if path != root.path {
                    self.tree.prune_subtree(&path);
                }
                let below = format!("{path}/");
                self.abort_listing(|candidate| candidate == path || candidate.starts_with(&below));
                self.apply_precise(&root, &parent_path(&path), cx, |listing| {
                    remove_entry(listing, &path)
                });
            }
            // Another root's, or the announcement handled above.
            _ => return,
        }
        self.changed(cx);
    }

    /// Applies one precise change to the listing that owns it, or owes that
    /// directory one coalesced re-read. A directory the tree is not showing is
    /// not a gap in anything.
    fn apply_precise(
        &mut self,
        root: &ActiveRoot,
        directory: &str,
        cx: &mut Context<Self>,
        patch: impl FnOnce(Option<&DirectoryListing>) -> Patched,
    ) {
        if !self.tree.on_directory(root, directory) {
            return;
        }
        let paint = PaintTicket::start(EXTERNAL_CHANGE_PAINT);
        match patch(self.tree.listings.get(directory).map(|held| &**held)) {
            Patched::Changed(listing) => {
                self.tree
                    .listings
                    .insert(directory.to_owned(), Rc::new(listing));
                // Only a patch that moved something publishes a measurement.
                if paint.is_some() && self.tree.expanded.contains(directory) {
                    crate::perf::counter("explorer.listingPatches", 1);
                    self.note_due(paint);
                }
            }
            Patched::Unchanged => {}
            Patched::Recover(_) => self.coalesce_recovery(root, directory, cx),
        }
    }

    // The active root.

    fn reset_probe(&mut self, key: String, cx: &mut Context<Self>) {
        self.probe = Probe {
            key,
            ..Probe::default()
        };
        if self.probe.key.is_empty() {
            return;
        }
        self.resolve_behind_paint(cx);
        self.probe.backstop = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(ROOT_BACKSTOP).await;
                if this.update(cx, |this, cx| this.backstop_tick(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn backstop_tick(&mut self, cx: &mut Context<Self>) {
        // A window nobody can see checks nothing; it checks once when it is
        // seen again (`set_foreground`).
        if !self.foreground {
            return;
        }
        self.probe.ticks += 1;
        let settled = self.probe.unchanged >= ROOT_STABLE_PROBES;
        if settled && !self.probe.ticks.is_multiple_of(ROOT_SETTLED_MULTIPLIER) {
            return;
        }
        self.resolve_behind_paint(cx);
    }

    /// The window was shown, hidden, activated or deactivated. Port of the
    /// `visibilitychange` listener in `useActiveRoot`: coming back is a rearm.
    pub(super) fn set_foreground(&mut self, foreground: bool, cx: &mut Context<Self>) {
        let returned = foreground && !self.foreground;
        self.foreground = foreground;
        if returned {
            self.rearm(cx);
        }
    }

    /// Something could have moved the root: probe behind the pane's paint.
    pub fn rearm(&mut self, cx: &mut Context<Self>) {
        self.note_activity();
        if self.foreground {
            self.resolve_behind_paint(cx);
        }
    }

    /// A person asked: probe now, ahead of the paint gate. A hidden window
    /// issues nothing, gesture or not.
    pub fn rearm_now(&mut self, cx: &mut Context<Self>) {
        self.note_activity();
        if self.foreground {
            self.resolve(cx);
        }
    }

    /// The user is working, so stop being settled, but ask nothing.
    fn note_activity(&mut self) {
        self.probe.unchanged = 0;
        self.probe.ticks = 0;
    }

    /// The root cascades into listings and watches on the lane the pane's
    /// screen is arriving on, so a probe nobody asked for waits for that
    /// screen. A timeout, never a barrier.
    fn resolve_behind_paint(&mut self, cx: &mut Context<Self>) {
        let Some(scope) = &self.inputs.scope else {
            return;
        };
        let key = format!("{}\0{}", scope.host_profile_id, scope.pane_id);
        // Looked up once this update has settled: the terminal surface mounts
        // and arms the pane in its own observer of the same switch, which may
        // run after this one.
        self.probe.gate = Some(cx.spawn(async move |this, cx| {
            cx.update(|cx| wait_for_pane_paint(&key, PANE_PAINT_TIMEOUT, cx))
                .await;
            this.update(cx, |this, cx| this.resolve(cx)).ok();
        }));
    }

    /// One probe at a time: two in flight would race to install answers whose
    /// order says nothing about which is current.
    fn resolve(&mut self, cx: &mut Context<Self>) {
        if self.probe.resolving.is_some() {
            return;
        }
        let Some(scope) = self
            .inputs
            .scope
            .clone()
            .filter(|scope| scope.key() == self.probe.key)
        else {
            return;
        };
        let operation = operation_id();
        let known = self.tree.root.as_ref().map(|root| root.token.clone());
        let paint = PaintTicket::start(ROOT_PAINT);
        let request = client::request(
            self.transport.clone(),
            scope.client_id.clone(),
            operation.clone(),
            command::resolve_root(&scope, known.as_deref(), &operation),
        );
        self.probe.resolving = Some(cx.spawn(async move |this, cx| {
            let result = request.await.and_then(decode_root);
            this.update(cx, |this, cx| {
                this.probe.resolving = None;
                match result {
                    // Unchanged only when it is also this selection's tree: a
                    // stale one is adopted even when its root matches.
                    Ok(root)
                        if this.paints_selection()
                            && same_root(this.tree.root.as_ref(), Some(&root)) =>
                    {
                        this.probe.unchanged += 1;
                    }
                    Ok(root) => {
                        this.probe.unchanged = 0;
                        this.adopt_root(root);
                        this.note_due(paint);
                    }
                    Err(error) => this.tree.error = Some(error),
                }
                this.changed(cx);
            })
            .ok();
        }));
    }

    /// Installs the root the selection resolved to. The host derives a token
    /// from the server and path alone, so an equal token and revision is the
    /// same worktree and the tree on screen is adopted as it is; anything else
    /// is a different root, and every path the old one authorised goes.
    pub fn adopt_root(&mut self, root: ActiveRoot) {
        let Some(scope) = self.inputs.scope.clone() else {
            return;
        };
        let kept = self
            .tree
            .root
            .as_ref()
            .is_some_and(|held| held.token == root.token && held.revision == root.revision);
        if self.tree.root.as_ref().map(|held| &held.token) != Some(&root.token) {
            self.view.reset_for_root();
        }
        if !kept {
            self.abort_listing(|_| true);
            self.recovery_paints.clear();
            self.cache
                .invalidate_other_roots(&cache_scope(&scope.client_id, &root));
            self.tree.listings.clear();
            self.tree.expanded = HashSet::from([root.path.clone()]);
            // The root's listing arrives with its watch; until then the tree has
            // nothing to draw, and says so.
            self.tree.loading = HashSet::from([root.path.clone()]);
        }
        self.tree.scope_key = self.selection_key.clone();
        self.tree.root = Some(root);
        self.tree.error = None;
    }

    // What a person does.

    pub fn toggle_directory(&mut self, path: &str, cx: &mut Context<Self>) {
        // A row of the tree behind the user, not of the pane in front of them.
        if !self.paints_selection() {
            return;
        }
        let expanding = !self.tree.expanded.contains(path);
        // Activity, not evidence of a `cd`: no probe.
        self.note_activity();
        let cached = match (&self.inputs.scope, &self.tree.root) {
            (Some(scope), Some(root)) if expanding && !self.tree.listings.contains_key(path) => {
                self.cache.get(&cache_scope(&scope.client_id, root), path)
            }
            _ => None,
        };
        if expanding {
            // A newer expansion of the same path replaces the older's span.
            if let Some(ticket) = PaintTicket::start(EXPAND_PAINT) {
                self.expand_paints.insert(path.to_owned(), ticket);
            }
            self.tree.expanded.insert(path.to_owned());
            // A valid cached revisit paints now and revalidates behind the paint.
            if let Some(cached) = cached {
                self.tree.listings.insert(path.to_owned(), cached);
            }
            if !self.tree.listings.contains_key(path) {
                self.tree.loading.insert(path.to_owned());
            }
        } else {
            self.tree.expanded.remove(path);
            self.tree.loading.remove(path);
            let below = format!("{path}/");
            self.abort_listing(|candidate| candidate == path || candidate.starts_with(&below));
        }
        self.changed(cx);
    }

    /// A read a person asked for. Nothing is read against a tree on its way
    /// out, but the root is re-checked, which is how that window closes.
    pub fn refresh(&mut self, directory: Option<String>, cx: &mut Context<Self>) {
        if !self.paints_selection() {
            self.rearm_now(cx);
            return;
        }
        let Some(root) = self.tree.root.clone() else {
            return;
        };
        self.rearm_now(cx);
        let target = directory.unwrap_or_else(|| root.path.clone());
        self.load_directory(&root, &target, None, true, None, cx);
        self.changed(cx);
    }

    pub fn load_more(&mut self, directory: &str, cx: &mut Context<Self>) {
        if !self.paints_selection() {
            return;
        }
        let (Some(root), Some(held)) = (self.tree.root.clone(), self.tree.listings.get(directory))
        else {
            return;
        };
        if let Some(token) = held.next_page_token.clone().filter(|_| !held.complete) {
            self.load_directory(&root, directory, Some(token), false, None, cx);
            self.changed(cx);
        }
    }

    /// One file change, then the directory it touched is read again. A rename
    /// or move takes the open tabs of its files with it.
    pub fn mutate(
        &mut self,
        mutation: FileMutation,
        cx: &mut Context<Self>,
    ) -> Task<Result<(), String>> {
        let (Some(scope), Some(root)) = (self.inputs.scope.clone(), self.tree.root.clone()) else {
            return Task::ready(Err(DISCONNECTED.into()));
        };
        if !self.inputs.can_mutate {
            return Task::ready(Err(DISCONNECTED.into()));
        }
        let operation = operation_id();
        // Not cancellable: a mutation stopped midway leaves its outcome unknown.
        let request = self.transport.request(
            scope.client_id.clone(),
            command::mutate(&root, &mutation, &operation),
        );
        cx.spawn(async move |this, cx| {
            request.await?;
            this.update(cx, |this, cx| {
                if let FileMutation::Rename(relocation) | FileMutation::Move(relocation) = &mutation
                {
                    cx.emit(ExplorerEvent::Relocated {
                        host_profile_id: scope.host_profile_id.clone(),
                        server_identity: scope.server_identity.clone(),
                        root_path: root.path.clone(),
                        source: relocation.path.clone(),
                        destination: relocation.destination.clone(),
                    });
                }
                let directory = match &mutation {
                    FileMutation::CreateFile { parent, .. }
                    | FileMutation::CreateDirectory { parent, .. } => parent.clone(),
                    FileMutation::Delete { path, .. } => parent_of(path, &root.path),
                    FileMutation::Rename(relocation)
                    | FileMutation::Move(relocation)
                    | FileMutation::Duplicate(relocation) => {
                        parent_of(&relocation.path, &root.path)
                    }
                };
                this.refresh(Some(directory), cx);
            })
            .map_err(|error| error.to_string())
        })
    }

    // Downloads.

    fn status(&self, message: impl Into<String>, cx: &mut Context<Self>) {
        cx.emit(ExplorerEvent::Status(message.into()));
    }

    /// Whether a download asked for under `scope` and `root` still applies.
    fn selection_is_current(
        &self,
        scope: &FileScope,
        root: &ActiveRoot,
        origin: DownloadOrigin,
    ) -> bool {
        self.inputs
            .scope
            .as_ref()
            .is_some_and(|current| current.key() == scope.key())
            && (origin != DownloadOrigin::Explorer
                || same_root(self.tree.root.as_ref(), Some(root)))
    }

    /// The first step of a download: the save panel, one at a time.
    pub fn begin_download(
        &mut self,
        root: &ActiveRoot,
        origin: DownloadOrigin,
        cx: &mut Context<Self>,
    ) -> Option<FileScope> {
        if self.picker_open {
            return None;
        }
        let Some(scope) = self
            .inputs
            .scope
            .clone()
            .filter(|scope| self.selection_is_current(scope, root, origin))
        else {
            self.status("Downloads require the active live file workspace.", cx);
            return None;
        };
        self.picker_open = true;
        Some(scope)
    }

    /// The save panel closed: `chosen` is its answer, or why it failed.
    #[allow(clippy::too_many_arguments)]
    pub fn finish_picking(
        &mut self,
        scope: FileScope,
        root: ActiveRoot,
        intent: DownloadIntent,
        origin: DownloadOrigin,
        chosen: Result<Option<String>, String>,
        cx: &mut Context<Self>,
    ) {
        self.picker_open = false;
        let destination = match chosen {
            Err(error) => {
                return self.status(format!("Could not open the save panel: {error}"), cx);
            }
            Ok(None) => return,
            Ok(Some(destination)) => destination,
        };
        if !self.selection_is_current(&scope, &root, origin) {
            return self.status(DOWNLOAD_CANCELLED, cx);
        }
        // The panel showed this exact name, so its own prompt was consent to
        // replace it. A folder archive whose name lacks `.tar` is written
        // somewhere the panel never showed.
        let overwrite = intent.kind == DownloadKind::File || destination.ends_with(".tar");
        self.start_download(scope, root, intent, destination, overwrite, origin, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn start_download(
        &mut self,
        scope: FileScope,
        root: ActiveRoot,
        intent: DownloadIntent,
        destination: String,
        overwrite: bool,
        origin: DownloadOrigin,
        cx: &mut Context<Self>,
    ) {
        let (sender, receiver) = mpsc::channel();
        let started = self.transport.start_download(
            DownloadStart {
                scope: scope.clone(),
                root: root.clone(),
                source: intent.path.clone(),
                destination: destination.clone(),
                folder: intent.kind == DownloadKind::Folder,
                overwrite,
            },
            Arc::new(EventQueue(sender)),
        );
        let still_current = self.selection_is_current(&scope, &root, origin);
        let id = match started {
            Ok(id) if !still_current => {
                let _ = self.transport.cancel_download(id);
                return self.status(DOWNLOAD_CANCELLED, cx);
            }
            Err(_) if !still_current => return self.status(DOWNLOAD_CANCELLED, cx),
            Err(error) => {
                self.downloads.record(DownloadStatus::refused(
                    uuid::Uuid::new_v4().to_string(),
                    scope.transfer_key(),
                    &intent,
                    Some(destination),
                    error.clone(),
                ));
                cx.emit(ExplorerEvent::DownloadChanged);
                self.status(error, cx);
                cx.notify();
                return;
            }
            Ok(id) => id,
        };
        self.downloads
            .record(queued(&id, &scope, &intent, &destination));
        // Events already sent are merged like any later ones, so none weakens
        // what an earlier one said.
        while let Ok(body) = receiver.try_recv() {
            if let EventBody::Json(json) = body {
                self.downloads
                    .record(decode_event(&json, &intent, &destination, &scope));
            }
        }
        let state = self
            .downloads
            .get(&id)
            .map_or(crate::transfers::TransferState::Queued, |item| item.state);
        let banner = format!("Download {}: {}", state_word(state), intent.path);
        self.active_download = Some(ActiveDownload {
            id,
            path: intent.path.clone(),
            banner: banner.clone(),
        });
        self.status(banner, cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(DOWNLOAD_POLL).await;
                let mut statuses = Vec::new();
                let finished = loop {
                    match receiver.try_recv() {
                        Ok(EventBody::Json(json)) => {
                            statuses.push(decode_event(&json, &intent, &destination, &scope));
                        }
                        Ok(EventBody::Raw(_)) => {}
                        Err(mpsc::TryRecvError::Empty) => break false,
                        Err(mpsc::TryRecvError::Disconnected) => break true,
                    }
                };
                let alive = this
                    .update(cx, |this, cx| {
                        if statuses.is_empty() {
                            return;
                        }
                        for status in statuses {
                            this.downloads.record(status);
                        }
                        cx.emit(ExplorerEvent::DownloadChanged);
                        cx.notify();
                    })
                    .is_ok();
                if finished || !alive {
                    break;
                }
            }
        })
        .detach();
        cx.emit(ExplorerEvent::DownloadChanged);
        cx.notify();
    }

    pub fn cancel_download(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Err(error) = self.transport.cancel_download(id.to_owned()) {
            self.status(error, cx);
        }
    }

    pub fn clear_finished_downloads(&mut self, cx: &mut Context<Self>) {
        self.view.open_error = None;
        self.downloads.clear_finished();
        cx.notify();
    }
}

fn recovery_key(root: &ActiveRoot, path: &str) -> String {
    format!("{}\0{path}", root.token)
}

/// The directory a mutation touched, or the root for a top-level path.
fn parent_of(path: &str, root: &str) -> String {
    match path.rfind('/') {
        Some(index) if index > 0 => path[..index].to_owned(),
        _ => root.to_owned(),
    }
}

fn queued(
    id: &str,
    scope: &FileScope,
    intent: &DownloadIntent,
    destination: &str,
) -> DownloadStatus {
    DownloadStatus {
        id: id.to_owned(),
        scope_key: scope.transfer_key(),
        path: intent.path.clone(),
        destination: Some(destination.to_owned()),
        kind: intent.kind,
        state: crate::transfers::TransferState::Queued,
        outcome: None,
        failure_kind: None,
        completed_bytes: 0,
        total_bytes: None,
        bytes_per_second: None,
        eta_seconds: None,
        error: None,
        cleanup_error: None,
        cleanup_status: None,
    }
}

/// `desktop-core`'s download events, queued for the next poll.
struct EventQueue(mpsc::Sender<EventBody>);

impl EventSink for EventQueue {
    fn send(&self, body: EventBody) -> Result<(), String> {
        self.0
            .send(body)
            .map_err(|_| "the Explorer stopped listening to this download".to_owned())
    }
}
