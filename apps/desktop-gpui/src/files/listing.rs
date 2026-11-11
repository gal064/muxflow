//! The Explorer's tree model: patching listings from precise events, the tree
//! state and its guarded transitions, and the cache of complete listings. Port
//! of `listingModel.ts`, `directoryState.ts` and `directoryCache.ts`.
//!
//! Listings are shared (`Rc`): the tree and the cache hold the same one, and a
//! patch makes a new one rather than copying a 4,096-entry page per cache write.

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet, VecDeque},
    rc::Rc,
};

use super::wire::{ActiveRoot, DirectoryListing, FileEntry, FileKind, same_root};

/// Why a held listing could not answer an event on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recovery {
    Missing,
    Unmappable,
}

/// What a precise event did to a listing.
#[derive(Debug, PartialEq)]
pub enum Patched {
    Changed(DirectoryListing),
    /// Nothing on screen to correct.
    Unchanged,
    Recover(Recovery),
}

pub fn parent_path(path: &str) -> String {
    match path.rfind('/') {
        Some(index) if index > 0 => path[..index].to_owned(),
        _ => "/".to_owned(),
    }
}

/// The host's own order: directories first, then the raw name bytes. Rust
/// compares `str` by its UTF-8 bytes, which is that order exactly.
pub fn compare_entries(left: &FileEntry, right: &FileEntry) -> Ordering {
    let rank = |entry: &FileEntry| u8::from(entry.kind != FileKind::Directory);
    rank(left)
        .cmp(&rank(right))
        .then_with(|| left.name.cmp(&right.name))
}

/// What `to_string_lossy` leaves for bytes that are not UTF-8: a name holding
/// one has a position in the host's raw-byte order this side cannot compute.
const REPLACEMENT: char = '\u{FFFD}';

/// Only a complete listing is worth caching for a later revisit.
pub fn is_patchable(listing: &DirectoryListing) -> bool {
    listing.complete && listing.next_page_token.is_none()
}

/// Whether this listing's rows have an opinion about `entry`. Pages are a
/// prefix of the host's order, so an incomplete listing holds everything up to
/// its last row and nothing after it. Requiring a complete listing instead sent
/// every change in a directory past one page to a full re-read.
fn covers(listing: &DirectoryListing, entry: &FileEntry) -> Result<bool, Recovery> {
    if is_patchable(listing) {
        return Ok(true);
    }
    let Some(last) = listing.entries.last() else {
        return Ok(false);
    };
    if entry.name.contains(REPLACEMENT) || last.name.contains(REPLACEMENT) {
        return Err(Recovery::Unmappable);
    }
    Ok(compare_entries(entry, last) != Ordering::Greater)
}

pub fn patch_entry(listing: Option<&DirectoryListing>, entry: &FileEntry) -> Patched {
    let Some(listing) = listing else {
        return Patched::Recover(Recovery::Missing);
    };
    if let Some(index) = listing
        .entries
        .iter()
        .position(|held| held.path == entry.path)
    {
        let mut patched = listing.clone();
        patched.entries[index] = entry.clone();
        return Patched::Changed(patched);
    }
    match covers(listing, entry) {
        Err(reason) => Patched::Recover(reason),
        Ok(false) => Patched::Unchanged,
        Ok(true) => {
            let mut patched = listing.clone();
            let at = patched
                .entries
                .partition_point(|held| compare_entries(held, entry) != Ordering::Greater);
            patched.entries.insert(at, entry.clone());
            Patched::Changed(patched)
        }
    }
}

pub fn remove_entry(listing: Option<&DirectoryListing>, path: &str) -> Patched {
    let Some(listing) = listing else {
        return Patched::Recover(Recovery::Missing);
    };
    if !listing.entries.iter().any(|entry| entry.path == path) {
        return Patched::Unchanged;
    }
    let mut patched = listing.clone();
    patched.entries.retain(|entry| entry.path != path);
    Patched::Changed(patched)
}

/// A later page appended to an established listing, keeping the earlier rows.
pub fn append_page(previous: &DirectoryListing, page: DirectoryListing) -> DirectoryListing {
    let mut entries = previous.entries.clone();
    let positions: HashMap<String, usize> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.path.clone(), index))
        .collect();
    for entry in page.entries {
        match positions.get(&entry.path) {
            Some(&index) => entries[index] = entry,
            None => entries.push(entry),
        }
    }
    DirectoryListing {
        entries,
        recovered_from_overflow: previous.recovered_from_overflow || page.recovered_from_overflow,
        ..page
    }
}

/// The root plus every expanded directory reachable through expanded,
/// expandable parents: exactly the watches the tree is owed. Expansion is
/// remembered below a collapsed parent, but a watch on rows nobody can see is a
/// host watcher paid for nothing.
pub fn reachable_watch_targets(
    root: &str,
    listings: &HashMap<String, Rc<DirectoryListing>>,
    expanded: &HashSet<String>,
) -> Vec<String> {
    let mut targets = vec![root.to_owned()];
    // A listing that named an ancestor would otherwise be an endless descent.
    let mut seen = HashSet::from([root.to_owned()]);
    let mut index = 0;
    while index < targets.len() {
        let directory = targets[index].clone();
        index += 1;
        // Depth first, in listing order, as the TypeScript walk produced it.
        let mut children = Vec::new();
        for entry in listings.get(&directory).map_or(&[][..], |l| &l.entries) {
            if entry.expandable && expanded.contains(&entry.path) && seen.insert(entry.path.clone())
            {
                children.push(entry.path.clone());
            }
        }
        for (offset, child) in children.into_iter().enumerate() {
            targets.insert(index + offset, child);
        }
    }
    targets
}

/// Revisions are decimal u64 strings, compared only when both parse: one that
/// does not must never order as zero and discard a real listing.
pub fn older_revision(incoming: &DirectoryListing, held: &DirectoryListing) -> bool {
    match (
        incoming.revision.parse::<u64>(),
        held.revision.parse::<u64>(),
    ) {
        (Ok(next), Ok(current)) => next < current,
        _ => false,
    }
}

/// What installing a listing left to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Installed {
    Done,
    /// An authoritative first page would have shortened a directory the user
    /// paged into: the rows stay, and this many are fetched back first.
    Restore(usize),
}

/// Everything the Explorer knows about one selection's tree. Port of
/// `WorkspaceFilesState`, less what the entity tracks itself.
#[derive(Clone, Debug, Default)]
pub struct TreeState {
    /// The selection this tree was resolved for. While it differs from the live
    /// selection, the tree is the previous one's, painted but not acted on.
    pub scope_key: String,
    pub root: Option<ActiveRoot>,
    pub listings: HashMap<String, Rc<DirectoryListing>>,
    pub expanded: HashSet<String>,
    pub loading: HashSet<String>,
    pub error: Option<String>,
}

impl TreeState {
    /// A transition is refused once the root moved — a path under a replaced
    /// capability is a different file — or once the tree stopped showing the
    /// directory.
    pub fn on_directory(&self, root: &ActiveRoot, directory: &str) -> bool {
        same_root(self.root.as_ref(), Some(root)) && self.expanded.contains(directory)
    }

    /// Installs an authoritative listing or a page of one. Producers mint a
    /// revision before a scan and publish after it, so arrival order is not
    /// freshness order.
    pub fn install_listing(
        &mut self,
        directory: &str,
        listing: DirectoryListing,
        append: bool,
        restored: bool,
    ) -> Installed {
        self.loading.remove(directory);
        let held = self.listings.get(directory).cloned();
        if append {
            // A page from a listing that has since been re-listed would put
            // removed rows back and roll the revision backwards.
            let Some(held) = held.filter(|held| held.revision == listing.revision) else {
                return Installed::Done;
            };
            self.listings
                .insert(directory.to_owned(), Rc::new(append_page(&held, listing)));
            self.error = None;
            return Installed::Done;
        }
        if let Some(held) = &held {
            if older_revision(&listing, held) {
                return Installed::Done;
            }
            // A rescan carries only the first page. `restored` is the answer to
            // this guard, so a restore that fell short cannot queue itself again.
            if !restored && !listing.complete && held.entries.len() > listing.entries.len() {
                return Installed::Restore(held.entries.len());
            }
        }
        self.listings.insert(directory.to_owned(), Rc::new(listing));
        self.error = None;
        Installed::Done
    }

    /// A deleted directory takes its whole subtree with it.
    pub fn prune_subtree(&mut self, path: &str) {
        let prefix = format!("{path}/");
        let covered = |candidate: &String| candidate == path || candidate.starts_with(&prefix);
        self.listings.retain(|key, _| !covered(key));
        self.expanded.retain(|key| !covered(key));
        self.loading.retain(|key| !covered(key));
    }
}

/// How many complete listings are retained.
const MAX_CACHED_DIRECTORIES: usize = 256;

/// Complete listings by connection, root capability and path, so a revisit
/// paints at once and revalidates behind the paint. Every key carries the
/// connection and the root, so neither a reconnect nor a new root can paint
/// from another one's answer. Port of `DirectoryListingCache`.
#[derive(Default)]
pub struct ListingCache {
    /// Oldest first.
    entries: VecDeque<(String, Rc<DirectoryListing>)>,
}

/// The connection and root a cached listing belongs to. The root's generation
/// is what the wire carries beside the token; today it adds nothing the token
/// does not already separate.
pub fn cache_scope(client_id: &str, root: &ActiveRoot) -> String {
    format!("{client_id}\0{}\0{}\0", root.token, root.revision)
}

impl ListingCache {
    pub fn get(&mut self, scope: &str, directory: &str) -> Option<Rc<DirectoryListing>> {
        let key = format!("{scope}{directory}");
        let index = self.entries.iter().position(|(held, _)| *held == key)?;
        // Recency, so a working set survives a burst of one-off reads.
        let entry = self.entries.remove(index)?;
        let listing = entry.1.clone();
        self.entries.push_back(entry);
        Some(listing)
    }

    pub fn set(&mut self, scope: &str, root_token: &str, listing: &Rc<DirectoryListing>) {
        if !is_patchable(listing) || listing.root_token != root_token {
            return;
        }
        let key = format!("{scope}{}", listing.directory);
        if let Some(index) = self.entries.iter().position(|(held, _)| *held == key) {
            if Rc::ptr_eq(&self.entries[index].1, listing) {
                return;
            }
            self.entries.remove(index);
        }
        self.entries.push_back((key, listing.clone()));
        while self.entries.len() > MAX_CACHED_DIRECTORIES {
            self.entries.pop_front();
        }
    }

    /// A directory removed and recreated must not paint its previous life.
    pub fn invalidate_subtree(&mut self, scope: &str, path: &str) {
        let removed = format!("{scope}{path}");
        let below = format!("{removed}/");
        self.entries
            .retain(|(key, _)| *key != removed && !key.starts_with(&below));
    }

    /// Drops everything that is not the exact live connection and root.
    pub fn invalidate_other_roots(&mut self, scope: &str) {
        self.entries.retain(|(key, _)| key.starts_with(scope));
    }

    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.entries.len()
    }
}
