//! Host directory watches, shared by the tree and the open documents. Port of
//! `watchLeases.ts` and the shared watch registry in `api.ts`.
//!
//! One host watch serves every holder of a directory under a root: the host
//! allows 128 per connection, and a document watching the folder the tree has
//! open must not spend a second one. Its bootstrap listing is produced once,
//! so a holder that joins after it landed is handed a listing of unknown age:
//! the tree paints it and reads the directory again, a document ignores it.

use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
    time::{Duration, Instant},
};

use futures::{StreamExt, channel::mpsc};
use gpui_kit::{Context, Task};
use serde_json::Value;

use crate::editor::{FileNotice, WatchTarget};

use super::{
    client::{self, operation_id},
    explorer::{Explorer, ExplorerEvent, WATCH_RETRY_BASE, WATCH_RETRY_MAX},
    listing::{parent_path, reachable_watch_targets},
    wire::{ActiveRoot, DirectoryListing, FileKind, FileScope, command, decode_listing},
};

/// How many refused directories are remembered once nothing wants them.
const MAX_TRACKED_REFUSALS: usize = 64;

/// Who holds a watch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Holder {
    Tree,
    Document(u64),
}

/// One host watch on a directory under a root.
struct Watch {
    id: u64,
    client_id: String,
    token: String,
    directory: String,
    watch_id: String,
    operation_id: String,
    holders: HashSet<Holder>,
    /// Holders that joined before the bootstrap landed: it answers them too.
    waiting: HashSet<Holder>,
    /// The arming request, until it settles.
    task: Option<Task<()>>,
    bootstrap: Option<Rc<DirectoryListing>>,
}

struct Refusal {
    until: Instant,
    wait: Duration,
}

/// A document's interest in its file, from the editor's watcher.
pub struct DocumentWatch {
    pub host: String,
    pub server: String,
    pub token: String,
    pub root_path: String,
    pub watched: String,
    pub resource: String,
}

/// What a document holds while it lives. Dropped, it gives the watch back;
/// the drop happens on the app's thread, which the channel then wakes.
pub struct DocumentLease {
    id: u64,
    released: mpsc::UnboundedSender<u64>,
}

impl Drop for DocumentLease {
    fn drop(&mut self) {
        let _ = self.released.unbounded_send(self.id);
    }
}

pub struct Watches {
    /// The connection every watch belongs to.
    connection: String,
    held: HashMap<String, Watch>,
    /// Given back while their request was in flight: the unwatch goes once it
    /// settles, as the host registers a watch before it lists.
    retiring: HashMap<u64, Watch>,
    pub(super) documents: HashMap<u64, DocumentWatch>,
    /// The holders last synced, so a sync runs once per change.
    desired: Vec<(String, Holder)>,
    /// The tree's reachable directories, in tree order.
    pub(super) tree: Vec<String>,
    refused: HashMap<String, Refusal>,
    retry: Option<(Instant, Task<()>)>,
    released: mpsc::UnboundedSender<u64>,
    _released: Task<()>,
}

fn watch_key(token: &str, directory: &str) -> String {
    format!("{token}\0{directory}")
}

/// What a listing says about one file: its generation, or nothing for a
/// symlink (rewriting its target does not move it) or a name it omits
/// (absence proves nothing; only a delete event retires a tab).
fn opinion(listing: &DirectoryListing, resource: &str) -> Option<String> {
    listing
        .entries
        .iter()
        .find(|entry| entry.path == resource)
        .filter(|entry| entry.kind != FileKind::Symlink)
        .map(|entry| entry.generation.clone())
}

impl Watches {
    pub fn new(cx: &mut Context<Explorer>) -> Self {
        let (released, mut leases) = mpsc::unbounded();
        let task = cx.spawn(async move |this, cx| {
            while let Some(id) = leases.next().await {
                if this
                    .update(cx, |this, cx| this.release_document(id, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            connection: String::new(),
            held: HashMap::new(),
            retiring: HashMap::new(),
            documents: HashMap::new(),
            desired: Vec::new(),
            tree: Vec::new(),
            refused: HashMap::new(),
            retry: None,
            released,
            _released: task,
        }
    }

    /// The directories the tree holds a watch on.
    #[cfg(test)]
    pub fn watched(&self) -> Vec<String> {
        let mut held: Vec<String> = self
            .held
            .values()
            .filter(|watch| watch.holders.contains(&Holder::Tree))
            .map(|watch| watch.directory.clone())
            .collect();
        held.sort();
        held
    }
}

impl Explorer {
    /// A document's watch, from the editor's watcher.
    pub fn watch_document(
        &mut self,
        target: &WatchTarget,
        cx: &mut Context<Self>,
    ) -> DocumentLease {
        let id = self.next_id();
        self.watches.documents.insert(
            id,
            DocumentWatch {
                host: target.host.clone(),
                server: target.server.clone(),
                token: target.root.token.clone(),
                root_path: target.root.path.clone(),
                watched: target.watched.clone(),
                resource: target.resource.clone(),
            },
        );
        self.sync_watches(false, cx);
        DocumentLease {
            id,
            released: self.watches.released.clone(),
        }
    }

    fn release_document(&mut self, id: u64, cx: &mut Context<Self>) {
        if self.watches.documents.remove(&id).is_some() {
            self.sync_watches(false, cx);
        }
    }

    /// The watch set follows the reachable expanded directories of this
    /// selection's own root, withheld while the tree is stale, and the files
    /// of the documents open on this host.
    pub(super) fn sync_watches(&mut self, force: bool, cx: &mut Context<Self>) {
        let connection = self
            .inputs
            .scope
            .as_ref()
            .map(FileScope::transfer_key)
            .unwrap_or_default();
        if connection != self.watches.connection {
            self.release_all();
            self.watches.connection = connection;
        }
        let Some(scope) = self.inputs.scope.clone() else {
            self.watches.tree.clear();
            return;
        };
        // (key, holder, token, root path, directory), the tree first and in
        // tree order.
        let mut wanted: Vec<(String, Holder, String, String, String)> = Vec::new();
        self.watches.tree = match self.active_root() {
            Some(root) => {
                reachable_watch_targets(&root.path, &self.tree.listings, &self.tree.expanded)
            }
            None => Vec::new(),
        };
        if let Some(root) = self.active_root() {
            for directory in &self.watches.tree {
                wanted.push((
                    watch_key(&root.token, directory),
                    Holder::Tree,
                    root.token.clone(),
                    root.path.clone(),
                    directory.clone(),
                ));
            }
        }
        let mut documents: Vec<(&u64, &DocumentWatch)> = self.watches.documents.iter().collect();
        documents.sort_by_key(|(id, _)| **id);
        for (id, document) in documents {
            if document.host == scope.host_profile_id && document.server == scope.server_identity {
                wanted.push((
                    watch_key(&document.token, &document.watched),
                    Holder::Document(*id),
                    document.token.clone(),
                    document.root_path.clone(),
                    document.watched.clone(),
                ));
            }
        }
        let mut signature: Vec<(String, Holder)> = wanted
            .iter()
            .map(|(key, holder, ..)| (key.clone(), *holder))
            .collect();
        signature.sort();
        if !force && signature == self.watches.desired {
            return;
        }
        self.watches.desired = signature.clone();
        let wants = |key: &str, holder: &Holder| {
            signature
                .binary_search_by(|(k, h)| (k.as_str(), h).cmp(&(key, holder)))
                .is_ok()
        };
        let mut abandoned = Vec::new();
        for (key, watch) in &mut self.watches.held {
            watch.holders.retain(|holder| wants(key, holder));
            watch.waiting.retain(|holder| wants(key, holder));
            if watch.holders.is_empty() {
                abandoned.push(key.clone());
            }
        }
        for key in abandoned {
            self.retire(&key);
        }
        let now = cx.background_executor().now();
        // Refusals outlive a close and reopen, or collapsing and expanding would
        // dodge the backoff. They are bounded instead, and expire.
        self.watches
            .refused
            .retain(|key, refusal| now < refusal.until || signature.iter().any(|(k, _)| k == key));
        while self.watches.refused.len() > MAX_TRACKED_REFUSALS {
            let oldest = self
                .watches
                .refused
                .iter()
                .min_by_key(|(_, refusal)| refusal.until)
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                self.watches.refused.remove(&oldest);
            }
        }
        let mut soonest: Option<Instant> = None;
        for (key, holder, token, root_path, directory) in wanted {
            if let Some(watch) = self.watches.held.get_mut(&key) {
                if !watch.holders.insert(holder) {
                    continue;
                }
                match watch.bootstrap.clone() {
                    None => {
                        watch.waiting.insert(holder);
                    }
                    Some(listing) => self.bootstrap(holder, &directory, &listing, false, cx),
                }
                continue;
            }
            if let Some(refusal) = self.watches.refused.get(&key).filter(|r| now < r.until) {
                soonest = Some(soonest.map_or(refusal.until, |at| at.min(refusal.until)));
                if holder == Holder::Tree {
                    self.deferred(&directory, cx);
                }
                continue;
            }
            self.acquire(&scope, key, holder, token, root_path, directory, cx);
        }
        if let Some(at) = soonest {
            self.schedule_retry(at.saturating_duration_since(now), cx);
        }
    }

    /// A tree directory still inside its backoff gets its contents another
    /// way, but not while that read is already running: a fresh list per sync
    /// would cancel the one in flight.
    fn deferred(&mut self, directory: &str, cx: &mut Context<Self>) {
        let Some(root) = self.active_root().cloned() else {
            return;
        };
        if self.tree.listings.contains_key(directory)
            || self.reading(directory, super::explorer::ReadKind::List)
        {
            return;
        }
        self.load_directory(&root, directory, None, false, None, cx);
    }

    #[allow(clippy::too_many_arguments)]
    fn acquire(
        &mut self,
        scope: &FileScope,
        key: String,
        holder: Holder,
        token: String,
        root_path: String,
        directory: String,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_id();
        let (watch_id, operation) = (operation_id(), operation_id());
        let root = ActiveRoot {
            token: token.clone(),
            path: root_path,
            ..ActiveRoot::default()
        };
        let request = client::request(
            self.transport(),
            scope.client_id.clone(),
            operation.clone(),
            command::watch(&root, &directory, &watch_id, &operation),
        );
        let answered = key.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = request.await;
            this.update(cx, |this, cx| this.watch_answered(answered, id, result, cx))
                .ok();
        });
        self.watches.held.insert(
            key,
            Watch {
                id,
                client_id: scope.client_id.clone(),
                token,
                directory,
                watch_id,
                operation_id: operation,
                holders: HashSet::from([holder]),
                waiting: HashSet::from([holder]),
                task: Some(task),
                bootstrap: None,
            },
        );
    }

    fn watch_answered(
        &mut self,
        key: String,
        id: u64,
        result: Result<Value, String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(retired) = self.watches.retiring.remove(&id) {
            self.unwatch(&retired);
            return;
        }
        let Some(watch) = self
            .watches
            .held
            .get_mut(&key)
            .filter(|watch| watch.id == id)
        else {
            return;
        };
        watch.task = None;
        let bootstrap = result.and_then(|value| {
            decode_listing(value, &watch.token)
                .map_err(|_| "Host omitted the watch bootstrap snapshot.".to_owned())
        });
        match bootstrap {
            Ok(listing) => {
                let listing = Rc::new(listing);
                watch.bootstrap = Some(listing.clone());
                let mut waiting: Vec<Holder> = watch.waiting.drain().collect();
                waiting.sort();
                let directory = watch.directory.clone();
                self.watches.refused.remove(&key);
                for holder in waiting {
                    self.bootstrap(holder, &directory, &listing, true, cx);
                }
            }
            Err(error) => {
                // Given back even though it failed: the host registers a watch
                // before it lists, so a timeout leaves one it still counts.
                let Some(watch) = self.watches.held.remove(&key) else {
                    return;
                };
                self.unwatch(&watch);
                let wait = self
                    .watches
                    .refused
                    .get(&key)
                    .map_or(WATCH_RETRY_BASE, |refusal| refusal.wait * 2)
                    .min(WATCH_RETRY_MAX);
                // The clock at the moment of refusal, not of asking.
                let now = cx.background_executor().now();
                self.watches.refused.insert(
                    key,
                    Refusal {
                        until: now + wait,
                        wait,
                    },
                );
                self.schedule_retry(wait, cx);
                // A watch that could not be armed must not also mean a folder
                // with no contents: the bootstrap was its listing.
                if watch.holders.contains(&Holder::Tree)
                    && let Some(root) = self
                        .active_root()
                        .cloned()
                        .filter(|r| r.token == watch.token)
                {
                    self.load_directory(&root, &watch.directory, None, false, Some(error), cx);
                }
            }
        }
        self.changed(cx);
    }

    /// Hands a holder the watch's listing. `fresh` is whether it is this
    /// holder's own answer rather than one produced for an earlier holder.
    fn bootstrap(
        &mut self,
        holder: Holder,
        directory: &str,
        listing: &Rc<DirectoryListing>,
        fresh: bool,
        cx: &mut Context<Self>,
    ) {
        match holder {
            Holder::Tree => {
                let Some(root) = self
                    .active_root()
                    .cloned()
                    .filter(|root| root.token == listing.root_token)
                else {
                    return;
                };
                if fresh {
                    self.apply_listing(&root, directory, (**listing).clone(), false, false, cx);
                    self.prefetch_next_page(&root, directory, listing, cx);
                    return;
                }
                // Another holder's answer, of unknown age: painted, then read.
                if !self.tree.listings.contains_key(directory) {
                    self.apply_listing(&root, directory, (**listing).clone(), false, false, cx);
                }
                self.load_directory(&root, directory, None, false, None, cx);
            }
            Holder::Document(id) => {
                let Some(document) = self.watches.documents.get(&id) else {
                    return;
                };
                cx.emit(ExplorerEvent::Notice {
                    host: document.host.clone(),
                    root_token: document.token.clone(),
                    path: document.resource.clone(),
                    notice: FileNotice::Listed {
                        generation: opinion(listing, &document.resource),
                        bootstrap: true,
                        fresh,
                    },
                });
            }
        }
    }

    /// The documents an event from the active host can concern, once per
    /// file: a notice reaches every tab showing it.
    fn concerned<'a>(
        &'a self,
        matches: impl Fn(&DocumentWatch) -> bool + 'a,
    ) -> Vec<&'a DocumentWatch> {
        let host = self.inputs.scope.as_ref().map(|scope| {
            (
                scope.host_profile_id.as_str(),
                scope.server_identity.as_str(),
            )
        });
        let mut seen = HashSet::new();
        let mut documents: Vec<(&u64, &DocumentWatch)> = self.watches.documents.iter().collect();
        documents.sort_by_key(|(id, _)| **id);
        documents
            .into_iter()
            .map(|(_, document)| document)
            .filter(|document| host == Some((document.host.as_str(), document.server.as_str())))
            .filter(|document| matches(document))
            .filter(|document| seen.insert((&document.host, &document.token, &document.resource)))
            .collect()
    }

    /// An authoritative listing of a document's parent directory, which can
    /// say whether that file moved. A terminal file capability watches the
    /// file itself, but its rescans still describe the parent.
    pub(super) fn notify_listing(&mut self, listing: &DirectoryListing, cx: &mut Context<Self>) {
        let notices: Vec<ExplorerEvent> = self
            .concerned(|document| {
                document.token == listing.root_token
                    && parent_path(&document.resource) == listing.directory
            })
            .into_iter()
            .map(|document| ExplorerEvent::Notice {
                host: document.host.clone(),
                root_token: document.token.clone(),
                path: document.resource.clone(),
                notice: FileNotice::Listed {
                    generation: opinion(listing, &document.resource),
                    bootstrap: false,
                    fresh: true,
                },
            })
            .collect();
        for notice in notices {
            cx.emit(notice);
        }
    }

    /// A precise change to one path, for the documents showing it.
    pub(super) fn notify_file(
        &mut self,
        root_token: &str,
        path: &str,
        notice: FileNotice,
        cx: &mut Context<Self>,
    ) {
        let hosts: Vec<String> = self
            .concerned(|document| document.token == root_token && document.resource == path)
            .into_iter()
            .map(|document| document.host.clone())
            .collect();
        for host in hosts {
            cx.emit(ExplorerEvent::Notice {
                host,
                root_token: root_token.to_owned(),
                path: path.to_owned(),
                notice: notice.clone(),
            });
        }
    }

    /// Re-attempts refused watches when the soonest backoff expires.
    fn schedule_retry(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let delay = delay.max(Duration::from_millis(1));
        let at = cx.background_executor().now() + delay;
        if self
            .watches
            .retry
            .as_ref()
            .is_some_and(|(held, _)| *held <= at)
        {
            return;
        }
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            this.update(cx, |this, cx| {
                this.watches.retry = None;
                if !this.watches.desired.is_empty() {
                    this.sync_watches(true, cx);
                }
            })
            .ok();
        });
        self.watches.retry = Some((at, task));
    }

    /// Stops a watch nobody holds. One still arming is cancelled and given
    /// back once its request settles; a settled one is given back now.
    fn retire(&mut self, key: &str) {
        let Some(watch) = self.watches.held.remove(key) else {
            return;
        };
        if watch.task.is_some() {
            self.transport()
                .cancel(watch.client_id.clone(), watch.operation_id.clone());
            self.watches.retiring.insert(watch.id, watch);
        } else {
            self.unwatch(&watch);
        }
    }

    fn unwatch(&self, watch: &Watch) {
        let operation = operation_id();
        // Sent and forgotten: the transport sends it whether or not it is
        // polled.
        drop(self.transport().request(
            watch.client_id.clone(),
            command::unwatch(&watch.watch_id, &operation),
        ));
    }

    /// Every watch belongs to one connection; a new one starts over.
    fn release_all(&mut self) {
        for key in self.watches.held.keys().cloned().collect::<Vec<_>>() {
            self.retire(&key);
        }
        self.watches.refused.clear();
        self.watches.desired.clear();
        self.watches.tree.clear();
        self.watches.retry = None;
    }
}
