use std::{collections::HashMap, sync::Arc};

#[derive(Debug, Clone, Default)]
struct LruLinks {
    key: Arc<str>,
    older: Option<Arc<str>>,
    newer: Option<Arc<str>>,
}

#[derive(Debug, Default)]
pub(super) struct Lru {
    links: HashMap<Arc<str>, LruLinks>,
    oldest: Option<Arc<str>>,
    newest: Option<Arc<str>>,
}

impl Lru {
    pub(super) fn touch(&mut self, pane_id: &str) {
        let key = self
            .detach_entry(pane_id)
            .map(|links| links.key)
            .unwrap_or_else(|| Arc::from(pane_id));
        let older = self.newest.take();
        if let Some(older_id) = older.as_ref() {
            self.links
                .get_mut(older_id)
                .expect("LRU tail has links")
                .newer = Some(Arc::clone(&key));
        } else {
            self.oldest = Some(Arc::clone(&key));
        }
        self.links.insert(
            Arc::clone(&key),
            LruLinks {
                key: Arc::clone(&key),
                older,
                newer: None,
            },
        );
        self.newest = Some(key);
    }

    pub(super) fn detach(&mut self, pane_id: &str) {
        self.detach_entry(pane_id);
    }

    fn detach_entry(&mut self, pane_id: &str) -> Option<LruLinks> {
        let links = self.links.remove(pane_id)?;
        if let Some(older) = links.older.as_ref() {
            self.links
                .get_mut(older)
                .expect("LRU predecessor has links")
                .newer = links.newer.clone();
        } else {
            self.oldest.clone_from(&links.newer);
        }
        if let Some(newer) = links.newer.as_ref() {
            self.links
                .get_mut(newer)
                .expect("LRU successor has links")
                .older = links.older.clone();
        } else {
            self.newest.clone_from(&links.older);
        }
        Some(links)
    }

    pub(super) fn pop_oldest(&mut self) -> Option<String> {
        let pane_id = self.oldest.clone()?;
        self.detach_entry(&pane_id);
        Some(pane_id.to_string())
    }
}

#[cfg(test)]
impl Lru {
    pub(super) fn assert_matches(&self, expected: &std::collections::HashSet<String>) {
        let mut visited = std::collections::HashSet::new();
        let mut current = self.oldest.as_deref();
        let mut previous = None;
        while let Some(pane_id) = current {
            assert!(visited.insert(pane_id.to_owned()), "LRU cycle at {pane_id}");
            let links = self.links.get(pane_id).expect("LRU pane has links");
            assert_eq!(links.older.as_deref(), previous);
            previous = Some(pane_id);
            current = links.newer.as_deref();
        }
        assert_eq!(previous, self.newest.as_deref());
        assert_eq!(visited, *expected);
        assert_eq!(visited.len(), self.links.len());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_reuses_its_stable_key_allocation() {
        let mut lru = Lru::default();
        lru.touch("%1");
        let first = Arc::as_ptr(&lru.links.get("%1").unwrap().key);

        lru.touch("%1");

        let second = Arc::as_ptr(&lru.links.get("%1").unwrap().key);
        assert_eq!(first, second);
    }
}
