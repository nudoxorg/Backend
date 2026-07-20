//! [`IrView`] — the read model of one package's IR at a channel tip.
//!
//! An `IrView` is the queryable face of the IR plane: the materialized
//! declaration entries ([`crate::apply::PristineIntroTable`]), the body facts
//! attached to each entry ([`crate::body::BodyEmbed`]), and the resolved
//! occurrence frames each entry owns. Reflections ([`crate::reflect`]) and the
//! Trustfall graph adapter read *only* this type — there is no separate
//! occurrence table, no `ApiSurface` artifact, no symbol table.
//!
//! It is a pure value with borrow-friendly accessors returning iterators; build
//! one from a materialized table plus the body / occurrence side channels of a
//! generation. Caches keyed on `(channel_tip, …)` are disposable and rebuilt
//! from an `IrView`; the view itself is never synced.

use rustc_hash::FxHashMap;

use crate::change::{IntroId, PackageLineageId, StableRef};

use crate::apply::{LinkRecord, PristineIntroTable};
use crate::body::BodyEmbed;
use crate::vocab::{Confidence, ReferenceKind, RelSpan};
use crate::wire::OwnedEntryPayload;

// ---------------------------------------------------------------------------
// Occurrence
// ---------------------------------------------------------------------------

/// A resolved reference *from* one entry *to* a target symbol, positioned by a
/// span relative to the owning entry's declaration span start.
///
/// Occurrences are the reference-bearing facts the host resolve ladder writes on
/// the owning entry (the "occ" frames of §5.4). Usages queries read the reverse
/// of these; the graph's `OccTarget` edge follows `target`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occurrence {
    /// The entry that encloses this reference (the occurrence's owner).
    pub owner: IntroId,
    /// The resolved target symbol.
    pub target: StableRef,
    /// The category of reference.
    pub kind: ReferenceKind,
    /// The confidence at which the target was resolved.
    pub confidence: Confidence,
    /// The reference span, relative to the owner entry span start.
    pub rel_span: RelSpan,
}

// ---------------------------------------------------------------------------
// IrView
// ---------------------------------------------------------------------------

/// The read model of one package's IR at a channel tip.
#[derive(Clone, Debug)]
pub struct IrView {
    package: PackageLineageId,
    table: PristineIntroTable,
    bodies: FxHashMap<IntroId, BodyEmbed>,
    /// Occurrences grouped by their owning entry (forward direction).
    occurrences: FxHashMap<IntroId, Vec<Occurrence>>,
}

impl IrView {
    /// Construct an empty view for a package lineage.
    pub fn new(package: PackageLineageId) -> Self {
        Self {
            package,
            table: PristineIntroTable::new(),
            bodies: FxHashMap::default(),
            occurrences: FxHashMap::default(),
        }
    }

    /// Construct a view from an already-materialized declaration table.
    pub fn from_table(package: PackageLineageId, table: PristineIntroTable) -> Self {
        Self {
            package,
            table,
            bodies: FxHashMap::default(),
            occurrences: FxHashMap::default(),
        }
    }

    // -----------------------------------------------------------------------
    // Builders (used by materialize + tests; additive)
    // -----------------------------------------------------------------------

    /// Insert or replace a declaration entry.
    pub fn insert_entry(
        &mut self,
        intro: IntroId,
        payload: OwnedEntryPayload,
        parent: Option<IntroId>,
    ) {
        self.table.insert_live(intro, payload, parent);
    }

    /// Attach a body embed to an entry.
    pub fn insert_body(&mut self, intro: IntroId, body: BodyEmbed) {
        self.bodies.insert(intro, body);
    }

    /// Record an occurrence on its owning entry.
    pub fn insert_occurrence(&mut self, occ: Occurrence) {
        self.occurrences.entry(occ.owner).or_default().push(occ);
    }

    /// Insert a materialized link record.
    pub fn insert_link(&mut self, record: LinkRecord) {
        self.table.insert_link(record);
    }

    // -----------------------------------------------------------------------
    // Read API
    // -----------------------------------------------------------------------

    /// The package lineage this view describes.
    #[inline]
    pub fn package(&self) -> &PackageLineageId {
        &self.package
    }

    /// The underlying declaration table.
    #[inline]
    pub fn table(&self) -> &PristineIntroTable {
        &self.table
    }

    /// Iterate over all live entries (intro + declaration payload).
    pub fn entries(&self) -> impl Iterator<Item = (IntroId, &OwnedEntryPayload)> {
        self.table.live_entries()
    }

    /// Look up an entry's declaration payload.
    #[inline]
    pub fn entry(&self, intro: IntroId) -> Option<&OwnedEntryPayload> {
        self.table.get(intro)
    }

    /// True if the intro is a live entry at this tip.
    #[inline]
    pub fn is_live(&self, intro: IntroId) -> bool {
        self.table.is_live(intro)
    }

    /// The parent of an entry (`None` for roots / unknown).
    #[inline]
    pub fn parent_of(&self, intro: IntroId) -> Option<IntroId> {
        self.table.parent_of(intro)
    }

    /// The direct children of an entry, in ascending intro order.
    ///
    /// Derived from the parent map (there is no stored child list on the view);
    /// callers that need many lookups should build a reverse map once.
    pub fn children_of(&self, parent: IntroId) -> impl Iterator<Item = IntroId> + '_ {
        self.table
            .live_entries()
            .filter_map(move |(intro, _)| (self.table.parent_of(intro) == Some(parent)).then_some(intro))
    }

    /// The body embed for an entry, if one was recorded.
    #[inline]
    pub fn body(&self, intro: IntroId) -> Option<&BodyEmbed> {
        self.bodies.get(&intro)
    }

    /// The occurrences owned by an entry (forward direction).
    pub fn occurrences_of(&self, owner: IntroId) -> impl Iterator<Item = &Occurrence> {
        self.occurrences.get(&owner).into_iter().flatten()
    }

    /// Every occurrence in the view, across all owners.
    pub fn all_occurrences(&self) -> impl Iterator<Item = &Occurrence> {
        self.occurrences.values().flatten()
    }

    /// Every materialized link.
    pub fn links(&self) -> impl Iterator<Item = &LinkRecord> {
        self.table.links()
    }

    /// Number of live entries.
    #[inline]
    pub fn len(&self) -> usize {
        self.table.len()
    }

    /// True if there are no live entries.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }
}
