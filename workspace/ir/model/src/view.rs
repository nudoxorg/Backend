//! [`IrView`] — the unified read model of one package's IR at a channel tip.
//!
//! `IrView` overlays the materialized declaration table
//! ([`PristineIntroTable`]) with two side maps:
//!
//! * **bodies** — [`BodyEmbed`] values keyed by [`IntroId`], attached after the
//!   apply pass runs.
//! * **occurrences** — per-entry [`Occurrence`] vecs recording resolved
//!   reference facts (the "occ" frames from the resolve ladder).
//!
//! Reflections and query adapters read *only* this type — no separate
//! occurrence table, no `ApiSurface` artifact. The view is a pure value with
//! borrow-friendly accessors; caches keyed on a channel tip are disposable
//! and rebuilt from an `IrView`.

use std::collections::HashMap;

use crate::{
    apply::PristineIntroTable,
    body::BodyEmbed,
    change::{IntroId, PackageLineageId},
    entry::Entry,
    vocab::Occurrence,
};

// ---------------------------------------------------------------------------
// IrView
// ---------------------------------------------------------------------------

/// The read model overlaying the materialized declaration table with bodies
/// and occurrences.
///
/// # Construction
///
/// The primary constructor is [`IrView::with_package`], which binds a
/// [`PackageLineageId`] to the view so that query layers (the graph adapter,
/// the reverse-position index) can convert same-package [`IntroId`]s into
/// [`crate::change::StableRef`]s without an out-of-band argument.
///
/// `IrView::new` is kept for callers that materialise a table first and attach
/// the package identity in a second step; it defaults the identity to an empty
/// placeholder and is appropriate only when the table is empty or the caller
/// does not need `package()` (e.g. pure containment tests).
///
/// After construction, call [`set_body`] and [`add_occurrence`] to populate
/// the side maps. Query via the borrow-friendly accessors; every `&[]` return
/// is cheaply obtained from a `HashMap` miss.
///
/// [`set_body`]: IrView::set_body
/// [`add_occurrence`]: IrView::add_occurrence
#[derive(Debug)]
pub struct IrView {
    /// The stable package lineage this view represents.
    ///
    /// Set at construction time via [`IrView::with_package`].  The graph
    /// adapter and reverse-position index use this to convert same-package
    /// [`IntroId`]s into [`crate::change::StableRef`]s.
    package: PackageLineageId,
    table: PristineIntroTable,
    bodies: HashMap<IntroId, BodyEmbed>,
    /// Occurrences grouped by their owning entry (forward direction).
    occurrences: HashMap<IntroId, Vec<Occurrence>>,
    /// Exact declaration text, sliced once from the package source while the
    /// producer still owns the extracted tree.  Keeping this beside bodies and
    /// occurrences makes source a first-class part of the read model without
    /// forcing documentation queries to reopen files.
    source: HashMap<IntroId, String>,
}

impl IrView {
    /// Construct an `IrView` for `package`, wrapping an already-materialized
    /// declaration table. The body and occurrence maps are empty; populate
    /// them with [`set_body`] and [`add_occurrence`].
    ///
    /// This is the primary constructor.  Use it whenever the package lineage
    /// is known at build time — the graph adapter and reverse-position index
    /// depend on `package()` to emit [`crate::change::StableRef`]s.
    ///
    /// [`set_body`]: IrView::set_body
    /// [`add_occurrence`]: IrView::add_occurrence
    pub fn with_package(package: PackageLineageId, table: PristineIntroTable) -> Self {
        Self {
            package,
            table,
            bodies: HashMap::new(),
            occurrences: HashMap::new(),
            source: HashMap::new(),
        }
    }

    /// Construct an `IrView` wrapping an already-materialized declaration
    /// table, with no package identity set.
    ///
    /// The package lineage is initialised to an empty placeholder
    /// (`""` ecosystem and `""` name).  Callers that need `package()` to
    /// return a meaningful value should use [`IrView::with_package`] instead.
    /// This variant exists for pure containment tests and for callers that
    /// populate the view before they know its lineage.
    ///
    /// [`set_body`]: IrView::set_body
    /// [`add_occurrence`]: IrView::add_occurrence
    pub fn new(table: PristineIntroTable) -> Self {
        use crate::change::{EcosystemId, PackageName};
        Self::with_package(
            PackageLineageId::new(EcosystemId::new(""), PackageName::new("")),
            table,
        )
    }

    // -----------------------------------------------------------------------
    // Package identity
    // -----------------------------------------------------------------------

    /// The package lineage this view represents.
    ///
    /// Used by the graph adapter and reverse-position index to convert
    /// same-package [`IntroId`]s into [`crate::change::StableRef`]s.
    #[inline]
    pub fn package(&self) -> &PackageLineageId {
        &self.package
    }

    // -----------------------------------------------------------------------
    // Declaration table accessors
    // -----------------------------------------------------------------------

    /// The underlying declaration table.
    #[inline]
    pub fn table(&self) -> &PristineIntroTable {
        &self.table
    }

    /// Look up a live entry by its [`IntroId`]. Returns `None` if absent.
    #[inline]
    pub fn entry(&self, intro: IntroId) -> Option<&Entry> {
        self.table.get(intro)
    }

    /// True if the intro is a live entry in the declaration table.
    #[inline]
    pub fn is_live(&self, intro: IntroId) -> bool {
        self.table.contains(intro)
    }

    /// The parent [`IntroId`] of this intro (`None` for roots or unknown).
    #[inline]
    pub fn parent_of(&self, intro: IntroId) -> Option<IntroId> {
        self.table.parent_of(intro)
    }

    /// The direct children of an entry as a slice.
    ///
    /// Returns `&[]` when the intro has no children or is not present.
    #[inline]
    pub fn children_of(&self, intro: IntroId) -> &[IntroId] {
        self.table.children_of(intro)
    }

    /// Iterate over all live intros and their declaration entries in
    /// **unspecified** order — see [`PristineIntroTable::iter`] for why that is
    /// a per-process hash seed and not merely "arbitrary but fixed".
    pub fn entries(&self) -> impl Iterator<Item = (IntroId, &Entry)> {
        self.table.iter()
    }

    /// Iterate over all live intros and their declaration entries in
    /// deterministic [`IntroId`] order.
    ///
    /// Every projection whose output a user can observe (search indexes, ranked
    /// lists, serialized snapshots) must be built from this rather than from
    /// [`entries`](Self::entries).
    pub fn entries_sorted(&self) -> impl Iterator<Item = (IntroId, &Entry)> {
        self.table.iter_sorted()
    }

    // -----------------------------------------------------------------------
    // Body accessors
    // -----------------------------------------------------------------------

    /// Attach (or replace) a body embed for an entry.
    pub fn set_body(&mut self, intro: IntroId, body: BodyEmbed) {
        self.bodies.insert(intro, body);
    }

    /// Borrow the body embed for an entry, if one was recorded.
    #[inline]
    pub fn body(&self, intro: IntroId) -> Option<&BodyEmbed> {
        self.bodies.get(&intro)
    }

    // -----------------------------------------------------------------------
    // Occurrence accessors
    // -----------------------------------------------------------------------

    /// Push an occurrence onto the owning entry's vec.
    pub fn add_occurrence(&mut self, owner: IntroId, occ: Occurrence) {
        self.occurrences.entry(owner).or_default().push(occ);
    }

    /// The occurrences owned by an entry (forward direction).
    ///
    /// Returns `&[]` when the entry has no recorded occurrences.
    pub fn occurrences_of(&self, intro: IntroId) -> &[Occurrence] {
        self.occurrences
            .get(&intro)
            .map_or(&[], Vec::as_slice)
    }

    /// Every occurrence across all owners, yielding `(owner, &Occurrence)`.
    pub fn all_occurrences(&self) -> impl Iterator<Item = (IntroId, &Occurrence)> {
        self.occurrences
            .iter()
            .flat_map(|(owner, occs)| occs.iter().map(move |occ| (*owner, occ)))
    }

    // -----------------------------------------------------------------------
    // Source accessors
    // -----------------------------------------------------------------------

    /// Attach the exact declaration text for an entry.
    pub fn set_source(&mut self, intro: IntroId, source: String) {
        self.source.insert(intro, source);
    }

    /// Borrow the exact declaration text recorded for an entry.
    #[inline]
    pub fn source(&self, intro: IntroId) -> Option<&str> {
        self.source.get(&intro).map(String::as_str)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        apply::PristineIntroTable,
        body::BodyEmbed,
        change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
        kinds::Module,
        test_helpers::{entry, node, sym},
        vocab::{Confidence, Occurrence, ReferenceKind, RelSpan},
    };

    fn intro(byte: u8) -> IntroId {
        IntroId::from_raw([byte; 32])
    }

    fn make_stable_ref() -> StableRef {
        let lineage =
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("view-test"));
        StableRef::new(lineage, intro(0xab))
    }

    fn make_occurrence() -> Occurrence {
        Occurrence::new(
            make_stable_ref(),
            ReferenceKind::FunctionCall,
            Confidence::Oracle,
            RelSpan::new(0, 10),
        )
    }

    #[test]
    fn ir_view_basic_round_trip() {
        // --- Build a small PristineIntroTable ---
        let parent_id = intro(1);
        let child_id = intro(2);

        let mut table = PristineIntroTable::new();
        table.insert_live(parent_id, entry(sym("root"), node::root([]), Module), None);
        table.insert_live(
            child_id,
            entry(sym("child"), node::root([]), Module),
            Some(parent_id),
        );

        let lineage =
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("view-test-pkg"));
        let mut view = IrView::with_package(lineage.clone(), table);

        // --- package accessor ---
        assert_eq!(view.package(), &lineage);

        // --- entry / is_live ---
        assert!(view.entry(parent_id).is_some(), "parent must be live");
        assert_eq!(view.entry(parent_id).unwrap().sym().name, "root");
        assert!(view.entry(child_id).is_some(), "child must be live");
        assert!(!view.is_live(intro(99)), "unknown intro must not be live");

        // --- parent_of / children_of ---
        assert_eq!(view.parent_of(parent_id), None);
        assert_eq!(view.parent_of(child_id), Some(parent_id));
        let kids = view.children_of(parent_id);
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0], child_id);
        assert_eq!(view.children_of(child_id), &[]);

        // --- entries iterator ---
        assert_eq!(view.entries().count(), 2);

        // --- body ---
        assert!(view.body(parent_id).is_none(), "no body yet");
        view.set_body(parent_id, BodyEmbed::Absent);
        assert_eq!(view.body(parent_id), Some(&BodyEmbed::Absent));

        // --- occurrences ---
        assert_eq!(view.occurrences_of(parent_id), &[]);
        let occ = make_occurrence();
        view.add_occurrence(parent_id, occ.clone());
        let stored = view.occurrences_of(parent_id);
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0], occ);

        // --- all_occurrences ---
        let all: Vec<(IntroId, &Occurrence)> = view.all_occurrences().collect();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, parent_id);
        assert_eq!(*all[0].1, occ);

        // --- child has no occurrences ---
        assert_eq!(view.occurrences_of(child_id), &[]);
    }
}
