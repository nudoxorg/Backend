//! [`PristineIntroTable`] — the materialized IR of one package at a channel tip.
//!
//! This is a pure **container**: a map of live entries keyed by
//! [`nudox_change::IntroId`], plus their parent edges and links. It is *not* a
//! change engine — libpijul (via `nudox-ir-vcs`) owns changes, dependencies,
//! apply, unrecord, and all provenance (which change introduced or deleted
//! what). A symbol absent from the table simply has no file at the channel
//! tip; there are no tombstones here. This type is what a `materialize`
//! produces (by reading libpijul's output tree) and what `nudox-ir-archive`
//! seals into a serve snapshot.

use rustc_hash::FxHashMap;

use nudox_change::domain::LinkDomainKey;
use nudox_change::{IntroId, StableRef};

use crate::kind::KindDiscriminant;
use crate::wire::OwnedEntryPayload;

// ---------------------------------------------------------------------------
// LinkRecord
// ---------------------------------------------------------------------------

/// A materialized undirected link between two stable-ref endpoints.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LinkRecord {
    pub a: StableRef,
    pub b: StableRef,
    pub kind_a: KindDiscriminant,
    pub kind_b: KindDiscriminant,
}

// ---------------------------------------------------------------------------
// PristineIntroTable
// ---------------------------------------------------------------------------

/// The in-memory materialized IR of a single package channel: a map
/// `IntroId → OwnedEntryPayload`, the links, and the parent edges.
///
/// A pure value — clone it freely. Build it with [`PristineIntroTable::insert_live`]
/// / [`PristineIntroTable::insert_link`]; read it with the accessors below (which
/// `nudox-ir-archive` uses to seal a serve snapshot).
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct PristineIntroTable {
    map: FxHashMap<IntroId, OwnedEntryPayload>,
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
        self.map.insert(intro, payload);
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

    /// Iterate over all live intros and their payloads.
    pub fn live_entries(&self) -> impl Iterator<Item = (IntroId, &OwnedEntryPayload)> {
        self.map.iter().map(|(id, p)| (*id, p))
    }

    /// Look up the parent of an intro (`None` if unknown or a root entry).
    pub fn parent_of(&self, intro: IntroId) -> Option<IntroId> {
        self.parent.get(&intro).copied().flatten()
    }

    /// Iterate over all active links.
    pub fn links(&self) -> impl Iterator<Item = &LinkRecord> {
        self.links.values()
    }

    /// Look up an entry's payload by intro.
    pub fn get(&self, intro: IntroId) -> Option<&OwnedEntryPayload> {
        self.map.get(&intro)
    }

    /// True if the intro is present (live) at this tip.
    pub fn is_live(&self, intro: IntroId) -> bool {
        self.map.contains_key(&intro)
    }

    /// Number of live entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// True if there are no live entries.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kind::KindDiscriminant;
    use crate::symbol::Visibility;
    use crate::wire::{EntryPayloadFlags, FunctionWire, KindWire, ModuleWire, SymbolWire};
    use nudox_change::{EcosystemId, PackageLineageId, PackageName};

    fn payload(name: &str, module: bool) -> OwnedEntryPayload {
        let sym = SymbolWire {
            name: name.into(),
            visibility: Visibility::Public,
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
        });

        assert_eq!(t.len(), 2);
        assert!(t.is_live(intro(1)) && t.is_live(intro(2)));
        assert_eq!(t.parent_of(intro(2)), Some(intro(1)));
        assert_eq!(t.get(intro(2)), Some(&payload("f", false)));
        assert_eq!(t.links().count(), 1);
    }

    #[test]
    fn empty_table_is_empty() {
        let t = PristineIntroTable::new();
        assert!(t.is_empty());
        assert_eq!(t.live_entries().count(), 0);
    }
}
