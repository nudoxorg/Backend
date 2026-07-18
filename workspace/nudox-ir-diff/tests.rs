//! Adversarial coverage for `nudox-ir-diff` (§7, §7.5).
//!
//! Tests are built around small `PristineIntroTable` fixtures constructed
//! via `OwnedEntryPayload::sealed`. They exercise:
//!
//! 1. Doc-only change emits only `DocChanged`
//! 2. Param insert in middle emits `ParamAdded` at correct index
//! 3. Param reorder emits `ParamsReordered`, not N changes
//! 4. Field type change emits `FieldTypeChanged`
//! 5. Enum variant add/remove emits `ChildAdded`/`ChildRemoved`
//! 6. Reexport retarget emits `ReexportRetargeted`
//! 7. Visibility change emits `VisChanged`
//! 8. Alias add/remove emits `AliasesChanged`
//! 9. Deprecation added/removed
//! 10. Trait flags change
//! 11. Round-trip law on ≥5 constructed pairs
//! 12. `delta_digest` is deterministic

use nudox_change::{ChangeSetFingerprint, EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};

use nudox_ir::apply::PristineIntroTable;
use nudox_ir::kind::KindDiscriminant;
use nudox_ir::symbol::Visibility;
use nudox_ir::wire::{
    DeprecationWire, EnumWire, EntryPayloadFlags, FieldWire, FnSigFlags,
    FunctionWire, KindWire, ModuleWire, OwnedEntryPayload, ParamWire,
    ReexportWire, SymbolWire, TraitFlags, TraitWire, TypeRefWire,
};

use crate::apply::apply_delta_with_t1;
use crate::delta::{delta_digest, PackageDelta};
use crate::diff::diff_tables;
use crate::ir_op::IrOp;

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn pkg() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("testlib"))
}

fn sref(n: u8) -> StableRef {
    StableRef::new(pkg(), IntroId::from_raw([n; 32]))
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn cset(n: u8) -> ChangeSetFingerprint {
    ChangeSetFingerprint::from_raw([n; 32])
}

fn sym(name: &str) -> SymbolWire {
    SymbolWire {
        name: name.into(),
        visibility: Visibility::Public,
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

fn module_payload(name: &str) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(sym(name), KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default())
}

fn fn_payload(name: &str, params: &[(&str, u8)]) -> OwnedEntryPayload {
    let inputs: Vec<ParamWire> = params.iter().map(|(pname, ty_n)| ParamWire {
        name: Some(pname.to_string()),
        ty: TypeRefWire::Same(intro(*ty_n)),
    }).collect();
    OwnedEntryPayload::sealed(
        sym(name),
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire {
            input_params: inputs.into_boxed_slice(),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}

fn field_payload(name: &str, ty: u8) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        sym(name),
        KindDiscriminant::Field,
        KindWire::Field(FieldWire { ty: Some(TypeRefWire::Same(intro(ty))) }),
        EntryPayloadFlags::default(),
    )
}

fn reexport_payload(name: &str, target_n: u8) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        sym(name),
        KindDiscriminant::Reexport,
        KindWire::Reexport(ReexportWire { target: sref(target_n) }),
        EntryPayloadFlags::default(),
    )
}

fn single_entry_tables(
    id: IntroId,
    p0: OwnedEntryPayload,
    p1: OwnedEntryPayload,
) -> (PristineIntroTable, PristineIntroTable) {
    let mut t0 = PristineIntroTable::new();
    t0.insert_live(id, p0, None);
    let mut t1 = PristineIntroTable::new();
    t1.insert_live(id, p1, None);
    (t0, t1)
}

fn diff(t0: &PristineIntroTable, t1: &PristineIntroTable) -> PackageDelta {
    diff_tables(t0, t1, cset(0), cset(1), None)
}

fn ops_for(delta: &PackageDelta, id: IntroId) -> Vec<&IrOp> {
    delta.ops.get(&id).map(|v| v.iter().collect()).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Test 1: doc-only change emits only DocChanged
// ---------------------------------------------------------------------------

#[test]
fn doc_only_change_emits_only_doc_changed() {
    let id = intro(1);
    let mut s0 = sym("foo");
    let mut s1 = sym("foo");
    s0.documentation = Some("original".into());
    s1.documentation = Some("updated".into());

    let p0 = OwnedEntryPayload::sealed(s0, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());
    let p1 = OwnedEntryPayload::sealed(s1, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());

    let (t0, t1) = single_entry_tables(id, p0, p1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, id);

    assert_eq!(ops.len(), 1, "expected exactly one op, got: {:?}", ops);
    assert!(matches!(ops[0], IrOp::DocChanged), "expected DocChanged, got {:?}", ops[0]);
}

// ---------------------------------------------------------------------------
// Test 2: param insert in middle emits ParamAdded at correct index
// ---------------------------------------------------------------------------

#[test]
fn param_insert_middle_emits_param_added_at_correct_index() {
    let id = intro(2);
    let p0 = fn_payload("f", &[("a", 10), ("c", 12)]);
    let p1 = fn_payload("f", &[("a", 10), ("b", 11), ("c", 12)]);

    let (t0, t1) = single_entry_tables(id, p0, p1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, id);

    // Should have: ParamAdded{index:1} and SignatureEvolved
    let param_added: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::ParamAdded { index: 1 })).collect();
    assert!(!param_added.is_empty(), "expected ParamAdded{{index:1}}, got: {:?}", ops);

    // The param that was NOT added should not appear as affected.
    let unchanged_removed: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::ParamRemoved { index: 0 } | IrOp::ParamRemoved { index: 2 })).collect();
    assert!(unchanged_removed.is_empty(), "params a and c should be unchanged, got: {:?}", ops);
}

// ---------------------------------------------------------------------------
// Test 3: param reorder emits ParamsReordered, not N changes
// ---------------------------------------------------------------------------

#[test]
fn param_reorder_emits_params_reordered_not_n_changes() {
    let id = intro(3);
    let p0 = fn_payload("g", &[("x", 20), ("y", 21), ("z", 22)]);
    let p1 = fn_payload("g", &[("z", 22), ("x", 20), ("y", 21)]);

    let (t0, t1) = single_entry_tables(id, p0, p1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, id);

    let reordered: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::ParamsReordered)).collect();
    assert!(!reordered.is_empty(), "expected ParamsReordered, got: {:?}", ops);

    let adds_removes: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::ParamAdded { .. } | IrOp::ParamRemoved { .. })).collect();
    assert!(adds_removes.is_empty(), "should not emit individual add/remove on reorder, got: {:?}", ops);
}

// ---------------------------------------------------------------------------
// Test 4: field type change emits FieldTypeChanged
// ---------------------------------------------------------------------------

#[test]
fn field_type_change_emits_field_type_changed() {
    let id = intro(4);
    let p0 = field_payload("count", 30);
    let p1 = field_payload("count", 31);

    let (t0, t1) = single_entry_tables(id, p0, p1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, id);

    let changed: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::FieldTypeChanged { .. })).collect();
    assert!(!changed.is_empty(), "expected FieldTypeChanged, got: {:?}", ops);

    if let IrOp::FieldTypeChanged { old, new } = changed[0] {
        assert_eq!(*old, Some(TypeRefWire::Same(intro(30))));
        assert_eq!(*new, Some(TypeRefWire::Same(intro(31))));
    }
}

// ---------------------------------------------------------------------------
// Test 5: enum variant add/remove emits ChildAdded/ChildRemoved
// ---------------------------------------------------------------------------

#[test]
fn enum_variant_add_remove_emits_child_ops() {
    let enum_id = intro(5);
    let variant_a = intro(50);
    let variant_b = intro(51);
    let variant_c = intro(52);

    let enum0 = OwnedEntryPayload::sealed(
        sym("Status"),
        KindDiscriminant::Enum,
        KindWire::Enum(EnumWire {
            variants: Box::new([variant_a, variant_b]),
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    );
    let enum1 = OwnedEntryPayload::sealed(
        sym("Status"),
        KindDiscriminant::Enum,
        KindWire::Enum(EnumWire {
            variants: Box::new([variant_a, variant_c]),
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    );

    let (t0, t1) = single_entry_tables(enum_id, enum0, enum1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, enum_id);

    let added: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::ChildAdded { child } if *child == variant_c)).collect();
    let removed: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::ChildRemoved { child } if *child == variant_b)).collect();

    assert!(!added.is_empty(), "expected ChildAdded for variant_c, got: {:?}", ops);
    assert!(!removed.is_empty(), "expected ChildRemoved for variant_b, got: {:?}", ops);
}

// ---------------------------------------------------------------------------
// Test 6: reexport retarget emits ReexportRetargeted
// ---------------------------------------------------------------------------

#[test]
fn reexport_retarget_emits_reexport_retargeted() {
    let id = intro(6);
    let p0 = reexport_payload("foo", 60);
    let p1 = reexport_payload("foo", 61);

    let (t0, t1) = single_entry_tables(id, p0, p1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, id);

    let retargeted: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::ReexportRetargeted { .. })).collect();
    assert!(!retargeted.is_empty(), "expected ReexportRetargeted, got: {:?}", ops);

    if let IrOp::ReexportRetargeted { old, new } = retargeted[0] {
        assert_eq!(old.intro, intro(60));
        assert_eq!(new.intro, intro(61));
    }
}

// ---------------------------------------------------------------------------
// Test 7: visibility change emits VisChanged
// ---------------------------------------------------------------------------

#[test]
fn visibility_change_emits_vis_changed() {
    let id = intro(7);
    let mut s0 = sym("bar");
    let mut s1 = sym("bar");
    s0.visibility = Visibility::Public;
    s1.visibility = Visibility::Crate;

    let p0 = OwnedEntryPayload::sealed(s0, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());
    let p1 = OwnedEntryPayload::sealed(s1, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());

    let (t0, t1) = single_entry_tables(id, p0, p1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, id);

    let vis: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::VisChanged { .. })).collect();
    assert!(!vis.is_empty(), "expected VisChanged, got: {:?}", ops);
    if let IrOp::VisChanged { old, new } = vis[0] {
        assert_eq!(*old, Visibility::Public);
        assert_eq!(*new, Visibility::Crate);
    }
}

// ---------------------------------------------------------------------------
// Test 8: alias add/remove emits AliasesChanged
// ---------------------------------------------------------------------------

#[test]
fn aliases_changed_emits_aliases_changed() {
    let id = intro(8);
    let mut s0 = sym("T");
    let mut s1 = sym("T");
    s0.aliases = vec!["A".into(), "B".into()];
    s1.aliases = vec!["B".into(), "C".into()];

    let p0 = OwnedEntryPayload::sealed(s0, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());
    let p1 = OwnedEntryPayload::sealed(s1, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());

    let (t0, t1) = single_entry_tables(id, p0, p1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, id);

    let alias_ops: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::AliasesChanged { .. })).collect();
    assert!(!alias_ops.is_empty(), "expected AliasesChanged, got: {:?}", ops);
    if let IrOp::AliasesChanged { added, removed } = alias_ops[0] {
        assert!(added.iter().any(|a| a.as_str() == "C"), "C should be added");
        assert!(removed.iter().any(|r| r.as_str() == "A"), "A should be removed");
    }
}

// ---------------------------------------------------------------------------
// Test 9: deprecation added / removed
// ---------------------------------------------------------------------------

#[test]
fn deprecation_toggle_emits_deprecation_changed() {
    let id_add = intro(90);
    let mut s0 = sym("old");
    let mut s1 = sym("old");
    s0.deprecation = None;
    s1.deprecation = Some(DeprecationWire { note: Some("use new".into()), since: Some("1.0".into()) });

    let p0 = OwnedEntryPayload::sealed(s0, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());
    let p1 = OwnedEntryPayload::sealed(s1, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());
    let (t0, t1) = single_entry_tables(id_add, p0, p1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, id_add);
    assert!(ops.iter().any(|op| matches!(op, IrOp::DeprecationChanged { added: true })), "expected DeprecationChanged{{added:true}}, got: {:?}", ops);

    // Reverse: deprecation removed
    let id_rem = intro(91);
    let mut r0 = sym("old2");
    let mut r1 = sym("old2");
    r0.deprecation = Some(DeprecationWire { note: None, since: None });
    r1.deprecation = None;
    let q0 = OwnedEntryPayload::sealed(r0, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());
    let q1 = OwnedEntryPayload::sealed(r1, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());
    let (t0r, t1r) = single_entry_tables(id_rem, q0, q1);
    let delta2 = diff(&t0r, &t1r);
    let ops2 = ops_for(&delta2, id_rem);
    assert!(ops2.iter().any(|op| matches!(op, IrOp::DeprecationChanged { added: false })), "expected DeprecationChanged{{added:false}}, got: {:?}", ops2);
}

// ---------------------------------------------------------------------------
// Test 10: trait flags change
// ---------------------------------------------------------------------------

#[test]
fn trait_flags_change_emits_trait_flags_changed() {
    let id = intro(10);
    let make = |flags: TraitFlags| -> OwnedEntryPayload {
        OwnedEntryPayload::sealed(
            sym("MyTrait"),
            KindDiscriminant::Trait,
            KindWire::Trait(TraitWire { supers: Box::new([]), flags, generics: Box::new([]), wheres: Box::new([]) }),
            EntryPayloadFlags::default(),
        )
    };

    let p0 = make(TraitFlags::default());
    let new_flags = TraitFlags { is_unsafe: true, ..TraitFlags::default() };
    let p1 = make(new_flags.clone());

    let (t0, t1) = single_entry_tables(id, p0, p1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, id);

    let flag_ops: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::TraitFlagsChanged { .. })).collect();
    assert!(!flag_ops.is_empty(), "expected TraitFlagsChanged, got: {:?}", ops);
    if let IrOp::TraitFlagsChanged { new, .. } = flag_ops[0] {
        assert!(new.is_unsafe);
    }
}

// ---------------------------------------------------------------------------
// Test 11: lifecycle ops
// ---------------------------------------------------------------------------

#[test]
fn introduced_and_deleted_lifecycle_ops() {
    let id_new = intro(110);
    let id_old = intro(111);

    let mut t0 = PristineIntroTable::new();
    t0.insert_live(id_old, module_payload("old"), None);

    let mut t1 = PristineIntroTable::new();
    t1.insert_live(id_new, module_payload("new"), None);

    let delta = diff(&t0, &t1);

    let new_ops = ops_for(&delta, id_new);
    assert!(new_ops.iter().any(|op| matches!(op, IrOp::Introduced)), "expected Introduced for new id");

    let old_ops = ops_for(&delta, id_old);
    assert!(old_ops.iter().any(|op| matches!(op, IrOp::Deleted)), "expected Deleted for old id");
}

// ---------------------------------------------------------------------------
// Test 12: round-trip law on 5 constructed pairs
// ---------------------------------------------------------------------------

/// Helper: verify `apply_delta(T0, diff(T0, T1)) == T1` for the round-trippable
/// subset. We supply T1 for introduced entries.
fn assert_round_trip(t0: &PristineIntroTable, t1: &PristineIntroTable, label: &str) {
    let delta = diff(t0, t1, );
    let applied = apply_delta_with_t1(t0, &delta, Some(t1))
        .unwrap_or_else(|e| panic!("apply_delta failed on {}: {}", label, e));

    // Compare all ids that are in T1 (the ground truth).
    for (id, p1_payload) in t1.live_entries() {
        let applied_payload = applied.get(id)
            .unwrap_or_else(|| panic!("{}: id {} missing from applied table", label, id.to_hex()));
        assert_eq!(
            applied_payload.payload_hash, p1_payload.payload_hash,
            "{}: payload_hash mismatch for id {}",
            label, id.to_hex()
        );
    }

    // All IDs in T0 that are deleted must not be in applied.
    for (id, _) in t0.live_entries() {
        if !t1.is_live(id) {
            assert!(
                !applied.is_live(id),
                "{}: deleted id {} still in applied table",
                label, id.to_hex()
            );
        }
    }
}

#[test]
fn round_trip_pair1_new_entry() {
    let t0 = PristineIntroTable::new();
    let mut t1 = PristineIntroTable::new();
    t1.insert_live(intro(200), module_payload("root"), None);
    assert_round_trip(&t0, &t1, "pair1 new_entry");
}

#[test]
fn round_trip_pair2_delete_entry() {
    let mut t0 = PristineIntroTable::new();
    t0.insert_live(intro(201), module_payload("gone"), None);
    let t1 = PristineIntroTable::new();
    assert_round_trip(&t0, &t1, "pair2 delete_entry");
}

#[test]
fn round_trip_pair3_rename() {
    let id = intro(202);
    let p0 = module_payload("alpha");
    let p1 = module_payload("beta");
    let (t0, t1) = single_entry_tables(id, p0, p1);
    assert_round_trip(&t0, &t1, "pair3 rename");
}

#[test]
fn round_trip_pair4_field_type_change() {
    let id = intro(203);
    let p0 = field_payload("x", 10);
    let p1 = field_payload("x", 20);
    let (t0, t1) = single_entry_tables(id, p0, p1);
    assert_round_trip(&t0, &t1, "pair4 field_type_change");
}

#[test]
fn round_trip_pair5_reexport_retarget() {
    let id = intro(204);
    let p0 = reexport_payload("re", 70);
    let p1 = reexport_payload("re", 80);
    let (t0, t1) = single_entry_tables(id, p0, p1);
    assert_round_trip(&t0, &t1, "pair5 reexport_retarget");
}

#[test]
fn round_trip_pair6_vis_change() {
    let id = intro(205);
    let mut s0 = sym("v");
    let mut s1 = sym("v");
    s0.visibility = Visibility::Public;
    s1.visibility = Visibility::Private;
    let p0 = OwnedEntryPayload::sealed(s0, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());
    let p1 = OwnedEntryPayload::sealed(s1, KindDiscriminant::Module, KindWire::Module(ModuleWire {}), EntryPayloadFlags::default());
    let (t0, t1) = single_entry_tables(id, p0, p1);
    assert_round_trip(&t0, &t1, "pair6 vis_change");
}

#[test]
fn round_trip_pair7_param_type_change() {
    let id = intro(206);
    let p0 = fn_payload("f", &[("x", 10)]);
    let p1 = fn_payload("f", &[("x", 20)]);
    let (t0, t1) = single_entry_tables(id, p0, p1);
    assert_round_trip(&t0, &t1, "pair7 param_type_change");
}

// ---------------------------------------------------------------------------
// Test 13: delta_digest is deterministic
// ---------------------------------------------------------------------------

#[test]
fn delta_digest_is_deterministic() {
    let id = intro(11);
    let p0 = module_payload("m");
    let p1 = module_payload("n");
    let (t0, t1) = single_entry_tables(id, p0, p1);

    let d1 = diff(&t0, &t1);
    let d2 = diff(&t0, &t1);

    assert_eq!(d1.canonical_bytes(), d2.canonical_bytes(), "canonical_bytes not deterministic");
    assert_eq!(delta_digest(&d1), delta_digest(&d2), "delta_digest not deterministic");
}

// ---------------------------------------------------------------------------
// Test 14: no ops on identical tables
// ---------------------------------------------------------------------------

#[test]
fn identical_tables_produce_no_ops() {
    let id = intro(12);
    let p = fn_payload("no_change", &[("a", 1), ("b", 2)]);

    let mut t0 = PristineIntroTable::new();
    t0.insert_live(id, p.clone(), None);
    let mut t1 = PristineIntroTable::new();
    t1.insert_live(id, p, None);

    let delta = diff(&t0, &t1);
    assert!(delta.ops.is_empty(), "expected no ops for identical tables, got: {:?}", delta.ops);
}

// ---------------------------------------------------------------------------
// Test 15: param remove from middle, others untouched
// ---------------------------------------------------------------------------

#[test]
fn param_remove_middle_others_untouched() {
    let id = intro(15);
    let p0 = fn_payload("h", &[("a", 1), ("b", 2), ("c", 3)]);
    let p1 = fn_payload("h", &[("a", 1), ("c", 3)]);

    let (t0, t1) = single_entry_tables(id, p0, p1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, id);

    let removed: Vec<_> = ops.iter().filter(|op| matches!(op, IrOp::ParamRemoved { .. })).collect();
    assert_eq!(removed.len(), 1, "expected exactly one ParamRemoved, got: {:?}", ops);
    if let IrOp::ParamRemoved { index } = removed[0] {
        assert_eq!(*index, 1, "expected removal at index 1 (b), got {}", index);
    }
}

// ---------------------------------------------------------------------------
// Test 16: a single added link produces exactly ONE LinkAdded op, attached to
// the link's canonical owner (§1.1) — never smeared across edited entries.
// ---------------------------------------------------------------------------

#[test]
fn added_link_emits_one_op_on_canonical_owner() {
    use nudox_ir::apply::LinkRecord;

    let (small, big) = (intro(1), intro(2)); // owner = smaller IntroId
    let link = LinkRecord {
        a: sref(2),
        b: sref(1),
        kind_a: KindDiscriminant::Function,
        kind_b: KindDiscriminant::Module,
    };

    // Both endpoints live in both generations; the link exists only in T1.
    let build = |with_link: bool| {
        let mut t = PristineIntroTable::new();
        t.insert_live(small, module_payload("root"), None);
        t.insert_live(big, fn_payload("f", &[]), Some(small));
        if with_link {
            t.insert_link(link.clone());
        }
        t
    };
    let (t0, t1) = (build(false), build(true));

    let delta = diff(&t0, &t1);

    let owner_ops = ops_for(&delta, small);
    let added: Vec<_> = owner_ops.iter().filter(|op| matches!(op, IrOp::LinkAdded { .. })).collect();
    assert_eq!(added.len(), 1, "exactly one LinkAdded on the owner intro, got: {:?}", owner_ops);

    // The non-owner endpoint must carry no link op (no smearing).
    assert!(
        ops_for(&delta, big).iter().all(|op| !matches!(op, IrOp::LinkAdded { .. } | IrOp::LinkRemoved { .. })),
        "non-owner endpoint must not carry link ops",
    );
}

// ---------------------------------------------------------------------------
// Test 17: ops within an entry are in canonical order (§7.3):
// lifecycle < continuity < meta < kind-specific < links < probes.
// ---------------------------------------------------------------------------

#[test]
fn ops_within_entry_are_canonically_ordered() {
    use crate::ir_op::op_sort_key;

    let id = intro(17);
    // Rename + visibility drop + param-type change ⇒ continuity + meta + kind ops.
    let mut p1sym = sym("renamed");
    p1sym.visibility = Visibility::Crate;
    let p0 = fn_payload("orig", &[("x", 1)]);
    let p1 = OwnedEntryPayload::sealed(
        p1sym,
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire {
            input_params: Box::new([ParamWire { name: Some("x".into()), ty: TypeRefWire::Same(intro(9)) }]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    );

    let (t0, t1) = single_entry_tables(id, p0, p1);
    let delta = diff(&t0, &t1);
    let ops = ops_for(&delta, id);
    assert!(ops.len() >= 3, "expected several ops, got: {:?}", ops);

    let keys: Vec<u8> = ops.iter().map(|op| op_sort_key(op)).collect();
    assert!(keys.windows(2).all(|w| w[0] <= w[1]), "ops not in canonical order: {:?}", keys);
    // Sanity: the categories we triggered are present.
    assert!(ops.iter().any(|op| matches!(op, IrOp::Renamed { .. })));
    assert!(ops.iter().any(|op| matches!(op, IrOp::VisChanged { .. })));
    assert!(ops.iter().any(|op| matches!(op, IrOp::ParamTypeChanged { .. })));
}
