//! One chat as the host describes it: the client half of the rules in
//! `chat.proto`. Nothing here knows about a window.
//!
//! A [`Chat`] holds the last snapshot and every change since. An event applies
//! only in revision order; a gap, or a rewind to an item this client never
//! loaded, asks for the chat to be opened again. Older pages arrive apart from
//! the events, so [`Chat::page_answer`] holds one back until the events it
//! already includes have arrived, and asks again for one that missed a change.

use desktop_core::connection::chat::{
    ChatCatalog, ChatChange, ChatEvent, ChatItem, ChatResponse, ChatState,
};

/// What an event did to the chat, in terms of the rows a view keeps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing held changed.
    Ignored,
    /// A change was lost, or names an item not loaded: send `OPEN` again.
    Reopen,
    /// Everything held was replaced.
    Reset,
    /// This many items were added at the end.
    Appended(usize),
    /// The item at this index changed.
    Updated(usize),
    /// Every item from this index on was dropped.
    Truncated(usize),
    /// The state or the catalog changed, and no item.
    State,
}

/// What became of an older page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageOutcome {
    /// It answered a request made before the snapshot now held.
    Dropped,
    /// Kept aside until the chat reaches the page's revision.
    Held,
    /// This many items were put before the first.
    Merged(usize),
    /// A change to one of its items was ignored after it was read: ask again.
    AskAgain,
}

/// What a `LOAD_OLDER` request quotes, kept to judge its answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageRequest {
    pub cursor: String,
    /// The snapshot the request was made under.
    snapshot: u64,
}

#[derive(Default)]
pub struct Chat {
    /// A snapshot has arrived. Until then every other event is ignored.
    held: bool,
    revision: u64,
    pub state: ChatState,
    pub catalog: ChatCatalog,
    /// Oldest first.
    pub items: Vec<ChatItem>,
    /// Passed to `LOAD_OLDER`; empty when nothing older can be loaded.
    pub older_cursor: String,
    /// Counts the times everything held was replaced or dropped.
    snapshot: u64,
    /// The revision of the newest `updated` or `delta` ignored because its
    /// item was not loaded.
    missed_at: u64,
    /// A page whose revision this chat has not reached yet.
    aside: Option<ChatResponse>,
}

impl Chat {
    pub fn is_held(&self) -> bool {
        self.held
    }

    pub fn item(&self, id: &str) -> Option<&ChatItem> {
        self.items.iter().find(|item| item.id == id)
    }

    /// Applies one event of this chat's pane.
    pub fn apply(&mut self, event: ChatEvent) -> Outcome {
        let change = match event.change {
            Some(ChatChange::Snapshot(snapshot)) => {
                self.held = true;
                self.revision = snapshot.revision;
                self.state = snapshot.state.unwrap_or_default();
                self.catalog = snapshot.catalog.unwrap_or_default();
                self.items = snapshot.items;
                self.older_cursor = snapshot.older_cursor;
                self.replaced();
                return Outcome::Reset;
            }
            Some(change) if self.held => change,
            _ => return Outcome::Ignored,
        };
        if event.revision <= self.revision {
            return Outcome::Ignored;
        }
        if event.revision != self.revision + 1 {
            return Outcome::Reopen;
        }
        self.revision = event.revision;
        match change {
            ChatChange::Item(item) => match self.position(&item.id) {
                Some(index) => {
                    self.items[index] = item;
                    Outcome::Updated(index)
                }
                None => {
                    self.items.push(item);
                    Outcome::Appended(1)
                }
            },
            ChatChange::Updated(item) => match self.position(&item.id) {
                Some(index) => {
                    self.items[index] = item;
                    Outcome::Updated(index)
                }
                None => self.missed(),
            },
            ChatChange::Delta(delta) => match self.position(&delta.item_id) {
                Some(index) => {
                    self.items[index].text.push_str(&delta.text_append);
                    Outcome::Updated(index)
                }
                None => self.missed(),
            },
            ChatChange::State(state) => {
                self.state = state;
                Outcome::State
            }
            ChatChange::Catalog(catalog) => {
                self.catalog = catalog;
                Outcome::State
            }
            ChatChange::Truncate(truncate) if truncate.after_item_id.is_empty() => {
                self.items.clear();
                self.older_cursor.clear();
                // A page read before this names items that are gone.
                self.replaced();
                Outcome::Truncated(0)
            }
            ChatChange::Truncate(truncate) => match self.position(&truncate.after_item_id) {
                Some(index) => {
                    self.items.truncate(index + 1);
                    Outcome::Truncated(index + 1)
                }
                None => Outcome::Reopen,
            },
            ChatChange::Snapshot(_) => unreachable!("handled above"),
        }
    }

    fn position(&self, id: &str) -> Option<usize> {
        // Changes land on the newest items.
        self.items.iter().rposition(|item| item.id == id)
    }

    fn missed(&mut self) -> Outcome {
        self.missed_at = self.revision;
        Outcome::Ignored
    }

    fn replaced(&mut self) {
        self.snapshot += 1;
        self.missed_at = 0;
        self.aside = None;
    }

    /// What to ask `LOAD_OLDER` with, when there is something older.
    pub fn page_request(&self) -> Option<PageRequest> {
        (self.held && !self.older_cursor.is_empty()).then(|| PageRequest {
            cursor: self.older_cursor.clone(),
            snapshot: self.snapshot,
        })
    }

    /// Takes the answer to `request`.
    pub fn page_answer(&mut self, request: &PageRequest, response: ChatResponse) -> PageOutcome {
        if request.snapshot != self.snapshot || request.cursor != self.older_cursor {
            return PageOutcome::Dropped;
        }
        if response.revision > self.revision {
            self.aside = Some(response);
            return PageOutcome::Held;
        }
        self.merge(response)
    }

    /// The page kept aside, once the chat has reached its revision. Called
    /// after every applied event.
    pub fn page_due(&mut self) -> Option<PageOutcome> {
        let due = self
            .aside
            .as_ref()
            .is_some_and(|page| page.revision <= self.revision);
        due.then(|| self.aside.take().map(|page| self.merge(page)))
            .flatten()
    }

    fn merge(&mut self, page: ChatResponse) -> PageOutcome {
        if self.missed_at > page.revision {
            return PageOutcome::AskAgain;
        }
        let mut older: Vec<ChatItem> = page
            .older
            .into_iter()
            .filter(|item| self.position(&item.id).is_none())
            .collect();
        let count = older.len();
        older.append(&mut self.items);
        self.items = older;
        self.older_cursor = page.older_cursor;
        PageOutcome::Merged(count)
    }
}
