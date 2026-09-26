//! Zero-copy version control over the canonical IR itself.
//!
//! There is no lowered VCS model, archive payload, or raise step here. A
//! snapshot is an identity plus `&Ir`; entity and link deltas merge the IR's
//! existing stable-order indices and borrow every result from the two images.

use core::{cmp::Ordering, fmt, iter::FusedIterator, iter::Peekable};

use crate::ir::{
    CorePayloadHash, DeclarationFamilyId, DeclarationIdentity, DeclarationLinkTarget, EntityId,
    ExternalTarget, Ir, ItemView, Link, LinkId, LinkTarget, SemanticEntity,
    SemanticReader, VariantFingerprint,
};

mod ir_diff;

pub use ir_diff::{
    Diff, EntityChanges, LinkChange, LinkChangeKind, LinkChanges, StableLink, StableLinkKey,
    StableLinks,
};
use ir_diff::delta;

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

/// One immutable generation exposed through the canonical semantic-reader
/// contract.
///
/// Unlike [`Snapshot`], this view also accepts a validated, borrowed
/// [`crate::ir::SemanticImageView`]. Durable publications can therefore be
/// compared without rebuilding an owned [`Ir`] or introducing a second VCS
/// representation.
pub struct SemanticSnapshot<'reader, Reader: SemanticReader + ?Sized> {
    pub generation: GenerationId,
    pub reader: &'reader Reader,
}

impl<Reader: SemanticReader + ?Sized> Clone for SemanticSnapshot<'_, Reader> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Reader: SemanticReader + ?Sized> Copy for SemanticSnapshot<'_, Reader> {}

impl<Reader: SemanticReader + ?Sized> fmt::Debug for SemanticSnapshot<'_, Reader> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticSnapshot")
            .field("generation", &self.generation)
            .field("entities", &self.reader.canonical_entities().len())
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delta<T> {
    Unchanged,
    Changed { before: T, after: T },
}

#[derive(Clone, Copy)]
pub enum EntityChange<'before, 'after> {
    Introduced {
        identity: DeclarationIdentity,
        after: ItemView<'after>,
    },
    Deleted {
        identity: DeclarationIdentity,
        before: ItemView<'before>,
    },
    Retained {
        family: DeclarationFamilyId,
        before: ItemView<'before>,
        after: ItemView<'after>,
        variant: Delta<VariantFingerprint>,
        core_payload: Delta<CorePayloadHash>,
        parent: Delta<Option<DeclarationIdentity>>,
    },
}

/// One stable declaration delta borrowed from any complete semantic reader.
///
/// Exact composite identities are merged directly. A changed overload
/// fingerprint is consequently represented as one deletion and one
/// introduction; callers comparing package aggregates may conservatively
/// re-pair singleton families without guessing inside ambiguous overload
/// groups.
/// Stable handle to one declaration in a borrowed semantic reader.
///
/// The handle keeps change records small while preserving allocation-free
/// access to the complete declaration row. Its private reader field prevents
/// callers from constructing an identity/id pair that was not admitted by the
/// reader.
pub struct SemanticEntityRef<'reader, Reader: SemanticReader + ?Sized> {
    pub id: EntityId,
    pub identity: DeclarationIdentity,
    reader: &'reader Reader,
}

impl<Reader: SemanticReader + ?Sized> SemanticEntityRef<'_, Reader> {
    fn new(reader: &Reader, entity: SemanticEntity) -> SemanticEntityRef<'_, Reader> {
        SemanticEntityRef {
            id: entity.id,
            identity: entity.version.identity(),
            reader,
        }
    }

    /// Resolves the complete row from the reader that admitted this handle.
    #[must_use]
    pub fn entity(self) -> SemanticEntity {
        self.reader
            .entity(self.id)
            .expect("canonical semantic entity remains addressable")
    }
}

impl<Reader: SemanticReader + ?Sized> Clone for SemanticEntityRef<'_, Reader> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Reader: SemanticReader + ?Sized> Copy for SemanticEntityRef<'_, Reader> {}

impl<Reader: SemanticReader + ?Sized> fmt::Debug for SemanticEntityRef<'_, Reader> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticEntityRef")
            .field("id", &self.id)
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

pub enum SemanticEntityChange<
    'before,
    'after,
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
> {
    Introduced {
        identity: DeclarationIdentity,
        after: SemanticEntityRef<'after, After>,
    },
    Deleted {
        identity: DeclarationIdentity,
        before: SemanticEntityRef<'before, Before>,
    },
    Retained {
        identity: DeclarationIdentity,
        before: SemanticEntityRef<'before, Before>,
        after: SemanticEntityRef<'after, After>,
        core_payload: Delta<CorePayloadHash>,
        parent: Delta<Option<DeclarationIdentity>>,
    },
}

impl<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized> fmt::Debug
    for SemanticEntityChange<'_, '_, Before, After>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Introduced { identity, after } => formatter
                .debug_struct("Introduced")
                .field("identity", identity)
                .field("after", after)
                .finish(),
            Self::Deleted { identity, before } => formatter
                .debug_struct("Deleted")
                .field("identity", identity)
                .field("before", before)
                .finish(),
            Self::Retained {
                identity,
                before,
                after,
                core_payload,
                parent,
            } => formatter
                .debug_struct("Retained")
                .field("identity", identity)
                .field("before", before)
                .field("after", after)
                .field("core_payload", core_payload)
                .field("parent", parent)
                .finish(),
        }
    }
}

/// Allocation-free merge over two canonical reader entity streams.
pub struct SemanticEntityChanges<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> {
    before: &'before Before,
    after: &'after After,
    left: Peekable<Before::CanonicalEntities<'before>>,
    right: Peekable<After::CanonicalEntities<'after>>,
}

impl<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> SemanticEntityChanges<'before, 'after, Before, After>
{
    #[must_use]
    pub fn new(
        before: SemanticSnapshot<'before, Before>,
        after: SemanticSnapshot<'after, After>,
    ) -> Self {
        Self {
            before: before.reader,
            after: after.reader,
            left: before.reader.canonical_entities().peekable(),
            right: after.reader.canonical_entities().peekable(),
        }
    }
}

impl<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> Iterator for SemanticEntityChanges<'before, 'after, Before, After>
{
    type Item = SemanticEntityChange<'before, 'after, Before, After>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let ordering = match (self.left.peek(), self.right.peek()) {
                (Some(left), Some(right)) => left.version.identity().cmp(&right.version.identity()),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => return None,
            };
            match ordering {
                Ordering::Less => {
                    let before = self.left.next()?;
                    return Some(SemanticEntityChange::Deleted {
                        identity: before.version.identity(),
                        before: SemanticEntityRef::new(self.before, before),
                    });
                }
                Ordering::Greater => {
                    let after = self.right.next()?;
                    return Some(SemanticEntityChange::Introduced {
                        identity: after.version.identity(),
                        after: SemanticEntityRef::new(self.after, after),
                    });
                }
                Ordering::Equal => {
                    let before = self.left.next()?;
                    let after = self.right.next()?;
                    let identity = before.version.identity();
                    let core_payload =
                        delta(before.version.core_payload, after.version.core_payload);
                    let parent = delta(
                        semantic_parent(self.before, before),
                        semantic_parent(self.after, after),
                    );
                    if matches!(core_payload, Delta::Unchanged)
                        && matches!(parent, Delta::Unchanged)
                    {
                        continue;
                    }
                    return Some(SemanticEntityChange::Retained {
                        identity,
                        before: SemanticEntityRef::new(self.before, before),
                        after: SemanticEntityRef::new(self.after, after),
                        core_payload,
                        parent,
                    });
                }
            }
        }
    }
}

fn semantic_parent<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    entity: SemanticEntity,
) -> Option<DeclarationIdentity> {
    entity
        .parent
        .and_then(|parent| reader.entity(parent))
        .map(|parent| parent.version.identity())
}

/// One canonical graph relation borrowed from any complete semantic reader.
pub struct SemanticStableLink<'reader, Reader: SemanticReader + ?Sized> {
    pub id: LinkId,
    pub key: StableLinkKey,
    pub evidence: Link,
    reader: &'reader Reader,
}

impl<Reader: SemanticReader + ?Sized> Clone for SemanticStableLink<'_, Reader> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Reader: SemanticReader + ?Sized> Copy for SemanticStableLink<'_, Reader> {}

impl<Reader: SemanticReader + ?Sized> fmt::Debug for SemanticStableLink<'_, Reader> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticStableLink")
            .field("id", &self.id)
            .field("key", &self.key)
            .field("evidence", &self.evidence)
            .finish()
    }
}

/// Allocation-free canonical graph cursor over a complete semantic reader.
pub struct SemanticStableLinks<'reader, Reader: SemanticReader + ?Sized + 'reader> {
    reader: &'reader Reader,
    links: Reader::CanonicalLinks<'reader>,
}

impl<'reader, Reader: SemanticReader + ?Sized> SemanticStableLinks<'reader, Reader> {
    #[must_use]
    pub fn new(snapshot: SemanticSnapshot<'reader, Reader>) -> Self {
        Self {
            reader: snapshot.reader,
            links: snapshot.reader.canonical_links(),
        }
    }
}

impl<'reader, Reader: SemanticReader + ?Sized> Iterator for SemanticStableLinks<'reader, Reader> {
    type Item = SemanticStableLink<'reader, Reader>;

    fn next(&mut self) -> Option<Self::Item> {
        self.links.find_map(|(id, evidence)| {
            semantic_stable_link_key(self.reader, evidence).map(|key| SemanticStableLink {
                id,
                key,
                evidence,
                reader: self.reader,
            })
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.links.size_hint()
    }
}

impl<Reader: SemanticReader + ?Sized> FusedIterator for SemanticStableLinks<'_, Reader> {}

/// One stable graph relation delta borrowed directly from two complete
/// semantic readers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticLinkChangeKind {
    Added,
    Removed,
    EvidenceChanged,
}

pub struct SemanticLinkChange<
    'before,
    'after,
    Before: SemanticReader + ?Sized,
    After: SemanticReader + ?Sized,
> {
    pub key: StableLinkKey,
    pub kind: SemanticLinkChangeKind,
    pub before: Option<SemanticStableLink<'before, Before>>,
    pub after: Option<SemanticStableLink<'after, After>>,
}

impl<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized> fmt::Debug
    for SemanticLinkChange<'_, '_, Before, After>
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticLinkChange")
            .field("key", &self.key)
            .field("kind", &self.kind)
            .field("before", &self.before)
            .field("after", &self.after)
            .finish()
    }
}

/// Allocation-free merge over two canonical semantic-reader graph streams.
pub struct SemanticLinkChanges<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> {
    left: Peekable<SemanticStableLinks<'before, Before>>,
    right: Peekable<SemanticStableLinks<'after, After>>,
}

impl<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> SemanticLinkChanges<'before, 'after, Before, After>
{
    #[must_use]
    pub fn new(
        before: SemanticSnapshot<'before, Before>,
        after: SemanticSnapshot<'after, After>,
    ) -> Self {
        Self {
            left: SemanticStableLinks::new(before).peekable(),
            right: SemanticStableLinks::new(after).peekable(),
        }
    }
}

impl<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> Iterator for SemanticLinkChanges<'before, 'after, Before, After>
{
    type Item = SemanticLinkChange<'before, 'after, Before, After>;

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
                    return Some(SemanticLinkChange {
                        key: before.key,
                        kind: SemanticLinkChangeKind::Removed,
                        before: Some(before),
                        after: None,
                    });
                }
                Ordering::Greater => {
                    let after = self.right.next()?;
                    return Some(SemanticLinkChange {
                        key: after.key,
                        kind: SemanticLinkChangeKind::Added,
                        before: None,
                        after: Some(after),
                    });
                }
                Ordering::Equal => {
                    let before = self.left.next()?;
                    let after = self.right.next()?;
                    if semantic_link_evidence_equal(before, after) {
                        continue;
                    }
                    return Some(SemanticLinkChange {
                        key: before.key,
                        kind: SemanticLinkChangeKind::EvidenceChanged,
                        before: Some(before),
                        after: Some(after),
                    });
                }
            }
        }
    }
}

fn semantic_stable_link_key<Reader: SemanticReader + ?Sized>(
    reader: &Reader,
    link: Link,
) -> Option<StableLinkKey> {
    let from = reader.entity(link.from)?.version.identity();
    let target = match link.target {
        LinkTarget::Local(entity) => {
            DeclarationLinkTarget::Local(reader.entity(entity)?.version.identity())
        }
        LinkTarget::External(external) => match reader.external(external)? {
            ExternalTarget::Stable { target } => DeclarationLinkTarget::Stable(target),
            ExternalTarget::Foreign(target) => DeclarationLinkTarget::Foreign(target.identity),
            ExternalTarget::FragmentEntity { target, .. } => {
                DeclarationLinkTarget::FragmentEntity(target)
            }
        },
    };
    Some(StableLinkKey {
        from,
        target,
        kind: link.kind,
    })
}

fn semantic_link_evidence_equal<Before: SemanticReader + ?Sized, After: SemanticReader + ?Sized>(
    before: SemanticStableLink<'_, Before>,
    after: SemanticStableLink<'_, After>,
) -> bool {
    before.evidence.confidence == after.evidence.confidence
        && match (before.evidence.source, after.evidence.source) {
            (None, None) => true,
            (Some(left), Some(right)) => {
                left.start() == right.start()
                    && left.end() == right.end()
                    && before.reader.atom(left.file()) == after.reader.atom(right.file())
            }
            (None, Some(_)) | (Some(_), None) => false,
        }
}

/// Direct entity and graph diff over any pair of complete semantic readers.
pub struct SemanticDiff<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> {
    pub entities: SemanticEntityChanges<'before, 'after, Before, After>,
    pub links: SemanticLinkChanges<'before, 'after, Before, After>,
}

impl<
    'before,
    'after,
    Before: SemanticReader + ?Sized + 'before,
    After: SemanticReader + ?Sized + 'after,
> SemanticDiff<'before, 'after, Before, After>
{
    #[must_use]
    pub fn between(
        before: SemanticSnapshot<'before, Before>,
        after: SemanticSnapshot<'after, After>,
    ) -> Self {
        Self {
            entities: SemanticEntityChanges::new(before, after),
            links: SemanticLinkChanges::new(before, after),
        }
    }
}
