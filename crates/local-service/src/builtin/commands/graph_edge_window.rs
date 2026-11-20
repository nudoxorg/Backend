//! Bounded ordered selection for a single graph-neighborhood page.
//!
//! The producer scans the admitted source, feeding every candidate. Retained
//! keys are the smallest `limit + 1` strictly after the exclusive cursor key.
//! Repeated keys are grouped, and conflicting evidence is retained as a
//! conflict rather than choosing an arbitrary producer.

use std::collections::BTreeMap;

pub(super) struct EdgeWindow<K, V> {
    after: Option<K>,
    capacity: usize,
    entries: BTreeMap<K, Option<V>>,
}

impl<K: Ord + Clone, V: Eq> EdgeWindow<K, V> {
    pub(super) fn new(after: Option<K>, limit: usize) -> Option<Self> {
        let capacity = limit.checked_add(1)?;
        (limit > 0).then(|| Self { after, capacity, entries: BTreeMap::new() })
    }

    pub(super) fn insert(&mut self, key: K, value: V) {
        if self.after.as_ref().is_some_and(|after| &key <= after) {
            return;
        }
        if let Some(existing) = self.entries.get_mut(&key) {
            if existing.as_ref().is_some_and(|old| old != &value) {
                *existing = None;
            }
            return;
        }
        if self.entries.len() == self.capacity
            && self.entries.last_key_value().is_some_and(|(last, _)| &key >= last)
        {
            return;
        }
        self.entries.insert(key, Some(value));
        if self.entries.len() > self.capacity {
            self.entries.pop_last();
        }
    }

    pub(super) fn finish(mut self) -> (Vec<(K, Option<V>)>, bool) {
        let more = self.entries.len() == self.capacity;
        if more {
            self.entries.pop_last();
        }
        (self.entries.into_iter().collect(), more)
    }
}

#[cfg(test)]
mod tests {
    use super::EdgeWindow;

    #[test]
    fn pages_unsorted_candidates_once_and_keeps_conflicts() {
        let candidates = [(5, 'e'), (1, 'a'), (3, 'c'), (2, 'b'), (4, 'd'), (3, 'x'), (2, 'b')];
        let mut first = EdgeWindow::new(None, 2).unwrap();
        for (key, value) in candidates { first.insert(key, value); }
        let (rows, more) = first.finish();
        assert_eq!(rows, [(1, Some('a')), (2, Some('b'))]);
        assert!(more);
        let mut second = EdgeWindow::new(Some(2), 2).unwrap();
        for (key, value) in candidates { second.insert(key, value); }
        let (rows, more) = second.finish();
        assert_eq!(rows, [(3, None), (4, Some('d'))]);
        assert!(more);
        let mut last = EdgeWindow::new(Some(4), 2).unwrap();
        for (key, value) in candidates { last.insert(key, value); }
        assert_eq!(last.finish(), (vec![(5, Some('e'))], false));
    }
    #[test]
    fn high_fanout_is_bounded_and_continuations_do_not_drop_keys() {
        let mut after = None;
        let mut collected = Vec::new();
        loop {
            let mut window = EdgeWindow::new(after, 37).unwrap();
            for key in (0..5000).rev() {
                window.insert(key, key);
                assert!(window.entries.len() <= 38);
            }
            let (rows, more) = window.finish();
            after = rows.last().map(|(key, _)| *key);
            collected.extend(rows.into_iter().map(|(key, value)| {
                assert_eq!(value, Some(key));
                key
            }));
            if !more { break; }
        }
        assert_eq!(collected, (0..5000).collect::<Vec<_>>());
    }

}
