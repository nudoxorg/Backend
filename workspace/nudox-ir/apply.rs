//! [`PristineIntroTable`] — the materialized IR of one package at a channel tip.
//!
//! This is a pure **container**: a map of live entries keyed by
//! [`nudox_change::IntroId`], plus their parent edges and links. It is *not* a
//! change engine — libpijul (via `nudox-ir-vcs`) owns changes, dependencies,
//! apply, and unrecord. This type is what a `materialize` produces (by reading
//! libpijul's output tree) and what `nudox-ir-archive` seals into a serve
//! snapshot.

use rustc_hash::FxHashMap;

use nudox_change::domain::LinkDomainKey;
use nudox_change::{ChangeId, ContentBlake3, IntroId, StableRef};

use crate::kind::KindDiscriminant;
use crate::wire::OwnedEntryPayload;

// ---------------------------------------------------------------------------
// MaterializedEntry
// ---------------------------------------------------------------------------

/// The materialized state of a single intro.
///
/// A `materialize` populates only [`MaterializedEntry::Live`] entries (a symbol
/// absent from the channel simply has no file); the [`MaterializedEntry::Deleted`]
/// tombstone variant is retained for callers that track deletions explicitly.
///
/// The `Live` variant is large (a full payload) and `Deleted` is small; that is
/// intentional — entries are accessed by reference through the table, never
/// moved by value on hot paths, so boxing would only add indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MaterializedEntry {
    /// The intro is live with this payload.
    Live(OwnedEntryPayload),
    /// The intro was deleted; its last `payload_hash` and the deleting change
    /// are recorded for provenance.
    Deleted {
        last_hash: ContentBlake3,
        deleted_by: ChangeId,
    },
}

impl MaterializedEntry {
    /// True if this entry is currently live.
    #[inline]
    pub fn is_live(&self) -> bool {
        matches!(self, MaterializedEntry::Live(_))
    }

    /// Extract the live payload, or `None`.
    #[inline]
    pub fn as_live(&self) -> Option<&OwnedEntryPayload> {
        match self {
            MaterializedEntry::Live(p) => Some(p),
            MaterializedEntry::Deleted { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// LinkRecord
// ---------------------------------------------------------------------------

/// A materialized undirected link between two stable-ref endpoints.
///
/// `added_by` records the change that introduced the link. Under the
/// libpijul-backed VCS, provenance is owned by libpijul, so a rebuilt table may
/// carry a placeholder here — the field is retained for API compatibility.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LinkRecord {
    pub a: StableRef,
    pub b: StableRef,
    pub kind_a: KindDiscriminant,
    pub kind_b: KindDiscriminant,
    pub added_by: ChangeId,
}

// ---------------------------------------------------------------------------
// PristineIntroTable
// ---------------------------------------------------------------------------

/// The in-memory materialized IR of a single package channel: a map
/// `IntroId → MaterializedEntry`, the links, and the parent edges.
///
/// A pure value — clone it freely. Build it with [`PristineIntroTable::insert_live`]
/// / [`PristineIntroTable::insert_link`]; read it with the accessors below (which
/// `nudox-ir-archive` uses to seal a serve snapshot).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PristineIntroTable {
    map: FxHashMap<IntroId, MaterializedEntry>,
    links: FxHashMap<LinkDomainKey, LinkRecord>,
    parent: FxHashMap<IntroId, Option<IntroId>>,
}

impl PristineIntroTable {
    /// Construct an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    // -----------------------------------------------------------------------
    // Builders
    // -----------------------------------------------------------------------

    /// Insert or replace a live entry.
    pub fn insert_live(&mut self, intro: IntroId, payload: OwnedEntryPayload, parent: Option<IntroId>) {
        self.map.insert(intro, MaterializedEntry::Live(payload));
        self.parent.insert(intro, parent);
    }

    /// Insert a link record, keyed by its undirected domain key.
    pub fn insert_link(&mut self, record: LinkRecord) {
        let key = LinkDomainKey::from_link(&record.a, &record.b, record.kind_a.as_u16(), record.kind_b.as_u16());
        self.links.insert(key, record);
    }

    // -----------------------------------------------------------------------
    // Read API
    // -----------------------------------------------------------------------

    /// Iterate over all currently live intros and their payloads.
    pub fn live_entries(&self) -> impl Iterator<Item = (IntroId, &OwnedEntryPayload)> {
        self.map.iter().filter_map(|(id, e)| e.as_live().map(|p| (*id, p)))
    }

    /// Look up the parent of an intro (`None` if unknown or a root entry).
    pub fn parent_of(&self, intro: IntroId) -> Option<IntroId> {
        self.parent.get(&intro).copied().flatten()
    }

    /// Iterate over all active links.
    pub fn links(&self) -> impl Iterator<Item = &LinkRecord> {
        self.links.values()
    }

    /// Look up any entry (live or deleted) by intro.
    pub fn get(&self, intro: IntroId) -> Option<&MaterializedEntry> {
        self.map.get(&intro)
    }

    /// True if the intro is currently live.
    pub fn is_live(&self, intro: IntroId) -> bool {
        self.map.get(&intro).map(|e| e.is_live()).unwrap_or(false)
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.map.values().filter(|e| e.is_live()).count()
    }

    /// True if there are no live entries.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::KindDiscriminant;
    use crate::wire::{EntryPayloadFlags, FunctionWire, KindWire, ModuleWire, SymbolWire};
    use nudox_change::{EcosystemId, PackageLineageId, PackageName};

    fn payload(name: &str, module: bool) -> OwnedEntryPayload {
        let sym = SymbolWire {
            name: name.into(),
            visibility: 0,
            documentation: None,
            source_path: "src/lib.rs".into(),
            span_start: 0,
            span_end: 1,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
        };
        if module {
            OwnedEntryPayload::sealed(sym, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default())
        } else {
            OwnedEntryPayload::sealed(
                sym,
                KindDiscriminant::Function,
                KindWire::Function(FunctionWire { input_params: Box::new([]), output_params: Box::new([]) }),
                EntryPayloadFlags::default(),
            )
        }
    }

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sref(n: u8) -> StableRef {
        StableRef::new(PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("lib")), intro(n))
    }

    #[test]
    fn insert_and_read_back() {
        let mut t = PristineIntroTable::new();
        t.insert_live(intro(1), payload("root", true), None);
        t.insert_live(intro(2), payload("f", false), Some(intro(1)));
        t.insert_link(LinkRecord {
            a: sref(1),
            b: sref(2),
            kind_a: KindDiscriminant::Module,
            kind_b: KindDiscriminant::Function,
            added_by: ChangeId::from_raw([0u8; 32]),
        });

        assert_eq!(t.len(), 2);
        assert!(t.is_live(intro(1)) && t.is_live(intro(2)));
        assert_eq!(t.parent_of(intro(2)), Some(intro(1)));
        assert_eq!(t.get(intro(2)).and_then(|e| e.as_live()), Some(&payload("f", false)));
        assert_eq!(t.links().count(), 1);
    }

    #[test]
    fn empty_table_is_empty() {
        let t = PristineIntroTable::new();
        assert!(t.is_empty());
        assert_eq!(t.live_entries().count(), 0);
    }
}
