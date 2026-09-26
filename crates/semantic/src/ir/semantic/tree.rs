use super::error::EntityRange;
use super::ids::{ExternalId, ItemKind, TreeEntityId};
use super::language_facts::LanguageExtensionInput;
use super::relations::{Confidence, DocInput, EntityVersion, LinkKind, SourceSpan};
use super::type_model::Visibility;
use crate::ir::{EntityAuthorityFacts, EntityId, OccurrenceAuthorityFacts, TypeId};

/// Slice-backed entity input. No field owns frontend memory.
#[derive(Clone, Copy)]
pub struct TreeItemInput<'source> {
    pub name: &'source [u8],
    pub kind: ItemKind,
    pub visibility: Visibility,
    /// Exact authority availability and containment truth for this row.
    pub authority: EntityAuthorityFacts,
    pub parent: Option<TreeEntityId>,
    pub semantic_type: Option<TypeId>,
    pub members: &'source [TreeEntityId],
    pub docs: &'source [DocInput<'source>],
    pub attributes: &'source [&'source [u8]],
    pub source: Option<SourceSpan>,
    pub extension: Option<LanguageExtensionInput<'source>>,
}

/// Slice-backed authority-observed link occurrence input.
///
/// Every input becomes exactly one [`LinkOccurrence`]. The owning builder
/// deduplicates only its canonical relation, never these source sites.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeLinkInput {
    pub from: TreeEntityId,
    pub target: TreeLinkTarget,
    pub kind: LinkKind,
    pub confidence: Confidence,
    /// Exact authority availability for this occurrence's source site.
    pub authority: OccurrenceAuthorityFacts,
    pub source: Option<SourceSpan>,
}

/// Target used by a local borrowed tree before its entity coordinates are rebased.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeLinkTarget {
    Local(TreeEntityId),
    External(ExternalId),
}

/// An entire frontend tree submitted as borrowed slices in one call.
pub struct BorrowedTree<'source> {
    pub versions: &'source [EntityVersion],
    pub items: &'source [TreeItemInput<'source>],
    pub links: &'source [TreeLinkInput],
}

/// Statically dispatched frontend view over an existing compiler tree.
///
/// An adapter may synthesize each [`TreeItemInput`] directly from its AST while
/// iterating. It never has to allocate the 120-byte compatibility rows as an
/// intermediate array, and monomorphization removes the adapter itself.
pub trait FrontendTree {
    /// Stable versions in the same order as [`Self::items`].
    fn versions(&self) -> &[EntityVersion];

    /// Re-iterable borrowed entity stream. A second pass is used only to make
    /// exact arena reservations before canonicalization.
    fn items(&self) -> impl ExactSizeIterator<Item = TreeItemInput<'_>>;

    /// Re-iterable local link stream.
    fn links(&self) -> impl ExactSizeIterator<Item = TreeLinkInput>;
}

impl FrontendTree for BorrowedTree<'_> {
    fn versions(&self) -> &[EntityVersion] {
        self.versions
    }

    fn items(&self) -> impl ExactSizeIterator<Item = TreeItemInput<'_>> {
        self.items.iter().map(|item| TreeItemInput {
            name: item.name,
            kind: item.kind,
            visibility: item.visibility,
            authority: item.authority,
            parent: item.parent,
            semantic_type: item.semantic_type,
            members: item.members,
            docs: item.docs,
            attributes: item.attributes,
            source: item.source,
            extension: item.extension,
        })
    }

    fn links(&self) -> impl ExactSizeIterator<Item = TreeLinkInput> {
        self.links.iter().copied()
    }
}
