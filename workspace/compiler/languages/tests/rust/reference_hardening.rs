//! Reference-resolution hardening for the Rust producer (rust-analyzer,
//! in-process).
//!
//! Rust is this codebase's most complete producer (Intro + Foreign +
//! occurrences), but an adversarial audit found real gaps in the body
//! occurrence walk and in how unresolvable / structurally-unrepresentable
//! types are classified. Each `#[test]` below pins one of those gaps closed
//! and, where the audit flagged existing-but-untested behavior, pins it too.
//!
//! Every fixture is a tiny, hermetic, self-contained Cargo package written to
//! `CARGO_TARGET_TMPDIR` and loaded with `direct_repo: false` — no network,
//! no Nix corpus. rust-analyzer's per-crate startup cost is substantial, so
//! this file loads exactly two crates: one exercising the occurrence walk
//! (`OCCURRENCE_SURFACE`), one exercising type lowering
//! (`TYPE_SURFACE`) — every item's assertion lives in the `#[test]` for its
//! surface, not in a one-item-per-crate fixture.
//!
//! # On asserting structure, not counts
//!
//! A count (`occurrences.len() == N`) would pass on a producer that recorded
//! the right *number* of facts against the wrong targets. Every assertion
//! here names the specific declaration (by `IntroId`, found by name + kind in
//! the same sealed table) a reference must resolve to, and the specific
//! `ReferenceKind`/`Type` shape it must resolve as.

use std::path::{Path, PathBuf};

use nudox_ir::{
    change::{EcosystemId, IntroId, PackageLineageId, PackageName},
    foreign::{ForeignOrigin, Unlinked},
    index::Ref,
    kind::KindDiscriminant,
    kinds::{
        Type,
        ty::{TupleElement, UnknownType},
    },
    vocab::{Confidence, ReferenceKind},
};
use nudox_languages::rust::RustProducer;
use nudox_languages::{PackageSource, Produced, produce};

// ── Shared fixture harness ──────────────────────────────────────────────────

fn write_fixture(dir: &str, pkg_name: &str, body: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(dir);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).expect("create fixture src dir");
    std::fs::write(
        root.join("Cargo.toml"),
        format!(
            "[package]\n\
             name = \"{pkg_name}\"\n\
             version = \"0.1.0\"\n\
             edition = \"2021\"\n\
             \n\
             [lib]\n\
             path = \"src/lib.rs\"\n\
             \n\
             [workspace]\n"
        ),
    )
    .expect("write fixture manifest");
    std::fs::write(root.join("src/lib.rs"), body).expect("write fixture lib.rs");
    root
}

/// Lower `body` through the real `produce()` pipeline (the same path every
/// consumer of a Rust package goes through — `Lowering::finish` + `seal`),
/// with every cross-package reference left unlinked. Panics with the
/// producer's real error chain on failure, so a broken fixture fails loudly
/// at the one call site instead of downstream in an assertion.
fn produce_fixture(dir: &str, pkg_name: &str, body: &str) -> (Produced, PackageLineageId) {
    let root = write_fixture(dir, pkg_name, body);
    let source = PackageSource::new(&root, pkg_name, "0.1.0");
    let lineage = PackageLineageId::new(
        EcosystemId::new("cargo"),
        PackageName::new(pkg_name.to_owned()),
    );
    let produced = produce(
        &RustProducer { direct_repo: false },
        &source,
        &lineage,
        &Unlinked,
    )
    .unwrap_or_else(|e| panic!("fixture {dir} must lower: {e}"));
    (produced, lineage)
}

/// Find the one declaration named `name` of kind `kind` in `produced.table`.
fn find_intro(produced: &Produced, name: &str, kind: KindDiscriminant) -> IntroId {
    produced
        .table
        .iter()
        .find(|(_, entry)| entry.sym().name == name && entry.kind().discriminant() == Some(kind))
        .map(|(intro, _)| intro)
        .unwrap_or_else(|| panic!("no {kind:?} declaration named `{name}` in the sealed table"))
}

/// Push `ty` and every type nested inside it — mirrors `no_top_type.rs`'s
/// `flatten`, so a gap or a wrong `Nominal` hiding inside an `Apply`'s
/// arguments (exactly where a generic parameter's real type lives) cannot
/// hide from a whole-table search.
fn flatten(ty: &Type, out: &mut Vec<Type>) {
    out.push(ty.clone());
    match ty {
        Type::Slice(t)
        | Type::Array { ty: t, .. }
        | Type::Annotated { inner: t, .. }
        | Type::Wildcard { bound: Some(t), .. }
        | Type::Primitive(
            nudox_ir::kinds::ty::Primitive::MutPointer(t)
            | nudox_ir::kinds::ty::Primitive::ConstPointer(t)
            | nudox_ir::kinds::ty::Primitive::Reference { ty: t, .. },
        ) => flatten(t, out),
        Type::Union(ts) | Type::Intersection(ts) | Type::ImplTrait(ts) | Type::DynTrait(ts) => {
            for t in ts {
                flatten(t, out);
            }
        }
        Type::Apply { base, args } => {
            flatten(base, out);
            for a in args {
                flatten(a, out);
            }
        }
        Type::Tuple(elems) => {
            for e in elems {
                match e {
                    TupleElement::Positional(t) => flatten(t, out),
                    TupleElement::Named { ty, .. } => flatten(ty, out),
                }
            }
        }
        Type::FunctionPointer { params, ret, .. } => {
            for p in params {
                flatten(p, out);
            }
            if let Some(r) = ret {
                flatten(r, out);
            }
        }
        Type::QualifiedPath {
            self_ty, trait_ref, ..
        } => {
            flatten(self_ty, out);
            if let Some(t) = trait_ref {
                flatten(t, out);
            }
        }
        _ => {}
    }
}

/// Every `Param`/`Impl` type in `produced.table`, flattened.
fn all_types(produced: &Produced) -> Vec<Type> {
    use nudox_ir::{entry::EntryInner, kind::Kind};
    let mut out = Vec::new();
    for (_, entry) in produced.table.iter() {
        let EntryInner::Owned(kind) = entry.kind() else {
            continue;
        };
        match kind {
            Kind::Param(p) => {
                if let Some(t) = &p.ty {
                    flatten(t, &mut out);
                }
            }
            Kind::Impl(i) => {
                flatten(&i.self_ty, &mut out);
                if let Some(t) = &i.of {
                    flatten(t, &mut out);
                }
            }
            Kind::Field(f) => {
                if let Some(t) = &f.ty {
                    flatten(t, &mut out);
                }
            }
            _ => {}
        }
    }
    out
}

// ═══════════════════════════════════════════════════════════════════════════
// R2 / R3 / R5 / R6 — occurrence-walk node-kind coverage
// ═══════════════════════════════════════════════════════════════════════════

/// Reaches every node kind `item.rs::record_body_occurrences` previously
/// dropped:
///
/// * `Wrap(1)` — a tuple-struct constructor call (R2). The callee path
///   resolves to `ModuleDef::Adt(Adt::Struct(_))`, not `ModuleDef::Function`,
///   so the pre-fix `path_resolution_to_target` (which matched only
///   `Function`) silently dropped it.
/// * `Color::Rgb(1, 2, 3)` — a tuple *enum-variant* constructor call (R2),
///   resolving to `ModuleDef::EnumVariant`.
/// * `vec![g()]` — a call inside an unexpanded macro's argument tokens (R3):
///   `body.syntax().descendants()` never reaches a `CALL_EXPR` node for `g()`
///   because none exists in the un-expanded tree.
/// * `let p = g; p` — a bare, non-call value path naming a function (R5):
///   `g` here is a `PATH_EXPR`, not the callee of any `CALL_EXPR`.
/// * `let c = Color::Red; c` — a bare value path naming a unit enum variant
///   (R5).
/// * `P { x: 0 }` — a struct-literal type reference (R6): `RecordExpr`'s path
///   was never walked at all; only `PathType` was.
const OCCURRENCE_SURFACE: &str = r#"
pub struct Wrap(pub u32);

pub fn make_wrap() -> Wrap {
    Wrap(1)
}

pub enum Color {
    Red,
    Rgb(u8, u8, u8),
}

pub fn make_color() -> Color {
    Color::Rgb(1, 2, 3)
}

pub fn pick_red() -> Color {
    let c = Color::Red;
    c
}

pub fn g() -> u32 {
    7
}

pub fn ref_g() -> u32 {
    let p = g;
    p()
}

pub fn uses_macro() -> Vec<u32> {
    vec![g()]
}

pub struct P {
    pub x: u32,
}

pub fn make_p() -> P {
    P { x: 0 }
}
"#;

#[test]
fn occurrence_walk_covers_ctor_calls_bare_value_paths_and_struct_literals() {
    let (produced, lineage) = produce_fixture(
        "reference_hardening_occurrences",
        "nudox_occurrence_surface",
        OCCURRENCE_SURFACE,
    );
    assert!(
        produced.report.rejected_facts.is_empty(),
        "the canonical fact sink rejected input: {:?}",
        produced.report.rejected_facts
    );

    let make_wrap = find_intro(&produced, "make_wrap", KindDiscriminant::Function);
    let wrap = find_intro(&produced, "Wrap", KindDiscriminant::Record);
    let make_color = find_intro(&produced, "make_color", KindDiscriminant::Function);
    let rgb = find_intro(&produced, "Rgb", KindDiscriminant::Variant);
    let pick_red = find_intro(&produced, "pick_red", KindDiscriminant::Function);
    let red = find_intro(&produced, "Red", KindDiscriminant::Variant);
    let g = find_intro(&produced, "g", KindDiscriminant::Function);
    let ref_g = find_intro(&produced, "ref_g", KindDiscriminant::Function);
    let uses_macro = find_intro(&produced, "uses_macro", KindDiscriminant::Function);
    let make_p = find_intro(&produced, "make_p", KindDiscriminant::Function);
    let p_struct = find_intro(&produced, "P", KindDiscriminant::Record);

    let has = |owner: IntroId, target: IntroId, kind: ReferenceKind| {
        produced.occurrences.iter().any(|(o, occ)| {
            *o == owner
                && occ.target.package == lineage
                && occ.target.intro == target
                && occ.kind == kind
                && occ.confidence == Confidence::Oracle
        })
    };

    // R2 — tuple-struct constructor call.
    assert!(
        has(make_wrap, wrap, ReferenceKind::FunctionCall),
        "`Wrap(1)` must record an occurrence targeting the `Wrap` struct \
         declaration; got {:?}",
        produced
            .occurrences
            .iter()
            .filter(|(o, _)| *o == make_wrap)
            .collect::<Vec<_>>()
    );

    // R2 — tuple enum-variant constructor call.
    assert!(
        has(make_color, rgb, ReferenceKind::FunctionCall),
        "`Color::Rgb(1, 2, 3)` must record an occurrence targeting the `Rgb` \
         variant declaration; got {:?}",
        produced
            .occurrences
            .iter()
            .filter(|(o, _)| *o == make_color)
            .collect::<Vec<_>>()
    );

    // R5 — bare (non-call) value path naming a unit enum variant.
    assert!(
        has(pick_red, red, ReferenceKind::VariableUse),
        "`let c = Color::Red;` must record an occurrence targeting the `Red` \
         variant declaration; got {:?}",
        produced
            .occurrences
            .iter()
            .filter(|(o, _)| *o == pick_red)
            .collect::<Vec<_>>()
    );

    // R5 — bare (non-call) value path naming a function.
    assert!(
        has(ref_g, g, ReferenceKind::VariableUse),
        "`let p = g;` must record an occurrence targeting `g`'s declaration \
         as a `VariableUse` (not, e.g., double-counted as a `FunctionCall`); \
         got {:?}",
        produced
            .occurrences
            .iter()
            .filter(|(o, _)| *o == ref_g)
            .collect::<Vec<_>>()
    );
    // Exactly once, not duplicated — `p` also appears as the callee of the
    // later `p()`, but `p` there is a local binding (`PathResolution::Local`,
    // not a `Def`), so it produces no occurrence at all; the count below
    // isolates the `let p = g;` occurrence from any accidental double-walk.
    assert_eq!(
        produced
            .occurrences
            .iter()
            .filter(|(o, occ)| *o == ref_g
                && occ.target.intro == g
                && occ.kind == ReferenceKind::VariableUse)
            .count(),
        1,
        "`let p = g;` must be recorded exactly once, not duplicated"
    );

    // R3 — call inside an unexpanded macro's argument tokens (best-effort).
    assert!(
        has(uses_macro, g, ReferenceKind::FunctionCall),
        "`vec![g()]` must record an occurrence targeting `g`'s declaration \
         even though `g()` only exists inside the macro's expanded body; \
         got {:?}",
        produced
            .occurrences
            .iter()
            .filter(|(o, _)| *o == uses_macro)
            .collect::<Vec<_>>()
    );

    // R6 — struct-literal type reference.
    assert!(
        has(make_p, p_struct, ReferenceKind::TypeReference),
        "`P {{ x: 0 }}` must record an occurrence targeting the `P` struct \
         declaration; got {:?}",
        produced
            .occurrences
            .iter()
            .filter(|(o, _)| *o == make_p)
            .collect::<Vec<_>>()
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// R1 / R4 / R7 / R8 / R9 — type lowering
// ═══════════════════════════════════════════════════════════════════════════

/// * `fn widget_return() -> Widget` — `Widget` is declared nowhere in this
///   crate or any dependency (R1): the return type must classify as
///   `UnresolvedLocalName`, not `UnresolvedExternal`.
/// * `trait Foo {} trait Bar { fn bar(&self); } impl Bar for dyn Foo { .. }`
///   — the impl's `self_ty` is a HIR-only `dyn Trait` with no backing AST
///   node other than the impl header (R4): it must lower to
///   `Type::DynTrait([Nominal(Foo)])`, not `NoIrRepresentation`.
/// * `trait Tr { type A; } fn assoc<T: Tr>() -> <T as Tr>::A` — the
///   `<T as Trait>::Assoc` qualified path over a *local* trait (R7): the
///   `trait_ref` must resolve to `Nominal(Tr)`, and `assoc` must stay `"A"`.
/// * `fn opt() -> Option<usize>` — prelude foreign key guard (R8).
/// * `fn nested<T, E>() -> Option<Vec<Result<T, E>>>` — nested-generic guard
///   (R9).
const TYPE_SURFACE: &str = r#"
pub fn widget_return() -> Widget {
    unimplemented!()
}

pub trait Foo {}

pub trait Bar {
    fn bar(&self);
}

impl Bar for dyn Foo {
    fn bar(&self) {}
}

pub trait Tr {
    type A;
}

pub fn assoc<T: Tr>() -> <T as Tr>::A {
    unimplemented!()
}

pub fn opt() -> Option<usize> {
    None
}

pub fn nested<T, E>() -> Option<Vec<Result<T, E>>> {
    None
}
"#;

#[test]
fn unresolved_local_names_dyn_trait_self_ty_and_qualified_assoc_are_correct() {
    let (produced, _lineage) = produce_fixture(
        "reference_hardening_types",
        "nudox_type_surface",
        TYPE_SURFACE,
    );
    assert!(
        produced.report.rejected_facts.is_empty(),
        "the canonical fact sink rejected input: {:?}",
        produced.report.rejected_facts
    );

    let types = all_types(&produced);
    assert!(
        !types.is_empty(),
        "the fixture produced no types at all — the harness, not the \
         invariant, is broken"
    );

    // ── R1 ────────────────────────────────────────────────────────────────
    let widget_gaps: Vec<&Type> = types
        .iter()
        .filter(|t| {
            matches!(
                t,
                Type::Unknown(UnknownType::UnresolvedLocalName { name }) if name == "Widget"
            )
        })
        .collect();
    assert!(
        !widget_gaps.is_empty(),
        "`fn widget_return() -> Widget` (with no `Widget` declared anywhere) \
         must classify as `UnresolvedLocalName {{ name: \"Widget\" }}` — it \
         names nothing outside the crate, so nothing here could resolve it \
         with cross-package linking; got {:?}",
        types
            .iter()
            .filter(|t| matches!(t, Type::Unknown(_)))
            .collect::<Vec<_>>()
    );
    assert!(
        !types.iter().any(|t| matches!(
            t,
            Type::Unknown(UnknownType::UnresolvedExternal { name }) if name == "Widget"
        )),
        "`Widget` must not also (or instead) appear as `UnresolvedExternal` — \
         that would wrongly claim it names something outside the crate"
    );

    // ── R4 ────────────────────────────────────────────────────────────────
    let foo_trait = find_intro(&produced, "Foo", KindDiscriminant::Trait);
    let dyn_foo_ok = types.iter().any(|t| match t {
        Type::DynTrait(members) => {
            members.len() == 1
                && matches!(
                    &members[0],
                    Type::Nominal(Ref::Intro(id)) if *id == foo_trait
                )
        }
        _ => false,
    });
    assert!(
        dyn_foo_ok,
        "`impl Bar for dyn Foo {{ .. }}`'s `self_ty` must lower to \
         `Type::DynTrait([Nominal(Ref::Intro(<Foo>))])`, not \
         `NoIrRepresentation`; got {:?}",
        types
            .iter()
            .filter(|t| matches!(t, Type::DynTrait(_) | Type::Unknown(_)))
            .collect::<Vec<_>>()
    );

    // ── R7 ────────────────────────────────────────────────────────────────
    let tr_trait = find_intro(&produced, "Tr", KindDiscriminant::Trait);
    let qualified_ok = types.iter().any(|t| match t {
        Type::QualifiedPath {
            trait_ref: Some(tr),
            assoc,
            ..
        } => {
            assoc == "A"
                && matches!(
                    tr.as_ref(),
                    Type::Nominal(Ref::Intro(id)) if *id == tr_trait
                )
        }
        _ => false,
    });
    assert!(
        qualified_ok,
        "`<T as Tr>::A` over the local trait `Tr` must lower to \
         `QualifiedPath {{ trait_ref: Some(Nominal(Ref::Intro(<Tr>))), \
         assoc: \"A\", .. }}`; got {:?}",
        types
            .iter()
            .filter(|t| matches!(t, Type::QualifiedPath { .. }))
            .collect::<Vec<_>>()
    );

    // ── R8 (guard) ────────────────────────────────────────────────────────
    let option_foreign_ok = types.iter().any(|t| match t {
        Type::Apply { base, args } => match base.as_ref() {
            Type::Nominal(Ref::Foreign { key, .. }) => {
                args.len() == 1
                    && key.display.as_ref() == "Option"
                    && matches!(
                        &key.origin,
                        ForeignOrigin::Package(lineage)
                            if lineage.ecosystem.as_str() == "rust-sysroot"
                                && lineage.name.as_str() == "core"
                    )
            }
            _ => false,
        },
        _ => false,
    });
    assert!(
        option_foreign_ok,
        "`Option<usize>`'s base must be `Nominal(Ref::Foreign {{ key: \
         ForeignKey {{ origin: Package(rust-sysroot:core), display: \
         \"Option\", .. }}, .. }})`; got {:?}",
        types
            .iter()
            .filter(|t| matches!(t, Type::Apply { .. }))
            .collect::<Vec<_>>()
    );

    // ── R9 (guard) ────────────────────────────────────────────────────────
    let nested_ok = types.iter().any(|t| match t {
        Type::Apply { base, args } if args.len() == 1 => {
            let Type::Nominal(Ref::Foreign { key: outer_key, .. }) = base.as_ref() else {
                return false;
            };
            if outer_key.display.as_ref() != "Option" {
                return false;
            }
            let Type::Apply {
                base: vec_base,
                args: vec_args,
            } = &args[0]
            else {
                return false;
            };
            if vec_args.len() != 1 {
                return false;
            }
            let Type::Nominal(Ref::Foreign { key: vec_key, .. }) = vec_base.as_ref() else {
                return false;
            };
            if vec_key.display.as_ref() != "Vec" {
                return false;
            }
            let Type::Apply {
                base: result_base,
                args: result_args,
            } = &vec_args[0]
            else {
                return false;
            };
            if result_args.len() != 2 {
                return false;
            }
            let Type::Nominal(Ref::Foreign { key: result_key, .. }) = result_base.as_ref() else {
                return false;
            };
            result_key.display.as_ref() == "Result"
                && matches!(&result_args[0], Type::TypeVar(n) if n == "T")
                && matches!(&result_args[1], Type::TypeVar(n) if n == "E")
        }
        _ => false,
    });
    assert!(
        nested_ok,
        "`Option<Vec<Result<T, E>>>` must lower to three nested `Apply`s, \
         each with a `Nominal` base (`Option`, `Vec`, `Result`) and the \
         innermost leaves as `TypeVar(\"T\")`/`TypeVar(\"E\")`; got {:?}",
        types
            .iter()
            .filter(|t| matches!(t, Type::Apply { .. }))
            .collect::<Vec<_>>()
    );
}
