//! Defines relations behavior for `interface-documents`, whose purpose is to project semantic images into one presentation-neutral document model every surface renders.
//! This module owns the relations invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The incoming-link index a wire image does not carry, built once per image in one linear pass.

use compiler_ir::{Confidence, EntityId, LinkKind, LinkTarget, SemanticReader};

/// One relation observed from the target's side.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IncomingLink {
    /// Declaration that points at the target.
    pub from: EntityId,
    /// Canonical link kind.
    pub kind: LinkKind,
    /// Strongest confidence recorded on the link.
    pub confidence: Confidence,
}

/// Every local incoming link in one image, grouped by target.
///
/// The wire image sorts links by their source, so `links_from` is a binary search and there is no
/// incoming direction at all. This index is the only way to answer "what points at this?", and it
/// is built by counting sort: two passes over the links plane and no comparison sort, so the cost
/// is linear in the number of links rather than `L log L`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ReverseLinks {
    offsets: Box<[u32]>,
    rows: Box<[IncomingLink]>,
}

impl ReverseLinks {
    /// Builds the index over every canonical entity's outgoing links.
    #[must_use]
    pub fn build<Reader: SemanticReader + ?Sized>(reader: &Reader) -> Self {
        let bound = reader
            .canonical_entities()
            .map(|entity| entity.id.index().saturating_add(1))
            .max()
            .unwrap_or(0);
        let mut counts = vec![0_u32; bound.saturating_add(1)];
        let mut total = 0_usize;
        for entity in reader.canonical_entities() {
            for (_, link) in reader.links_from(entity.id) {
                if let LinkTarget::Local(target) = link.target
                    && let Some(slot) = counts.get_mut(target.index())
                {
                    *slot = slot.saturating_add(1);
                    total = total.saturating_add(1);
                }
            }
        }
        let mut offsets = vec![0_u32; bound.saturating_add(1)];
        let mut running = 0_u32;
        for (slot, count) in offsets.iter_mut().zip(counts.iter()) {
            *slot = running;
            running = running.saturating_add(*count);
        }
        let mut cursors = offsets.clone();
        let mut rows = vec![
            IncomingLink {
                from: EntityId::new(0),
                kind: LinkKind::Calls,
                confidence: Confidence::Syntactic,
            };
            total
        ];
        for entity in reader.canonical_entities() {
            for (_, link) in reader.links_from(entity.id) {
                let LinkTarget::Local(target) = link.target else {
                    continue;
                };
                let Some(cursor) = cursors.get_mut(target.index()) else {
                    continue;
                };
                let position = usize::try_from(*cursor).unwrap_or(usize::MAX);
                if let Some(slot) = rows.get_mut(position) {
                    *slot = IncomingLink {
                        from: link.from,
                        kind: link.kind,
                        confidence: link.confidence,
                    };
                    *cursor = cursor.saturating_add(1);
                }
            }
        }
        Self {
            offsets: offsets.into_boxed_slice(),
            rows: rows.into_boxed_slice(),
        }
    }

    /// Every link that points at one declaration, in image order.
    #[must_use]
    pub fn incoming(&self, target: EntityId) -> &[IncomingLink] {
        let index = target.index();
        let (Some(start), Some(end)) = (
            self.offsets.get(index).copied(),
            self.offsets.get(index.saturating_add(1)).copied(),
        ) else {
            return &[];
        };
        let start = usize::try_from(start).unwrap_or(usize::MAX);
        let end = usize::try_from(end).unwrap_or(usize::MAX);
        self.rows.get(start..end).unwrap_or_default()
    }

    /// Total indexed incoming links.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the image had no local link at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}
