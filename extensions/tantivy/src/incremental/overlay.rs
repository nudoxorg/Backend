//! Exact lexical delta overlay and reverse term membership.

use super::plan::terms_for;
use crate::delta::DocumentDelta;
use crate::{Binding, Error};
use std::collections::{BTreeMap, BTreeSet};
use std::mem::size_of;
use std::sync::Arc;

/// Exact changed-document overlay over an immutable lexical base.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LexicalOverlay {
    binding: Binding,
    /// Base rows hidden by a delete or replacement.
    tombstones: Arc<[u64]>,
    /// New rows grouped by normalized term.
    additions: Arc<BTreeMap<String, Arc<[u64]>>>,
    /// Reverse membership makes overlay merges touch only affected terms.
    memberships: Arc<BTreeMap<u64, Arc<[String]>>>,
    bytes: usize,
}

impl LexicalOverlay {
    pub(crate) fn from_delta(delta: &DocumentDelta) -> Result<Self, Error> {
        let (tombstones, additions, memberships, bytes) = build_overlay_parts(delta)?;
        Ok(Self {
            binding: delta.binding,
            tombstones: Arc::from(tombstones.into_iter().collect::<Vec<_>>()),
            additions: Arc::new(additions),
            memberships: Arc::new(memberships),
            bytes,
        })
    }

    pub(crate) fn merge_delta(&self, delta: &DocumentDelta) -> Result<Self, Error> {
        if delta.delta.base() != self.binding.root {
            return Err(Error::StaleRoot);
        }
        let mut tombstones = self.tombstones.to_vec();
        let mut additions = (*self.additions).clone();
        let mut memberships = (*self.memberships).clone();

        for change in delta.delta.changes() {
            let id = change.key;
            tombstones.push(id);
            if let Some(old_terms) = memberships.remove(&id) {
                for term in old_terms.iter() {
                    update_posting(&mut additions, term, id, false);
                }
            }
            if let Some(fields) = &change.after {
                let terms = terms_for(fields).collect::<BTreeSet<_>>();
                for term in &terms {
                    update_posting(&mut additions, term, id, true);
                }
                // Retain empty membership as well: an empty document is a
                // visible row for an empty query, while it contributes no
                // term posting.
                memberships.insert(id, Arc::from(terms.into_iter().collect::<Vec<_>>()));
            }
        }
        tombstones.sort_unstable();
        tombstones.dedup();
        let bytes = shared_overlay_bytes(&tombstones, &additions, &memberships)?;
        Ok(Self {
            binding: delta.binding,
            tombstones: Arc::from(tombstones),
            additions: Arc::new(additions),
            memberships: Arc::new(memberships),
            bytes,
        })
    }

    /// Returns the exact target binding represented by this overlay.
    #[must_use]
    pub fn binding(&self) -> Binding {
        self.binding
    }

    /// Returns the hidden base document identities.
    #[must_use]
    pub fn tombstones(&self) -> &[u64] {
        &self.tombstones
    }

    /// Returns additions for one normalized term without allocating.
    #[must_use]
    pub fn additions(&self, term: &str) -> Option<&[u64]> {
        self.additions.get(term).map(AsRef::as_ref)
    }

    /// Returns retained overlay bytes.
    #[must_use]
    pub const fn bytes(&self) -> usize {
        self.bytes
    }

    pub(crate) fn term_count(&self) -> usize {
        self.additions.len()
    }

    pub(crate) fn document_ids(&self) -> impl Iterator<Item = u64> + '_ {
        self.memberships.keys().copied()
    }
}

type OverlayParts = (
    BTreeSet<u64>,
    BTreeMap<String, Arc<[u64]>>,
    BTreeMap<u64, Arc<[String]>>,
    usize,
);

fn build_overlay_parts(delta: &DocumentDelta) -> Result<OverlayParts, Error> {
    let mut tombstones = BTreeSet::new();
    let mut additions: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
    let mut memberships: BTreeMap<u64, BTreeSet<String>> = BTreeMap::new();
    for change in delta.delta.changes() {
        let id = change.key;
        tombstones.insert(id);
        if let Some(fields) = &change.after {
            let terms = terms_for(fields).collect::<BTreeSet<_>>();
            for term in &terms {
                additions.entry(term.clone()).or_default().insert(id);
            }
            memberships.insert(id, terms);
        }
    }
    let bytes = overlay_bytes(&tombstones, &additions, &memberships)?;
    let additions = additions
        .into_iter()
        .map(|(term, ids)| (term, Arc::from(ids.into_iter().collect::<Vec<_>>())))
        .collect();
    let memberships = memberships
        .into_iter()
        .map(|(id, terms)| (id, Arc::from(terms.into_iter().collect::<Vec<_>>())))
        .collect();
    Ok((tombstones, additions, memberships, bytes))
}

fn overlay_bytes(
    tombstones: &BTreeSet<u64>,
    additions: &BTreeMap<String, BTreeSet<u64>>,
    memberships: &BTreeMap<u64, BTreeSet<String>>,
) -> Result<usize, Error> {
    let tombstone_bytes = tombstones
        .len()
        .checked_mul(size_of::<u64>())
        .ok_or(Error::SizeLimit)?;
    let posting_bytes = additions.iter().try_fold(0usize, |bytes, (term, ids)| {
        let ids_bytes = ids
            .len()
            .checked_mul(size_of::<u64>())
            .ok_or(Error::SizeLimit)?;
        bytes
            .checked_add(term.len())
            .and_then(|bytes| bytes.checked_add(ids_bytes))
            .ok_or(Error::SizeLimit)
    })?;
    memberships.iter().try_fold(
        tombstone_bytes
            .checked_add(posting_bytes)
            .ok_or(Error::SizeLimit)?,
        |bytes, (_, terms)| {
            terms.iter().try_fold(
                bytes
                    .checked_add(size_of::<u64>())
                    .ok_or(Error::SizeLimit)?,
                |bytes, term| bytes.checked_add(term.len()).ok_or(Error::SizeLimit),
            )
        },
    )
}

fn update_posting(
    additions: &mut BTreeMap<String, Arc<[u64]>>,
    term: &str,
    id: u64,
    present: bool,
) {
    let mut ids = additions
        .remove(term)
        .map(|ids| ids.to_vec())
        .unwrap_or_default();
    match (present, ids.binary_search(&id)) {
        (true, Err(index)) => ids.insert(index, id),
        (false, Ok(index)) => {
            ids.remove(index);
        }
        _ => {}
    }
    if !ids.is_empty() {
        additions.insert(term.to_owned(), Arc::from(ids));
    }
}

fn shared_overlay_bytes(
    tombstones: &[u64],
    additions: &BTreeMap<String, Arc<[u64]>>,
    memberships: &BTreeMap<u64, Arc<[String]>>,
) -> Result<usize, Error> {
    let tombstone_bytes = tombstones
        .len()
        .checked_mul(size_of::<u64>())
        .ok_or(Error::SizeLimit)?;
    let posting_bytes = additions.iter().try_fold(0usize, |bytes, (term, ids)| {
        let ids_bytes = ids
            .len()
            .checked_mul(size_of::<u64>())
            .ok_or(Error::SizeLimit)?;
        bytes
            .checked_add(term.len())
            .and_then(|bytes| bytes.checked_add(ids_bytes))
            .ok_or(Error::SizeLimit)
    })?;
    memberships.iter().try_fold(
        tombstone_bytes
            .checked_add(posting_bytes)
            .ok_or(Error::SizeLimit)?,
        |bytes, (_, terms)| {
            terms.iter().try_fold(
                bytes
                    .checked_add(size_of::<u64>())
                    .ok_or(Error::SizeLimit)?,
                |bytes, term| bytes.checked_add(term.len()).ok_or(Error::SizeLimit),
            )
        },
    )
}
