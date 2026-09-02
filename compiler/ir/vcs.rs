//! Zero-copy version control over the canonical IR itself.
//!
//! There is no lowered VCS model, archive payload, or raise step here. A
//! snapshot is an identity plus `&Ir`; entity and link deltas merge the IR's
//! existing stable-order indices and borrow every result from the two images.

use core::{cmp::Ordering, fmt, iter::Peekable};

use crate::{Ir, ItemIdIter, ItemView, Link, LinkId, LinkKind, LinkTarget, StableEntityId};

/// Content identity of one complete immutable IR generation.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GenerationId([u8; 32]);

impl GenerationId {
    #[must_use]
    pub const fn from_raw(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    #[must_use]
    pub fn from_canonical_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// One VCS generation borrowing the same IR used by renderers and graph queries.
#[derive(Clone, Copy)]
pub struct Snapshot<'ir> {
    pub generation: GenerationId,
    pub ir: &'ir Ir,
}

impl fmt::Debug for Snapshot<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Snapshot")
            .field("generation", &self.generation)
            .field("entities", &self.ir.items().len())
            .finish_non_exhaustive()
    }
}

/// Classification of one stable declaration across two generations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityChangeKind {
    Introduced,
    Deleted,
    PayloadChanged,
    Moved,
    PayloadChangedAndMoved,
}

/// One allocation-free declaration delta.
#[derive(Clone, Copy)]
pub struct EntityChange<'before, 'after> {
    pub stable: StableEntityId,
    pub kind: EntityChangeKind,
    pub before: Option<ItemView<'before>>,
    pub after: Option<ItemView<'after>>,
}

impl fmt::Debug for EntityChange<'_, '_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EntityChange")
            .field("stable", &self.stable)
            .field("kind", &self.kind)
            .field("before", &self.before.map(ItemView::id))
            .field("after", &self.after.map(ItemView::id))
            .finish()
    }
}

/// Merge iterator over the two canonical stable-identity entity indices.
pub struct EntityChanges<'before, 'after> {
    before: Snapshot<'before>,
    after: Snapshot<'after>,
    left: Peekable<ItemIdIter<'before>>,
    right: Peekable<ItemIdIter<'after>>,
}

impl<'before, 'after> EntityChanges<'before, 'after> {
    #[must_use]
    pub fn new(before: Snapshot<'before>, after: Snapshot<'after>) -> Self {
        Self {
            before,
            after,
            left: before.ir.canonical_items().peekable(),
            right: after.ir.canonical_items().peekable(),
        }
    }
}

impl<'before, 'after> Iterator for EntityChanges<'before, 'after> {
    type Item = EntityChange<'before, 'after>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let ordering = match (self.left.peek(), self.right.peek()) {
                (Some(left), Some(right)) => left.version().stable.cmp(&right.version().stable),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => return None,
            };
            match ordering {
                Ordering::Less => {
                    let before = self.left.next()?;
                    return Some(EntityChange {
                        stable: before.version().stable,
                        kind: EntityChangeKind::Deleted,
                        before: Some(before),
                        after: None,
                    });
                }
                Ordering::Greater => {
                    let after = self.right.next()?;
                    return Some(EntityChange {
                        stable: after.version().stable,
                        kind: EntityChangeKind::Introduced,
                        before: None,
                        after: Some(after),
                    });
                }
                Ordering::Equal => {
                    let before = self.left.next()?;
                    let after = self.right.next()?;
                    let payload_changed = before.version().payload != after.version().payload;
                    let moved = stable_parent(self.before.ir, before)
                        != stable_parent(self.after.ir, after);
                    let kind = match (payload_changed, moved) {
                        (false, false) => continue,
                        (true, false) => EntityChangeKind::PayloadChanged,
                        (false, true) => EntityChangeKind::Moved,
                        (true, true) => EntityChangeKind::PayloadChangedAndMoved,
                    };
                    return Some(EntityChange {
                        stable: before.version().stable,
                        kind,
                        before: Some(before),
                        after: Some(after),
                    });
                }
            }
        }
    }
}

fn stable_parent(ir: &Ir, item: ItemView<'_>) -> Option<StableEntityId> {
    item.parent()
        .and_then(|parent| ir.version(parent))
        .map(|version| version.stable)
}

/// Canonical identity of a directed graph link.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct StableLinkKey {
    pub from: StableEntityId,
    pub target: StableEntityId,
    pub kind: LinkKind,
}

/// One canonical link carrying a reconstructed register-sized evidence row.
#[derive(Clone, Copy)]
pub struct StableLink<'ir> {
    pub id: LinkId,
    pub key: StableLinkKey,
    pub evidence: Link,
    ir: &'ir Ir,
}

impl fmt::Debug for StableLink<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StableLink")
            .field("id", &self.id)
            .field("key", &self.key)
            .field("evidence", &self.evidence)
            .finish()
    }
}

/// Canonical stable-order link iterator.
pub struct StableLinks<'ir> {
    ir: &'ir Ir,
    ids: &'ir [LinkId],
}

impl<'ir> Iterator for StableLinks<'ir> {
    type Item = StableLink<'ir>;
    fn next(&mut self) -> Option<Self::Item> {
        let (id, rest) = self.ids.split_first()?;
        self.ids = rest;
        let evidence = self.ir.link(*id)?;
        Some(StableLink {
            id: *id,
            key: stable_link_key(self.ir, evidence)?,
            evidence,
            ir: self.ir,
        })
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.ids.len(), Some(self.ids.len()))
    }
}
impl ExactSizeIterator for StableLinks<'_> {}
impl core::iter::FusedIterator for StableLinks<'_> {}

impl Ir {
    /// Iterates graph links in the canonical stable order used by IR-VCS.
    #[must_use]
    pub fn stable_links(&self) -> StableLinks<'_> {
        StableLinks {
            ir: self,
            ids: self.canonical_link_ids(),
        }
    }
}

/// Link lifecycle/evidence change classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkChangeKind {
    Added,
    Removed,
    EvidenceChanged,
}

/// One allocation-free stable link delta.
#[derive(Clone, Copy, Debug)]
pub struct LinkChange<'before, 'after> {
    pub key: StableLinkKey,
    pub kind: LinkChangeKind,
    pub before: Option<StableLink<'before>>,
    pub after: Option<StableLink<'after>>,
}

/// Merge iterator over two canonical stable-link indices.
pub struct LinkChanges<'before, 'after> {
    left: Peekable<StableLinks<'before>>,
    right: Peekable<StableLinks<'after>>,
}

impl<'before, 'after> LinkChanges<'before, 'after> {
    #[must_use]
    pub fn new(before: Snapshot<'before>, after: Snapshot<'after>) -> Self {
        Self {
            left: before.ir.stable_links().peekable(),
            right: after.ir.stable_links().peekable(),
        }
    }
}

impl<'before, 'after> Iterator for LinkChanges<'before, 'after> {
    type Item = LinkChange<'before, 'after>;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let ordering = match (self.left.peek(), self.right.peek()) {
                (Some(left), Some(right)) => left.key.cmp(&right.key),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => return None,
            };
            match ordering {
                Ordering::Less => {
                    let before = self.left.next()?;
                    return Some(LinkChange {
                        key: before.key,
                        kind: LinkChangeKind::Removed,
                        before: Some(before),
                        after: None,
                    });
                }
                Ordering::Greater => {
                    let after = self.right.next()?;
                    return Some(LinkChange {
                        key: after.key,
                        kind: LinkChangeKind::Added,
                        before: None,
                        after: Some(after),
                    });
                }
                Ordering::Equal => {
                    let before = self.left.next()?;
                    let after = self.right.next()?;
                    if link_evidence_equal(before, after) {
                        continue;
                    }
                    return Some(LinkChange {
                        key: before.key,
                        kind: LinkChangeKind::EvidenceChanged,
                        before: Some(before),
                        after: Some(after),
                    });
                }
            }
        }
    }
}

fn stable_link_key(ir: &Ir, link: Link) -> Option<StableLinkKey> {
    let from = ir.version(link.from)?.stable;
    let target = match link.target {
        LinkTarget::Local(entity) => ir.version(entity)?.stable,
        LinkTarget::External(external) => ir.external(external)?.stable,
    };
    Some(StableLinkKey {
        from,
        target,
        kind: link.kind,
    })
}

fn link_evidence_equal(before: StableLink<'_>, after: StableLink<'_>) -> bool {
    before.evidence.confidence == after.evidence.confidence
        && match (before.evidence.source, after.evidence.source) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                left.start() == right.start()
                    && left.end() == right.end()
                    && before.ir.atom(left.file()) == after.ir.atom(right.file())
            }
            (None, Some(_)) | (Some(_), None) => false,
        }
}

/// Convenience pair of direct, zero-copy entity and link diff iterators.
pub struct Diff<'before, 'after> {
    pub entities: EntityChanges<'before, 'after>,
    pub links: LinkChanges<'before, 'after>,
}

impl<'before, 'after> Diff<'before, 'after> {
    #[must_use]
    pub fn between(before: Snapshot<'before>, after: Snapshot<'after>) -> Self {
        Self {
            entities: EntityChanges::new(before, after),
            links: LinkChanges::new(before, after),
        }
    }
}
