//! Only an active, source-bound names-only search owns a revalidation timer.
//! A completed ordinary read does not poll. No timer owns a read-pool worker,
//! resumes a names cursor as full search, or invents a producer publication.

use super::*;
use crate::core::VersionedRoot;
use crate::model::browse::BrowseValue;
use crate::model::pages::PageValue;
use std::time::Duration;

const FIRST_REVALIDATION: Duration = Duration::from_millis(200);
const MAX_REVALIDATION: Duration = Duration::from_secs(2);

pub(super) struct SearchRevalidation {
    generation: Generation,
    root: VersionedRoot,
    receipt: PageRetryReceipt,
    delay: Duration,
    pub(super) task: Option<Task<()>>,
}

pub(super) fn names_only_value(value: &PageValue) -> bool {
    match value {
        PageValue::Search(page) | PageValue::SearchMore(page) => {
            page.coverage.published_names().is_some()
        }
        PageValue::Browse(BrowseValue::Find(find)) => find
            .answers
            .known()
            .is_some_and(|page| page.coverage.published_names().is_some()),
        _ => false,
    }
}

impl DataStore {
    /// Only current admitted rows can ask for another observation. Retained
    /// source bytes and a replacement owner's pages cannot restart this lease.
    pub(super) fn names_search_is_current(&self, key: &PageKey) -> bool {
        let root = self.snapshot.key();
        let serving = self.owner_serving();
        match key {
            PageKey::Search(query) => matches!(
                admit_resource(&self.pages.search(query), root, serving),
                ResourceAdmission::Current(page) if page.coverage.published_names().is_some()
            ),
            PageKey::Browse(key @ crate::model::browse::BrowseKey::Find(_)) => matches!(
                admit_resource(&self.pages.browse(key), root, serving),
                ResourceAdmission::Current(BrowseValue::Find(find))
                    if find.answers.known().is_some_and(|page| page.coverage.published_names().is_some())
            ),
            _ => false,
        }
    }

    pub(super) fn schedule_search_revalidation(
        &mut self,
        key: PageKey,
        generation: Generation,
        names_only: bool,
        cx: &mut Context<Self>,
    ) {
        if !names_only || self.close_committed || !self.names_search_is_current(&key) {
            self.search_revalidations.remove(&key);
            return;
        }
        let Some(receipt) = self.capture_page_retry(&key) else {
            return;
        };
        let delay = self
            .search_revalidations
            .get(&key)
            .map_or(FIRST_REVALIDATION, |previous| {
                previous.delay.saturating_mul(2).min(MAX_REVALIDATION)
            });
        self.search_revalidations.insert(
            key.clone(),
            SearchRevalidation {
                generation,
                root: self.snapshot.key(),
                receipt,
                delay,
                task: None,
            },
        );
        self.arm_search_revalidation(key, generation, cx);
    }

    fn arm_search_revalidation(
        &mut self,
        key: PageKey,
        generation: Generation,
        cx: &mut Context<Self>,
    ) {
        if self.close_paused || self.close_committed || self.pages.inflight(&key).is_some() {
            return;
        }
        let Some(refresh) = self.search_revalidations.get(&key) else {
            return;
        };
        if refresh.generation != generation || refresh.task.is_some() {
            return;
        }
        let delay = refresh.delay;
        let timer_key = key.clone();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |store, cx| {
                store.revalidate_search(&timer_key, generation, cx);
            });
        });
        if let Some(refresh) = self.search_revalidations.get_mut(&key) {
            refresh.task = Some(task);
        }
    }

    /// A timer must still own the exact landed generation, query, source,
    /// owner attachment and visible stamp. A later read always wins.
    fn revalidate_search(&mut self, key: &PageKey, generation: Generation, cx: &mut Context<Self>) {
        let Some(refresh) = self.search_revalidations.get(key) else {
            return;
        };
        if refresh.generation != generation {
            return;
        }
        let admitted = !self.close_paused
            && !self.close_committed
            && refresh.root.same_authority(self.snapshot.key())
            && self.admits_page_retry(&refresh.receipt)
            && self.names_search_is_current(key)
            && self.pages.inflight(key).is_none();
        if !admitted {
            self.search_revalidations.remove(key);
            return;
        }
        // Retry uses ReadRequest::for_key: a fresh initial query replaces the
        // displayed stage. Its old continuation is never passed to full search.
        self.retry(key.clone(), cx);
        if self.pages.inflight(key).is_none() {
            // A lane/admission refusal is terminal, not another polling cycle.
            self.search_revalidations.remove(key);
        }
    }

    pub(super) fn pause_search_revalidations(&mut self, paused: bool, cx: &mut Context<Self>) {
        if paused {
            for refresh in self.search_revalidations.values_mut() {
                refresh.task.take();
            }
        } else {
            let pending = self
                .search_revalidations
                .iter()
                .map(|(key, refresh)| (key.clone(), refresh.generation))
                .collect::<Vec<_>>();
            for (key, generation) in pending {
                self.arm_search_revalidation(key, generation, cx);
            }
        }
    }
}

#[cfg(test)]
#[path = "search_revalidation_tests.rs"]
mod tests;
