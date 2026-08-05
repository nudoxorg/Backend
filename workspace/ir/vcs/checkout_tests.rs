use std::sync::Arc;

use crate::wire::PayloadTable;
use crate::wire::{
    EntryPayloadFlags, FunctionWire, KindWire, ModuleWire, OwnedEntryPayload, SymbolWire,
};
use ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName};
use ir::entry::Visibility;
use ir::kind::KindDiscriminant;

use crate::f1::F1View;
use crate::repo::IrRepository;

/// Decode a symbol blob's bytes back to its payload (for assertions).
fn payload_of(bytes: &[u8]) -> crate::wire::OwnedEntryPayload {
    F1View::from_bytes(bytes)
        .unwrap()
        .to_owned_payload()
        .unwrap()
}

fn pkg() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn sym(name: &str) -> SymbolWire {
    SymbolWire {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: None,
        source_path: "src/lib.rs".to_owned(),
        span_start: 0,
        span_end: name.len() as u32,
        aliases: Vec::new(),
        deprecation: None,
        doc_links: Vec::new(),
        attrs: Vec::new(),
        cfg: None,
    }
}

fn function(name: &str) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        sym(name),
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: Default::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}

fn module(name: &str) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        sym(name),
        KindDiscriminant::Module,
        KindWire::Module(ModuleWire {}),
        EntryPayloadFlags::default(),
    )
}

fn build_ir(entries: &[(u8, OwnedEntryPayload)]) -> PayloadTable {
    let mut ir = PayloadTable::new();
    for (n, payload) in entries {
        ir.insert_live(intro(*n), payload.clone(), None);
    }
    ir
}

// -----------------------------------------------------------------------
// Test 1: full materialize_index round-trips both symbols
// -----------------------------------------------------------------------

#[test]
fn full_materialize_index_has_both_symbols() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    let ir = build_ir(&[(1, module("root")), (2, function("do_thing"))]);
    repo.record_generation(&ir)
        .unwrap()
        .expect("change recorded");

    let idx = repo.materialize_index().unwrap();

    assert_eq!(idx.len(), 2);
    assert!(idx.get(intro(1)).is_some(), "intro 1 must be present");
    assert!(idx.get(intro(2)).is_some(), "intro 2 must be present");

    // Decode the blobs and check payloads survive the round-trip.
    assert_eq!(payload_of(&idx.get(intro(1)).unwrap()[..]), module("root"));
    assert_eq!(
        payload_of(&idx.get(intro(2)).unwrap()[..]),
        function("do_thing")
    );
}

// -----------------------------------------------------------------------
// Test 2: incremental reuse — untouched Arc is reused
// -----------------------------------------------------------------------

#[test]
fn incremental_reuse_untouched_arc() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    // Gen A: symbols 1 + 2.
    let ir_a = build_ir(&[(1, module("root")), (2, function("do_thing"))]);
    repo.record_generation(&ir_a).unwrap().expect("change A");
    let idx_a = repo.materialize_index().unwrap();
    assert_eq!(idx_a.len(), 2);

    // Gen B: modify symbol 1, keep symbol 2, add symbol 3.
    let ir_b = build_ir(&[
        (1, module("root_v2")),    // changed
        (2, function("do_thing")), // unchanged
        (3, function("new_fn")),   // added
    ]);
    repo.record_generation(&ir_b).unwrap().expect("change B");

    // Incremental rebuild from idx_a.
    let idx_b = repo.materialize_index_incremental(&idx_a).unwrap();

    // Must have intros {1, 2, 3}.
    assert_eq!(idx_b.len(), 3, "expected 3 symbols in idx_b");
    assert!(idx_b.get(intro(1)).is_some());
    assert!(idx_b.get(intro(2)).is_some());
    assert!(idx_b.get(intro(3)).is_some());

    // Symbol 2 is untouched → same Arc pointer.
    assert!(
        Arc::ptr_eq(idx_a.get(intro(2)).unwrap(), idx_b.get(intro(2)).unwrap()),
        "symbol 2 must reuse the same Arc (untouched)"
    );

    // Symbol 1 must have changed.
    assert!(
        !Arc::ptr_eq(idx_a.get(intro(1)).unwrap(), idx_b.get(intro(1)).unwrap()),
        "symbol 1 must have a different Arc (modified)"
    );

    // idx_b must deep-equal a fresh full materialize.
    let idx_full = repo.materialize_index().unwrap();
    assert_eq!(idx_b.len(), idx_full.len());
    for intro_id in idx_full.intros() {
        let b_bytes = &idx_b.symbols[&intro_id][..];
        let f_bytes = &idx_full.symbols[&intro_id][..];
        assert_eq!(b_bytes, f_bytes, "bytes mismatch for intro {intro_id:?}");
    }
}

// -----------------------------------------------------------------------
// Test 3: incremental removal — deleted symbol disappears
// -----------------------------------------------------------------------

#[test]
fn incremental_removal_drops_deleted_symbol() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    // Gen A: symbols 1 + 2.
    let ir_a = build_ir(&[(1, module("root")), (2, function("do_thing"))]);
    repo.record_generation(&ir_a).unwrap().expect("change A");
    let idx_a = repo.materialize_index().unwrap();
    assert_eq!(idx_a.len(), 2);

    // Gen B: only symbol 1 (symbol 2 deleted).
    let ir_b = build_ir(&[(1, module("root"))]);
    repo.record_generation(&ir_b).unwrap().expect("change B");

    let idx_b = repo.materialize_index_incremental(&idx_a).unwrap();

    assert_eq!(idx_b.len(), 1, "only intro 1 survives");
    assert!(
        idx_b.get(intro(1)).is_some(),
        "intro 1 must still be present"
    );
    assert!(
        idx_b.get(intro(2)).is_none(),
        "intro 2 must be absent (deleted)"
    );
}

// -----------------------------------------------------------------------
// Test 4: no-op — same tip returns same tip, is_empty / equal
// -----------------------------------------------------------------------

#[test]
fn incremental_noop_when_tip_unchanged() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    let ir = build_ir(&[(1, module("root"))]);
    repo.record_generation(&ir).unwrap().expect("change");
    let idx = repo.materialize_index().unwrap();

    // Call incremental with the SAME index — tip hasn't changed.
    let idx2 = repo.materialize_index_incremental(&idx).unwrap();

    // Tips must be equal.
    assert_eq!(idx.tip, idx2.tip, "tips must match on no-op");
    // Pointer equality: Arc instances must be the SAME (clone of prev.symbols).
    assert!(
        Arc::ptr_eq(idx.get(intro(1)).unwrap(), idx2.get(intro(1)).unwrap()),
        "no-op incremental must return the exact same Arc"
    );
}

// -----------------------------------------------------------------------
// Test 5: partial checkout — checkout_symbol + checkout_symbols
// -----------------------------------------------------------------------

#[test]
fn partial_checkout_symbol_and_symbols() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    let ir = build_ir(&[
        (1, module("root")),
        (2, function("do_thing")),
        (3, function("helper")),
    ]);
    repo.record_generation(&ir).unwrap().expect("change");

    // checkout_symbol for a present intro.
    let bytes1 = repo.checkout_symbol(intro(1)).unwrap();
    assert!(bytes1.is_some(), "intro 1 must be present");
    assert_eq!(payload_of(&bytes1.unwrap()[..]), module("root"));

    // checkout_symbol for an absent intro.
    let absent = repo.checkout_symbol(intro(99)).unwrap();
    assert!(absent.is_none(), "non-existent intro must return None");

    // checkout_symbols for a subset.
    let multi = repo.checkout_symbols(&[intro(1), intro(3)]).unwrap();
    assert_eq!(multi.len(), 2);
    assert!(multi.contains_key(&intro(1)));
    assert!(multi.contains_key(&intro(3)));
    assert!(!multi.contains_key(&intro(2)));

    assert_eq!(payload_of(&multi[&intro(3)][..]), function("helper"));
}

// -----------------------------------------------------------------------
// Test 6: incremental from empty prev (MaterializedIndex::empty())
// -----------------------------------------------------------------------

#[test]
fn incremental_from_empty_rebuilds_all() {
    let repo = IrRepository::in_memory(pkg(), "main").unwrap();

    let ir = build_ir(&[(1, module("root")), (2, function("do_thing"))]);
    repo.record_generation(&ir).unwrap().expect("change");

    let prev = super::MaterializedIndex::empty();
    let idx = repo.materialize_index_incremental(&prev).unwrap();

    assert_eq!(idx.len(), 2);
    assert!(idx.get(intro(1)).is_some());
    assert!(idx.get(intro(2)).is_some());
}
