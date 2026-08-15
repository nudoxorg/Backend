//! Tests for Pack A CSC-parity semver lints.
use super::*;
use crate::semver::report::BreakClass;
use crate::semver::surface::{ApiSurface, ExportPolicy, surface};
use crate::wire::PayloadTable;
use crate::wire::{
    AttrTok, EntryPayloadFlags, EnumWire, FnSigFlags, FunctionWire, KindWire, OwnedEntryPayload,
    RecordForm, RecordWire, Sealed, SymbolWire, TraitFlags, TraitWire, TriState, VariantForm,
    VariantWire,
};
use ir::change::IntroId;
use ir::entry::Visibility;
use ir::kind::KindDiscriminant;

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

fn make_sym(name: &str, vis: Visibility) -> SymbolWire {
    SymbolWire {
        name: name.into(),
        visibility: vis,
        documentation: Option::None,
        source_path: "src/lib.rs".into(),
        span_start: 0,
        span_end: 10,
        aliases: Vec::new(),
        deprecation: Option::None,
        doc_links: Vec::new(),
        attrs: Vec::new(),
        cfg: Option::None,
    }
}

fn fn_payload(name: &str, vis: Visibility) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        make_sym(name, vis),
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

fn enum_payload(name: &str, variants: &[IntroId]) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        make_sym(name, Visibility::Public),
        KindDiscriminant::Enum,
        KindWire::Enum(EnumWire {
            variants: variants.to_vec().into_boxed_slice(),
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}

fn variant_payload(name: &str) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        make_sym(name, Visibility::Public),
        KindDiscriminant::Variant,
        KindWire::Variant(VariantWire {
            form: VariantForm::Unit,
            discr: Option::None,
            fields: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}

fn record_payload(name: &str, fields: &[IntroId]) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        make_sym(name, Visibility::Public),
        KindDiscriminant::Record,
        KindWire::Record(RecordWire {
            form: RecordForm::Struct,
            fields: fields.to_vec().into_boxed_slice(),
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}

/// Build a single-item surface from one payload (no parent).
fn single_item_surface(id: IntroId, payload: OwnedEntryPayload) -> ApiSurface {
    let mut table = PayloadTable::new();
    table.insert_live(id, payload, Option::None);
    surface(&table, &ExportPolicy::default())
}

fn collect_findings(old: &ApiSurface, new: &ApiSurface) -> Vec<Finding> {
    let mut out = Vec::new();
    run_pack_a(old, new, &mut out);
    out
}

// ── A-1: item removed → Major ─────────────────────────────────────────────

#[test]
fn a1_item_removed_is_major() {
    let id = intro(1);
    let old = single_item_surface(id, fn_payload("foo", Visibility::Public));
    let new = {
        let table = PayloadTable::new();
        surface(&table, &ExportPolicy::default())
    };

    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A1 && f.class == BreakClass::Major),
        "expected A-1 Major; got: {findings:?}"
    );
}

// ── A-2: struct → enum → Major ────────────────────────────────────────────

#[test]
fn a2_kind_changed_is_major() {
    let id = intro(2);

    // Build old surface manually (struct).
    let old_payload = OwnedEntryPayload::sealed(
        make_sym("Foo", Visibility::Public),
        KindDiscriminant::Record,
        KindWire::Record(RecordWire {
            form: RecordForm::Struct,
            fields: Box::new([]),
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    );
    let old = single_item_surface(id, old_payload);

    // New surface: same id, but now an enum.
    let new_payload = enum_payload("Foo", &[]);
    let new = single_item_surface(id, new_payload);

    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A2 && f.class == BreakClass::Major),
        "expected A-2 Major; got: {findings:?}"
    );
}

// ── A-3: pub → pub(crate) → Major ────────────────────────────────────────

#[test]
fn a3_vis_lowered_is_major() {
    // A-3 proper: vis lowered while the item stays ON the surface (floor admits
    // both levels). Crossing the *public* boundary (pub → pub(crate) under the
    // default Public floor) instead removes the item from the surface and is
    // reported as A-1 — see `a3_lowering_below_floor_is_item_missing`.
    let id = intro(3);
    let policy = ExportPolicy {
        visibility_floor: Visibility::Private,
        ..Default::default()
    };
    let build = |vis| {
        let mut table = PayloadTable::new();
        table.insert_live(id, fn_payload("bar", vis), Option::None);
        surface(&table, &policy)
    };
    let old = build(Visibility::Public);
    let new = build(Visibility::Crate);

    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A3 && f.class == BreakClass::Major),
        "expected A-3 Major; got: {findings:?}"
    );
}

#[test]
fn a3_lowering_below_floor_is_item_missing() {
    // pub → pub(crate) under the default Public floor: the item leaves the
    // public surface → A-1 Major (the CSC-equivalent verdict).
    let id = intro(30);
    let old = single_item_surface(id, fn_payload("bar", Visibility::Public));
    let new = single_item_surface(id, fn_payload("bar", Visibility::Crate));
    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A1 && f.class == BreakClass::Major),
        "expected A-1 Major (item left public surface); got: {findings:?}"
    );
}

// ── A-6: variant added exhaustive → Major ─────────────────────────────────

#[test]
fn a6_variant_added_exhaustive_is_major() {
    let enum_id = intro(10);
    let v1 = intro(11);
    let v2 = intro(12);

    let mut old_table = PayloadTable::new();
    old_table.insert_live(enum_id, enum_payload("MyEnum", &[v1]), Option::None);
    old_table.insert_live(v1, variant_payload("Alpha"), Some(enum_id));
    let old = surface(&old_table, &ExportPolicy::default());

    let mut new_table = PayloadTable::new();
    new_table.insert_live(enum_id, enum_payload("MyEnum", &[v1, v2]), Option::None);
    new_table.insert_live(v1, variant_payload("Alpha"), Some(enum_id));
    new_table.insert_live(v2, variant_payload("Beta"), Some(enum_id));
    let new = surface(&new_table, &ExportPolicy::default());

    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A6 && f.class == BreakClass::Major),
        "expected A-6 Major; got: {findings:?}"
    );
}

// ── A-7: variant added non_exhaustive → Minor ─────────────────────────────

#[test]
fn a7_variant_added_non_exhaustive_is_minor() {
    let enum_id = intro(20);
    let v1 = intro(21);
    let v2 = intro(22);

    // Mark enum as non_exhaustive.
    let mut sym = make_sym("MyEnum", Visibility::Public);
    sym.attrs.push(AttrTok {
        token: "non_exhaustive".into(),
        arg: Option::None,
    });
    let ne_enum_payload = OwnedEntryPayload::sealed(
        sym,
        KindDiscriminant::Enum,
        KindWire::Enum(EnumWire {
            variants: Box::new([v1]),
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    );

    let mut old_table = PayloadTable::new();
    old_table.insert_live(enum_id, ne_enum_payload, Option::None);
    old_table.insert_live(v1, variant_payload("Alpha"), Some(enum_id));
    let old = surface(&old_table, &ExportPolicy::default());

    let mut sym2 = make_sym("MyEnum", Visibility::Public);
    sym2.attrs.push(AttrTok {
        token: "non_exhaustive".into(),
        arg: Option::None,
    });
    let ne_enum2 = OwnedEntryPayload::sealed(
        sym2,
        KindDiscriminant::Enum,
        KindWire::Enum(EnumWire {
            variants: Box::new([v1, v2]),
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    );

    let mut new_table = PayloadTable::new();
    new_table.insert_live(enum_id, ne_enum2, Option::None);
    new_table.insert_live(v1, variant_payload("Alpha"), Some(enum_id));
    new_table.insert_live(v2, variant_payload("Beta"), Some(enum_id));
    let new = surface(&new_table, &ExportPolicy::default());

    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A7 && f.class == BreakClass::Minor),
        "expected A-7 Minor; got: {findings:?}"
    );
}

// ── A-12: must_use added → Minor ──────────────────────────────────────────

#[test]
fn a12_must_use_added_is_minor() {
    let id = intro(30);
    let old = single_item_surface(id, fn_payload("must_fn", Visibility::Public));

    let mut sym = make_sym("must_fn", Visibility::Public);
    sym.attrs.push(AttrTok {
        token: "must_use".into(),
        arg: Option::None,
    });
    let new_payload = OwnedEntryPayload::sealed(
        sym,
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    );
    let new = single_item_surface(id, new_payload);

    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A12 && f.class == BreakClass::Minor),
        "expected A-12 Minor; got: {findings:?}"
    );
}

// ── A-14: non_exhaustive added → Major ────────────────────────────────────

#[test]
fn a14_non_exhaustive_added_is_major() {
    let id = intro(40);

    let old_payload = record_payload("Cfg", &[]);
    let old = single_item_surface(id, old_payload);

    let mut sym = make_sym("Cfg", Visibility::Public);
    sym.attrs.push(AttrTok {
        token: "non_exhaustive".into(),
        arg: Option::None,
    });
    let new_payload = OwnedEntryPayload::sealed(
        sym,
        KindDiscriminant::Record,
        KindWire::Record(RecordWire {
            form: RecordForm::Struct,
            fields: Box::new([]),
            generics: Box::new([]),
            wheres: Box::new([]),
            auto: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    );
    let new = single_item_surface(id, new_payload);

    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A14 && f.class == BreakClass::Major),
        "expected A-14 Major; got: {findings:?}"
    );
}

// ── internal (non-exported) deleted → no finding ──────────────────────────

#[test]
fn internal_deleted_no_finding() {
    let pub_id = intro(50);
    let priv_id = intro(51);

    // Old: one public fn + one private fn.
    let mut old_table = PayloadTable::new();
    old_table.insert_live(
        pub_id,
        fn_payload("pub_fn", Visibility::Public),
        Option::None,
    );
    old_table.insert_live(
        priv_id,
        fn_payload("priv_fn", Visibility::Private),
        Option::None,
    );
    let old = surface(&old_table, &ExportPolicy::default());

    // New: private fn removed.
    let mut new_table = PayloadTable::new();
    new_table.insert_live(
        pub_id,
        fn_payload("pub_fn", Visibility::Public),
        Option::None,
    );
    let new = surface(&new_table, &ExportPolicy::default());

    let findings = collect_findings(&old, &new);
    // Should have no A-1 finding for the private item.
    assert!(
        !findings.iter().any(|f| f.item == Some(priv_id)),
        "private deletion should produce no finding; got: {findings:?}"
    );
}

// ── A-8 / A-9 / A-10: trait items (the parent→child join, §8.2) ───────────

fn method_payload(name: &str, defaulted: bool) -> OwnedEntryPayload {
    let sig = FnSigFlags {
        defaulted,
        ..FnSigFlags::default()
    };
    OwnedEntryPayload::sealed(
        make_sym(name, Visibility::Public),
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig,
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}

fn trait_payload(name: &str, sealed: Sealed) -> OwnedEntryPayload {
    OwnedEntryPayload::sealed(
        make_sym(name, Visibility::Public),
        KindDiscriminant::Trait,
        KindWire::Trait(TraitWire {
            supers: Box::new([]),
            flags: TraitFlags {
                is_auto: false,
                is_unsafe: false,
                dyn_compat: TriState::Yes,
                sealed,
            },
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}

/// Build a surface with a public trait (`trait_id`) plus method children,
/// each `(intro, name, defaulted)`, parented to the trait.
fn trait_surface(
    trait_id: IntroId,
    sealed: Sealed,
    methods: &[(IntroId, &str, bool)],
) -> ApiSurface {
    let mut table = PayloadTable::new();
    table.insert_live(trait_id, trait_payload("Tr", sealed), Option::None);
    for (mid, mname, defaulted) in methods {
        table.insert_live(*mid, method_payload(mname, *defaulted), Some(trait_id));
    }
    surface(&table, &ExportPolicy::default())
}

#[test]
fn a8_required_item_added_unsealed_is_major() {
    let tr = intro(80);
    let old = trait_surface(tr, Sealed::None, &[(intro(81), "a", false)]);
    // Add a second, non-defaulted (required) method to an unsealed trait.
    let new = trait_surface(
        tr,
        Sealed::None,
        &[(intro(81), "a", false), (intro(82), "b", false)],
    );
    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A8 && f.class == BreakClass::Major),
        "expected A-8 Major; got: {findings:?}"
    );
}

#[test]
fn a9_defaulted_item_added_is_minor() {
    let tr = intro(83);
    let old = trait_surface(tr, Sealed::None, &[(intro(84), "a", false)]);
    // Add a defaulted method → Minor (existing impls still compile).
    let new = trait_surface(
        tr,
        Sealed::None,
        &[(intro(84), "a", false), (intro(85), "b", true)],
    );
    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A9 && f.class == BreakClass::Minor),
        "expected A-9 Minor; got: {findings:?}"
    );
    assert!(
        !findings.iter().any(|f| f.lint == A8),
        "a defaulted addition must NOT fire A-8; got: {findings:?}"
    );
}

#[test]
fn a10_trait_item_removed_is_major() {
    let tr = intro(86);
    let old = trait_surface(
        tr,
        Sealed::None,
        &[(intro(87), "a", false), (intro(88), "b", false)],
    );
    let new = trait_surface(tr, Sealed::None, &[(intro(87), "a", false)]);
    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A10 && f.class == BreakClass::Major),
        "expected A-10 Major; got: {findings:?}"
    );
}

#[test]
fn a8_required_add_on_sealed_trait_is_minor() {
    // Adding a required item to a sealed trait can't break downstream impls
    // (none exist) → A-9 Minor, not A-8 Major (§9.4 A-9 "defaulted or sealed").
    let tr = intro(89);
    let old = trait_surface(tr, Sealed::Full, &[(intro(90), "a", false)]);
    let new = trait_surface(
        tr,
        Sealed::Full,
        &[(intro(90), "a", false), (intro(91), "b", false)],
    );
    let findings = collect_findings(&old, &new);
    assert!(
        findings
            .iter()
            .any(|f| f.lint == A9 && f.class == BreakClass::Minor),
        "sealed trait item add should be A-9 Minor; got: {findings:?}"
    );
}
