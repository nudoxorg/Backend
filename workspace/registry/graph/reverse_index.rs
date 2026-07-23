//! Reverse-position index — a disposable O(log n) look-up layer over the IR.
//!
//! A [`ReversePositionIndex`] is built from one [`IrView`] and is **never** synced or
//! persisted: it is always re-derived from the view at the same
//! `(channel_tip, schema_version)` key and can be discarded and rebuilt freely.
//!
//! ## What it indexes
//!
//! * **Reverse occurrence postings** (`target → [owner, …]`): built from
//!   [`IrView::all_occurrences`] filtered to `confidence >= Confidence::Index`
//!   (the graph-worthy floor, §vocab).
//!
//! * **Reverse type-ref postings** (`ty_ref → [entry, …]`): built by scanning
//!   [`KindWire::Impl`] (the implemented trait, via `of`) and [`KindWire::Trait`]
//!   (each entry in `supers`). Field-type mentions are left to a later wave (see
//!   the `NOTE` in [`typerefs_of_entry`]).
//!
//! Both posting lists are sorted and deduped for determinism.

use std::collections::BTreeMap;

use ir::change::{IntroId, PackageLineageId, StableRef};
use ir::wire::{KindWire, OwnedEntryPayload, TypeRefWire};
use ir::IrView;

// ---------------------------------------------------------------------------
// SCHEMA_VERSION
// ---------------------------------------------------------------------------

/// Frozen schema version of the reverse-position index format.
///
/// Bump this whenever the posting-list construction rules change so that callers
/// can detect stale cached indices.
pub const SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// ReverseIndexKey
// ---------------------------------------------------------------------------

/// Cache key for a [`ReversePositionIndex`].
///
/// Encodes the identity of the channel tip and the index schema so that a
/// cached index can be invalidated when either changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ReverseIndexKey {
    /// The 32-byte BLAKE3 digest that names the channel tip this index was
    /// built from (i.e. the `GenerationStamp` or `CasKey` of the generation,
    /// treated as an opaque byte array here so this crate need not depend on
    /// the full hash-types hierarchy).
    pub channel_tip: [u8; 32],
    /// The schema version this index was built against; compare with
    /// [`SCHEMA_VERSION`].
    pub schema_version: u32,
}

// ---------------------------------------------------------------------------
// typerefs_of_entry  (shared helper — also used by trustfall_adapter)
// ---------------------------------------------------------------------------

/// Extract the load-bearing type references from one entry payload.
///
/// Returns the [`StableRef`]s that are structurally load-bearing for graph
/// edge construction:
///
/// * For [`KindWire::Impl`]: the implemented trait (`of`, if present).
/// * For [`KindWire::Trait`]: each supertrait in `supers`.
///
/// [`TypeRefWire::Same`] variants are converted to a [`StableRef`] against the
/// `package` of the owning view; [`TypeRefWire::Foreign`] variants are used
/// directly.
///
/// **NOTE:** field-type mentions (`KindWire::Field`, parameter types in
/// `KindWire::Function`, etc.) are intentionally omitted here; they will be
/// added in a later wave once the field-type graph edges are specced.
pub fn typerefs_of_entry(
    payload: &OwnedEntryPayload,
    package: &PackageLineageId,
) -> Vec<StableRef> {
    let mut refs: Vec<StableRef> = Vec::new();

    match &payload.kind {
        KindWire::Impl(impl_wire) => {
            // The implemented trait (e.g. `impl Display for T` → trait is load-bearing).
            if let Some(of) = &impl_wire.of {
                if let Some(sr) = typeref_to_stable(of, package) {
                    refs.push(sr);
                }
            }
        }
        KindWire::Trait(trait_wire) => {
            // Each supertrait (e.g. `trait Foo: Bar + Baz` → Bar and Baz).
            for super_ref in trait_wire.supers.iter() {
                if let Some(sr) = typeref_to_stable(super_ref, package) {
                    refs.push(sr);
                }
            }
        }
        // All other kinds: no load-bearing type refs extracted in this wave.
        _ => {}
    }

    refs
}

/// Convert a [`TypeRefWire`] to a [`StableRef`] within the given package context.
///
/// `Same(intro)` → `StableRef { package: package.clone(), intro }`.
/// `Foreign(sr)` → `sr` directly (already a `StableRef`).
#[inline]
fn typeref_to_stable(tyref: &TypeRefWire, package: &PackageLineageId) -> Option<StableRef> {
    match tyref {
        TypeRefWire::Same(intro) => Some(StableRef::new(package.clone(), *intro)),
        TypeRefWire::Foreign(sr) => Some(sr.clone()),
    }
}

// ---------------------------------------------------------------------------
// ReversePositionIndex
// ---------------------------------------------------------------------------

/// A disposable reverse-position index over one [`IrView`].
///
/// Build once with [`ReversePositionIndex::build`]; cache keyed on
/// `(channel_tip, schema_version)` via the embedded [`ReverseIndexKey`]. Discard
/// and rebuild whenever the channel tip advances or [`SCHEMA_VERSION`] bumps.
///
/// Never persisted, never synced.
#[derive(Debug)]
pub struct ReversePositionIndex {
    /// The cache key this index was built under.
    key: ReverseIndexKey,
    /// Reverse occurrence postings: `target StableRef → [owner IntroId, …]`.
    ///
    /// Populated only from occurrences with `confidence >= Confidence::Index`
    /// (the graph floor). Posting lists are sorted and deduped.
    occ_postings: BTreeMap<StableRef, Vec<IntroId>>,
    /// Reverse type-reference postings: `type StableRef → [entry IntroId, …]`.
    ///
    /// Built from `Impl.of` (trait refs) and `Trait.supers`. Posting lists are
    /// sorted and deduped.
    typeref_postings: BTreeMap<StableRef, Vec<IntroId>>,
}

impl ReversePositionIndex {
    // -----------------------------------------------------------------------
    // Constructor
    // -----------------------------------------------------------------------

    /// Build the reverse-position index from an `IrView`.
    ///
    /// This is a one-shot, single-pass operation.  It iterates
    /// [`IrView::all_occurrences`] once (O(n)) and all live entries once
    /// (O(m)); it then sorts and deduplicates every posting list for
    /// determinism (O(n log n)).
    pub fn build(ir: &IrView, key: ReverseIndexKey) -> Self {
        let mut occ_postings: BTreeMap<StableRef, Vec<IntroId>> = BTreeMap::new();
        let mut typeref_postings: BTreeMap<StableRef, Vec<IntroId>> = BTreeMap::new();

        // -- Occurrence reverse pass (graph-worthy only) --------------------
        for occ in ir.all_occurrences() {
            if occ.confidence.is_graph_worthy() {
                occ_postings.entry(occ.target.clone()).or_default().push(occ.owner);
            }
        }

        // -- Type-ref reverse pass -----------------------------------------
        let package = ir.package();
        for (intro, payload) in ir.entries() {
            let refs = typerefs_of_entry(payload, package);
            for sr in refs {
                typeref_postings.entry(sr).or_default().push(intro);
            }
        }

        // -- Dedup + sort for determinism ----------------------------------
        for list in occ_postings.values_mut() {
            list.sort_unstable();
            list.dedup();
        }
        for list in typeref_postings.values_mut() {
            list.sort_unstable();
            list.dedup();
        }

        Self { key, occ_postings, typeref_postings }
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// The cache key this index was built under.
    #[inline]
    pub fn key(&self) -> &ReverseIndexKey {
        &self.key
    }

    /// The sorted, deduplicated list of owners that hold a graph-worthy
    /// occurrence whose `target` is `target`.
    ///
    /// Returns an empty slice when no entry references `target` at the graph
    /// floor.
    #[inline]
    pub fn usages_of(&self, target: &StableRef) -> &[IntroId] {
        self.occ_postings.get(target).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The sorted, deduplicated list of entries that carry a load-bearing type
    /// reference pointing at `ty` (impl-of or trait-super only in this wave).
    ///
    /// Returns an empty slice when no entry mentions `ty`.
    #[inline]
    pub fn mentions_of(&self, ty: &StableRef) -> &[IntroId] {
        self.typeref_postings.get(ty).map(Vec::as_slice).unwrap_or(&[])
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};
    use ir::view::Occurrence;
    use ir::vocab::{Confidence, ReferenceKind, RelSpan};
    use ir::kind::KindDiscriminant;
    use ir::wire::{
        EntryPayloadFlags, FnSigFlags, FunctionWire, KindWire, ModuleWire, SymbolWire, TraitFlags,
        TraitWire,
    };
    use ir::IrView;

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn eco() -> EcosystemId {
        EcosystemId::new("cargo")
    }

    fn pkg_id() -> PackageLineageId {
        PackageLineageId::new(eco(), PackageName::new("test-pkg"))
    }

    fn intro(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sref_local(n: u8) -> StableRef {
        StableRef::new(pkg_id(), intro(n))
    }

    fn sym(name: &str) -> SymbolWire {
        SymbolWire {
            name: name.into(),
            visibility: ir::symbol::Visibility::Public,
            documentation: None,
            source_path: "src/lib.rs".into(),
            span_start: 0,
            span_end: 10,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: Vec::new(),
            cfg: None,
        }
    }

    fn module_payload(name: &str) -> ir::wire::OwnedEntryPayload {
        ir::wire::OwnedEntryPayload::sealed(
            sym(name),
            KindDiscriminant::Module,
            KindWire::Module(ModuleWire {}),
            EntryPayloadFlags::default(),
        )
    }

    fn fn_payload(name: &str) -> ir::wire::OwnedEntryPayload {
        ir::wire::OwnedEntryPayload::sealed(
            sym(name),
            KindDiscriminant::Function,
            KindWire::Function(FunctionWire {
                input_params: Box::new([]),
                output_params: Box::new([]),
                sig: FnSigFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn trait_payload_with_super(name: &str, super_ref: TypeRefWire) -> ir::wire::OwnedEntryPayload {
        ir::wire::OwnedEntryPayload::sealed(
            sym(name),
            KindDiscriminant::Trait,
            KindWire::Trait(TraitWire {
                supers: Box::new([super_ref]),
                flags: TraitFlags::default(),
                generics: Box::new([]),
                wheres: Box::new([]),
            }),
            EntryPayloadFlags::default(),
        )
    }

    fn default_key() -> ReverseIndexKey {
        ReverseIndexKey { channel_tip: [0u8; 32], schema_version: SCHEMA_VERSION }
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    /// `usages_of` returns exactly the owners for a target when confidence is
    /// at or above the graph floor.
    #[test]
    fn usages_of_returns_graph_worthy_owners() {
        let mut ir = IrView::new(pkg_id());
        ir.insert_entry(intro(1), module_payload("root"), None);
        ir.insert_entry(intro(2), fn_payload("caller"), Some(intro(1)));
        ir.insert_entry(intro(3), fn_payload("callee"), Some(intro(1)));

        let target = sref_local(3);

        // Oracle occurrence (above floor) → should appear.
        ir.insert_occurrence(Occurrence {
            owner: intro(2),
            target: target.clone(),
            kind: ReferenceKind::FunctionCall,
            confidence: Confidence::Oracle,
            rel_span: RelSpan::new(0, 5),
        });

        let idx = ReversePositionIndex::build(&ir, default_key());
        assert_eq!(idx.usages_of(&target), &[intro(2)]);
    }

    /// A `Confidence::Syntactic` occurrence (below the graph floor) MUST NOT
    /// appear in the reverse-occurrence postings.
    #[test]
    fn syntactic_occurrence_is_excluded() {
        let mut ir = IrView::new(pkg_id());
        ir.insert_entry(intro(1), module_payload("root"), None);
        ir.insert_entry(intro(2), fn_payload("caller"), Some(intro(1)));
        ir.insert_entry(intro(3), fn_payload("callee"), Some(intro(1)));

        let target = sref_local(3);

        // Syntactic only — below floor.
        ir.insert_occurrence(Occurrence {
            owner: intro(2),
            target: target.clone(),
            kind: ReferenceKind::FunctionCall,
            confidence: Confidence::Syntactic,
            rel_span: RelSpan::new(0, 5),
        });

        let idx = ReversePositionIndex::build(&ir, default_key());
        assert_eq!(
            idx.usages_of(&target),
            &[] as &[IntroId],
            "Syntactic confidence must not project into graph postings"
        );
    }

    /// `Confidence::Suffix` (also below floor) is likewise excluded.
    #[test]
    fn suffix_occurrence_is_excluded() {
        let mut ir = IrView::new(pkg_id());
        ir.insert_entry(intro(1), fn_payload("a"), None);
        let target = sref_local(2);
        ir.insert_occurrence(Occurrence {
            owner: intro(1),
            target: target.clone(),
            kind: ReferenceKind::TypeReference,
            confidence: Confidence::Suffix,
            rel_span: RelSpan::new(0, 3),
        });
        let idx = ReversePositionIndex::build(&ir, default_key());
        assert_eq!(idx.usages_of(&target), &[] as &[IntroId]);
    }

    /// Building the index twice from the same view yields identical posting maps.
    #[test]
    fn build_is_deterministic() {
        let mut ir = IrView::new(pkg_id());
        ir.insert_entry(intro(1), module_payload("mod"), None);
        ir.insert_entry(intro(2), fn_payload("a"), Some(intro(1)));
        ir.insert_entry(intro(3), fn_payload("b"), Some(intro(1)));

        let target = sref_local(3);
        ir.insert_occurrence(Occurrence {
            owner: intro(2),
            target: target.clone(),
            kind: ReferenceKind::FunctionCall,
            confidence: Confidence::Index,
            rel_span: RelSpan::new(0, 4),
        });

        let idx1 = ReversePositionIndex::build(&ir, default_key());
        let idx2 = ReversePositionIndex::build(&ir, default_key());

        // Compare occ postings via the public API.
        assert_eq!(
            idx1.usages_of(&target),
            idx2.usages_of(&target),
            "rebuild must yield identical occurrence postings"
        );
        // Compare typeref postings (empty here, but the call exercising both maps).
        let some_ty = sref_local(10);
        assert_eq!(
            idx1.mentions_of(&some_ty),
            idx2.mentions_of(&some_ty),
            "rebuild must yield identical typeref postings"
        );
    }

    /// `mentions_of` returns the entries that carry a trait supertrait pointing
    /// at the given StableRef.
    #[test]
    fn mentions_of_supertrait() {
        let mut ir = IrView::new(pkg_id());
        ir.insert_entry(intro(1), module_payload("mod"), None);

        // A base trait (intro 2) and a derived trait (intro 3) that extends it.
        ir.insert_entry(intro(2), fn_payload("BaseTrait"), Some(intro(1)));
        let base_sr = sref_local(2);
        ir.insert_entry(
            intro(3),
            trait_payload_with_super("DerivedTrait", TypeRefWire::Same(intro(2))),
            Some(intro(1)),
        );

        let idx = ReversePositionIndex::build(&ir, default_key());
        let mentions = idx.mentions_of(&base_sr);
        assert_eq!(mentions, &[intro(3)], "derived trait should appear in mentions of base");
    }

    /// Multiple owners for the same target are all captured and deduplicated.
    #[test]
    fn multiple_owners_for_same_target() {
        let mut ir = IrView::new(pkg_id());
        ir.insert_entry(intro(1), module_payload("mod"), None);
        ir.insert_entry(intro(2), fn_payload("fn_a"), Some(intro(1)));
        ir.insert_entry(intro(3), fn_payload("fn_b"), Some(intro(1)));
        ir.insert_entry(intro(4), fn_payload("target_fn"), Some(intro(1)));

        let target = sref_local(4);

        ir.insert_occurrence(Occurrence {
            owner: intro(2),
            target: target.clone(),
            kind: ReferenceKind::FunctionCall,
            confidence: Confidence::Import,
            rel_span: RelSpan::new(0, 3),
        });
        ir.insert_occurrence(Occurrence {
            owner: intro(3),
            target: target.clone(),
            kind: ReferenceKind::FunctionCall,
            confidence: Confidence::Oracle,
            rel_span: RelSpan::new(0, 3),
        });
        // Duplicate from owner 2 (should dedup).
        ir.insert_occurrence(Occurrence {
            owner: intro(2),
            target: target.clone(),
            kind: ReferenceKind::MethodCall,
            confidence: Confidence::Index,
            rel_span: RelSpan::new(10, 15),
        });

        let idx = ReversePositionIndex::build(&ir, default_key());
        let owners = idx.usages_of(&target);
        // Both intro(2) and intro(3), deduped and sorted.
        assert_eq!(owners, &[intro(2), intro(3)]);
    }
}
