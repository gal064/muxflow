//! One shared observation of each repository, for however many consumers.
//! Port of `features/git/repositoryStore.ts`.
//!
//! The panel and every diff tab of a repository share one watch: the second
//! consumer pays no round trip at all and sees the current status the moment
//! it acquires. Consumers observe this entity and read [`GitRepositories::state`].
//! A command's authoritative status is reconciled here through
//! [`GitRepositories::accept`], in the one place that owns the repository's
//! state.
//!
//! In-flight diff requests are not shared between consumers, as the Tauri
//! store did: only two consumers of the same file at once could ever join one,
//! and each consumer cancels its own read.

use std::{collections::HashMap, rc::Rc};

use futures::{FutureExt, future::Shared};
use gpui_kit::{Context, Task};
use uuid::Uuid;

use super::{
    client::{self, Transport},
    model::{GitScope, StatusSnapshot, WatchEvent},
};

/// Repositories whose last status is remembered after their last consumer.
const MAX_REMEMBERED_STATUSES: usize = 8;

/// Watch events held while a bootstrap response is still in flight.
const MAX_PENDING_EVENTS: usize = 64;

/// The published state of one repository.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RepositoryState {
    pub status: Option<Rc<StatusSnapshot>>,
    pub loading: bool,
    pub error: Option<String>,
}

struct Entry {
    /// Which acquisition this is: a released entry's late answer must not land
    /// in a new one under the same key.
    id: u64,
    scope: GitScope,
    consumers: usize,
    state: RepositoryState,
    /// The watch this entry holds, once its bootstrap answered.
    watch_id: Option<String>,
    /// The bootstrap in flight. Without it, a refresh issued while a watch
    /// was still pending would open a second watch and leak one of the two.
    bootstrap: Option<u64>,
    /// Events that arrived before the watch answered: the host activates a
    /// watch only after its response is queued, but the two reach the app
    /// independently. Bounded, because a wedged bootstrap must not grow it.
    pending: Vec<WatchEvent>,
    refreshing: Option<Shared<Task<()>>>,
}

pub struct GitRepositories {
    transport: Transport,
    entries: HashMap<String, Entry>,
    remembered: Vec<(String, Rc<StatusSnapshot>)>,
    /// Acquisitions and bootstrap attempts across every entry, so an answer
    /// for an entry since released is never taken by a new one under the same key.
    attempts: u64,
}

impl GitRepositories {
    pub fn new(transport: Transport) -> Self {
        Self {
            transport,
            entries: HashMap::new(),
            remembered: Vec::new(),
            attempts: 0,
        }
    }

    pub fn transport(&self) -> Transport {
        self.transport.clone()
    }

    /// The scope's shared state without acquiring it. A remembered snapshot
    /// paints at once while the watch is re-acquired.
    pub fn state(&self, key: &str) -> RepositoryState {
        if let Some(entry) = self.entries.get(key) {
            return entry.state.clone();
        }
        RepositoryState {
            status: self
                .remembered
                .iter()
                .find(|(remembered, _)| remembered == key)
                .map(|(_, status)| status.clone()),
            loading: true,
            error: None,
        }
    }

    /// One consumer's claim on the scope's observation, until [`Self::release`].
    pub fn acquire(&mut self, scope: &GitScope, cx: &mut Context<Self>) -> String {
        let key = scope.key();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.consumers += 1;
            return key;
        }
        let state = RepositoryState {
            status: self
                .remembered
                .iter()
                .find(|(remembered, _)| *remembered == key)
                .map(|(_, status)| status.clone()),
            loading: true,
            error: None,
        };
        self.attempts += 1;
        self.entries.insert(
            key.clone(),
            Entry {
                id: self.attempts,
                scope: scope.clone(),
                consumers: 1,
                state,
                watch_id: None,
                bootstrap: None,
                pending: Vec::new(),
                refreshing: None,
            },
        );
        self.start(&key, cx);
        key
    }

    /// The last release ends the watch and remembers the status.
    pub fn release(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get_mut(key) else {
            return;
        };
        entry.consumers -= 1;
        if entry.consumers > 0 {
            return;
        }
        let entry = self.entries.remove(key).expect("the entry just read");
        if let Some(watch_id) = entry.watch_id {
            cx.background_executor()
                .spawn(client::unwatch(&self.transport, &entry.scope, watch_id))
                .detach();
        }
        if let Some(status) = entry.state.status {
            self.remembered.retain(|(remembered, _)| remembered != key);
            self.remembered.push((key.to_owned(), status));
            if self.remembered.len() > MAX_REMEMBERED_STATUSES {
                self.remembered.remove(0);
            }
        }
    }

    pub fn scope(&self, key: &str) -> Option<&GitScope> {
        self.entries.get(key).map(|entry| &entry.scope)
    }

    fn start(&mut self, key: &str, cx: &mut Context<Self>) {
        self.attempts += 1;
        let attempt = self.attempts;
        let Some(entry) = self.entries.get_mut(key) else {
            return;
        };
        entry.bootstrap = Some(attempt);
        let watch_id = Uuid::new_v4().to_string();
        let scope = entry.scope.clone();
        let answer = client::watch(self.transport.clone(), scope.clone(), watch_id.clone());
        let key = key.to_owned();
        cx.spawn(async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |this, cx| {
                let current = this
                    .entries
                    .get_mut(&key)
                    .filter(|entry| entry.bootstrap == Some(attempt));
                let Some(entry) = current else {
                    // Released, or replaced by a later attempt: nobody holds it.
                    if answer.is_ok() {
                        cx.background_executor()
                            .spawn(client::unwatch(&this.transport, &scope, watch_id))
                            .detach();
                    }
                    return;
                };
                entry.bootstrap = None;
                match answer {
                    Ok(status) => {
                        entry.watch_id = Some(watch_id.clone());
                        let pending = std::mem::take(&mut entry.pending);
                        this.accept(&key, status, cx);
                        for event in pending
                            .into_iter()
                            .filter(|event| event.watch_id == watch_id)
                        {
                            this.apply(&key, event.update, cx);
                        }
                        this.settle(&key, cx);
                    }
                    Err(error) => {
                        entry.state.loading = false;
                        entry.state.error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// An explicit re-read, which the host answers without its watch cache.
    /// It is also how a failed bootstrap gets its watch back.
    pub fn refresh(&mut self, key: &str, cx: &mut Context<Self>) -> Shared<Task<()>> {
        let Some(entry) = self.entries.get_mut(key) else {
            return Task::ready(()).shared();
        };
        if let Some(refreshing) = &entry.refreshing {
            return refreshing.clone();
        }
        entry.state.loading = true;
        entry.state.error = None;
        let restart = entry.watch_id.is_none() && entry.bootstrap.is_none();
        let id = entry.id;
        let answer = client::status(self.transport.clone(), entry.scope.clone());
        let owned = key.to_owned();
        let task = cx
            .spawn(async move |this, cx| {
                let answer = answer.await;
                this.update(cx, |this, cx| {
                    let Some(entry) = this.entries.get_mut(&owned).filter(|entry| entry.id == id)
                    else {
                        return;
                    };
                    entry.refreshing = None;
                    match answer {
                        Ok(status) => this.accept(&owned, status, cx),
                        Err(error) => {
                            entry.state.loading = false;
                            entry.state.error = Some(error);
                            cx.notify();
                        }
                    }
                })
                .ok();
            })
            .shared();
        if let Some(entry) = self.entries.get_mut(key) {
            entry.refreshing = Some(task.clone());
        }
        if restart {
            self.start(key, cx);
        }
        cx.notify();
        task
    }

    /// A status this repository's state follows, unless it says nothing new.
    /// An older or identical snapshot is still an answer: whatever waited for
    /// it stops waiting.
    pub fn accept(&mut self, key: &str, status: StatusSnapshot, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get_mut(key) else {
            return;
        };
        if let Some(current) = &entry.state.status
            && current.repository.id == status.repository.id
            && (status.generation < current.generation
                || (status.generation == current.generation
                    && status.source_generation == current.source_generation))
        {
            self.settle(key, cx);
            return;
        }
        entry.state = RepositoryState {
            status: Some(Rc::new(status)),
            loading: false,
            error: None,
        };
        cx.notify();
    }

    /// Stops saying it is loading, and clears an error, without a transition.
    fn settle(&mut self, key: &str, cx: &mut Context<Self>) {
        if let Some(entry) = self.entries.get_mut(key)
            && (entry.state.loading || entry.state.error.is_some())
        {
            entry.state.loading = false;
            entry.state.error = None;
            cx.notify();
        }
    }

    fn apply(&mut self, key: &str, update: Result<StatusSnapshot, String>, cx: &mut Context<Self>) {
        match update {
            Ok(status) => self.accept(key, status, cx),
            Err(error) => {
                if let Some(entry) = self.entries.get_mut(key) {
                    entry.state.loading = false;
                    entry.state.error = Some(error);
                    cx.notify();
                }
            }
        }
    }

    /// A watch event from any host. Events for a watch an entry does not own
    /// belong to another scope.
    pub fn handle_event(&mut self, event: WatchEvent, cx: &mut Context<Self>) {
        let keys: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.scope.root_token == event.root_token)
            .map(|(key, _)| key.clone())
            .collect();
        for key in keys {
            let entry = self.entries.get_mut(&key).expect("the key just listed");
            match &entry.watch_id {
                None => {
                    entry.pending.push(event.clone());
                    if entry.pending.len() > MAX_PENDING_EVENTS {
                        entry.pending.remove(0);
                    }
                }
                Some(watch_id) if *watch_id == event.watch_id => {
                    self.apply(&key, event.update.clone(), cx)
                }
                Some(_) => {}
            }
        }
    }
}
