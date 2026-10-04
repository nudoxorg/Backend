use super::ids::{ExternalId, ItemKind, TreeEntityId};
use super::language_facts::LanguageExtensionInput;
use super::relations::{Confidence, DocInput, EntityVersion, LinkKind, SourceSpan};
use super::type_model::Visibility;
use crate::ir::{EntityAuthorityFacts, OccurrenceAuthorityFacts, TypeId};

/// Slice-backed entity input. No field owns frontend memory.
#[derive(Clone, Copy)]
pub struct TreeItemInput<'source> {
    /// Declaration name as borrowed bytes, interned by the admitting builder.
    pub name: &'source [u8],
    /// Cross-language declaration category for this row.
    pub kind: ItemKind,
    /// Language-independent visibility, including `Unknown` when unavailable.
    pub visibility: Visibility,
    /// Exact authority availability and containment truth for this row.
    pub authority: EntityAuthorityFacts,
    /// Optional parent in this tree's local entity-ID space.
    pub parent: Option<TreeEntityId>,
    /// Optional type coordinate already interned in the destination builder.
    pub semantic_type: Option<TypeId>,
    /// Child declarations in source or authority-defined member order.
    pub members: &'source [TreeEntityId],
    /// Borrowed documentation fragments in display order.
    pub docs: &'source [DocInput<'source>],
    /// Borrowed attribute spellings, interned while admitting this row.
    pub attributes: &'source [&'source [u8]],
    /// Optional source file and half-open byte range.
    pub source: Option<SourceSpan>,
    /// Optional facts for exactly one language extension profile.
    pub extension: Option<LanguageExtensionInput<'source>>,
}

/// Slice-backed authority-observed link occurrence input.
///
/// Every input becomes exactly one [`LinkOccurrence`]. The owning builder
/// deduplicates only its canonical relation, never these source sites.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TreeLinkInput {
    /// Tree-local entity from which the relation originates.
    pub from: TreeEntityId,
    /// Local tree entity or destination-builder external target.
    pub target: TreeLinkTarget,
    /// Directed graph relation represented by this occurrence.
    pub kind: LinkKind,
    /// Resolution quality observed at this source site.
    pub confidence: Confidence,
    /// Exact authority availability for this occurrence's source site.
    pub authority: OccurrenceAuthorityFacts,
    /// Optional half-open source range for this use site.
    pub source: Option<SourceSpan>,
}

/// Target used by a local borrowed tree before its entity coordinates are rebased.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TreeLinkTarget {
    /// Entity coordinate local to the borrowed tree.
    Local(TreeEntityId),
    /// External-target coordinate already present in the destination builder.
    External(ExternalId),
}

/// An entire frontend tree submitted as borrowed slices in one call.
pub struct BorrowedTree<'source> {
    /// One stable version row for each item, in the same order.
    pub versions: &'source [EntityVersion],
    /// Borrowed declaration rows whose references use tree-local entity IDs.
    pub items: &'source [TreeItemInput<'source>],
    /// Borrowed relation occurrences; repeated sites are preserved.
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
