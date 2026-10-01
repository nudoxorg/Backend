//! Persistent first-wins definitions with source ownership for tail reconciliation.
use super::document_storage::SourceSnapshot;
use super::node::LinkMark;
use gpui::SharedString;
use std::sync::Arc;
use sum_tree::TreeMap;

#[derive(Debug)]
struct Reference {
    mark: Arc<LinkMark>,
    offset: usize,
    end_offset: usize,
    #[cfg(test)]
    copies: Arc<ReferenceWork>,
}
impl Clone for Reference {
    fn clone(&self) -> Self {
        #[cfg(test)]
        self.copies
            .entry_clones
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            mark: self.mark.clone(),
            offset: self.offset,
            end_offset: self.end_offset,
            #[cfg(test)]
            copies: self.copies.clone(),
        }
    }
}
impl PartialEq for Reference {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset
            && self.end_offset == other.end_offset
            && self.mark == other.mark
    }
}

#[derive(Debug)]
pub(crate) struct ReferenceEnvironment {
    by_id: TreeMap<SharedString, Reference>,
    definitions_by_offset: TreeMap<(usize, SharedString), usize>,
    original_winners: TreeMap<SharedString, Option<Arc<LinkMark>>>,
    tracking_update: bool,
    #[cfg(test)]
    work: Arc<ReferenceWork>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct ReferenceWork {
    entry_clones: std::sync::atomic::AtomicUsize,
    environment_clones: std::sync::atomic::AtomicUsize,
    lookups: std::sync::atomic::AtomicUsize,
}

impl Default for ReferenceEnvironment {
    fn default() -> Self {
        Self {
            by_id: TreeMap::default(),
            definitions_by_offset: TreeMap::default(),
            original_winners: TreeMap::default(),
            tracking_update: false,
            #[cfg(test)]
            work: Arc::default(),
        }
    }
}

impl Clone for ReferenceEnvironment {
    fn clone(&self) -> Self {
        #[cfg(test)]
        self.work
            .environment_clones
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            by_id: self.by_id.clone(),
            definitions_by_offset: self.definitions_by_offset.clone(),
            original_winners: self.original_winners.clone(),
            tracking_update: self.tracking_update,
            #[cfg(test)]
            work: self.work.clone(),
        }
    }
}
impl PartialEq for ReferenceEnvironment {
    fn eq(&self, other: &Self) -> bool {
        self.by_id == other.by_id && self.definitions_by_offset == other.definitions_by_offset
    }
}
impl ReferenceEnvironment {
    pub(crate) fn get(&self, id: &SharedString) -> Option<&LinkMark> {
        #[cfg(test)]
        self.work
            .lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.by_id.get(id).map(|entry| entry.mark.as_ref())
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
    pub(crate) fn begin_update(&mut self) {
        self.original_winners.clear();
        self.tracking_update = true;
    }

    /// Return whether the first-wins mapping changed semantically during this
    /// parser pass, then drop the bounded before-image used for that check.
    pub(crate) fn finish_update(&mut self) -> bool {
        let changed = self.tracking_update
            && self.original_winners.iter().any(|(id, original)| {
                original.as_ref() != self.by_id.get(id).map(|reference| &reference.mark)
            });
        self.original_winners.clear();
        self.tracking_update = false;
        changed
    }

    fn record_original_winner(&mut self, id: &SharedString) {
        if self.tracking_update && !self.original_winners.contains_key(id) {
            self.original_winners.insert(
                id.clone(),
                self.by_id.get(id).map(|reference| reference.mark.clone()),
            );
        }
    }
    pub(crate) fn add(
        &mut self,
        id: SharedString,
        mark: LinkMark,
        offset: usize,
        end_offset: usize,
    ) {
        self.definitions_by_offset
            .insert((offset, id.clone()), end_offset);
        let replaces_winner = self
            .by_id
            .get(&id)
            .is_none_or(|reference| offset < reference.offset);
        if replaces_winner {
            self.record_original_winner(&id);
            self.by_id.insert(
                id,
                Reference {
                    mark: Arc::new(mark),
                    offset,
                    end_offset,
                    #[cfg(test)]
                    copies: self.work.clone(),
                },
            );
        }
    }

    /// The final definition can still be extended by the next append when the
    /// only source after it is horizontal whitespace and at most one line
    /// ending. Reparse it through the real Markdown grammar; do not guess how
    /// a destination or a multiline title is tokenized.
    pub(crate) fn open_tail_definition_start(&self, source: &SourceSnapshot) -> Option<usize> {
        let ((start, _), end) = self.definitions_by_offset.last()?;
        source
            .reference_definition_can_continue(*end)
            .then_some(*start)
    }

    pub(crate) fn truncate_from(&mut self, offset: usize) {
        let start = (offset, SharedString::default());
        let removed: Vec<_> = self
            .definitions_by_offset
            .iter_from(&start)
            .map(|(key, _end)| key.clone())
            .collect();
        for (definition_offset, id) in removed {
            self.definitions_by_offset
                .remove(&(definition_offset, id.clone()));
            if self
                .by_id
                .get(&id)
                .is_some_and(|reference| reference.offset == definition_offset)
            {
                self.record_original_winner(&id);
                self.by_id.remove(&id);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn copied_entries(&self) -> usize {
        self.work
            .entry_clones
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn environment_clones(&self) -> usize {
        self.work
            .environment_clones
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn lookups(&self) -> usize {
        self.work.lookups.load(std::sync::atomic::Ordering::Relaxed)
    }
}
