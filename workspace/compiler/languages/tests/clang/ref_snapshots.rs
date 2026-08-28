//! GUARD snapshot: locks the full lowered-IR reference-graph shape for the
//! C/C++ (clang) producer.
//!
//! # Why a snapshot, and why THIS renderer
//!
//! A count-based assertion ("N occurrences", "table has M entries") cannot
//! see a field swap inside a resolved `Ref` — see
//! `docs/AGENTS-DOCTRINE.md`'s "count-based tests cannot see field loss".
//! This is the exact historical shape of bug this snapshot exists to catch a
//! regression of: before the fix documented at the top of
//! `tests/clang/refs_resolution.rs` (and in `src/clang/lower.rs`'s
//! "Reference resolution" module doc), `extract::resolve_type` kept only a
//! `Named` type's *display name* and discarded the declaration's USR, so
//! `lower_type` — which had no access to the `Lowering` sink at all —
//! blanket-mapped every zero-arg `OracleType::Named` to
//! `Type::unresolved_external`, even a same-package `struct` field one line
//! above its own declaration. A count-based test ("N fields", "M types")
//! cannot see that regression: the field is still there, it just silently
//! stopped pointing at anything. This test instead renders EVERY `Type`
//! reachable from the sealed table (fields, super-types, alias targets,
//! template applications) plus every resolved occurrence, explicitly
//! spelling out `Ref::Intro` (by the target's resolved NAME, not raw
//! `IntroId` hex) and `Ref::Foreign` (origin + path + display + link state)
//! so any regression in a resolved reference surfaces as a snapshot diff,
//! not a passing count.
//!
//! # The fixture
//!
//! `tests/clang/refs_resolution.rs` already covers each of these cases in
//! isolation (`cc1`..`cc9`, `cc5`). This test combines as many of them as
//! coexist into ONE dense package (a shared header + two translation units)
//! and snapshots the WHOLE resulting table + occurrences in one shot:
//!   - an intra-package struct field naming another same-package struct
//!     (`Outer::in: Inner`, mirrors `cc1`)
//!   - a self-referential pointer field (`Node::next: Node*`, mirrors `cc2`)
//!   - base-class inheritance (`Derived : public Base`, mirrors `cc3`)
//!   - a `using` alias naming a same-package struct (`WidgetAlias = Widget`,
//!     mirrors `cc4`)
//!   - an enum-typed field (`Pixel::c: Color`, mirrors `cc7`)
//!   - a genuine class-template application (`Registry::box_: Box<Widget>`),
//!     resolving to `Type::Apply { base: Nominal(Ref::Intro(Box)), args:
//!     [Nominal(Ref::Intro(Widget))] }` — see `extract.rs`'s
//!     `named_decl_usr` doc for the USR-normalization fix that made this
//!     resolve instead of falling back to `Unknown::UnresolvedExternal`
//!   - `std::string` resolving to a NAMED `Ref::Foreign` in the "std"
//!     namespace (`Row::name: std::string`, mirrors `cc9`)
//!   - (bonus) `cc5`'s cross-TU call pattern: `helper` is defined in one
//!     translation unit and declared (via a shared header) and called from
//!     another, producing a `FunctionCall` occurrence in the OCCURRENCES
//!     section
//!
//! Hermetic: written fresh into a `tempfile::TempDir` and run through the
//! full `nudox_languages::produce` pipeline, exactly like
//! `tests/clang/refs_resolution.rs`.
//!
//! Requires libclang at runtime (`ClangProducer::invoke` loads it via
//! `clang-sys`'s `runtime` feature) — a missing libclang here is loud (a
//! panic naming the underlying error), not a quiet green.

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, PackageLineageId, PackageName},
    entry::EntryInner,
    foreign::{ForeignOrigin, Unlinked},
    index::{Indexable, Ref},
    kind::Kind,
    kinds::{
        generics::GenericParam,
        ty::{Primitive, TemplatePart, TupleElement, Type, UnknownType},
    },
};
use nudox_languages::clang::ClangProducer;
use nudox_languages::{PackageSource, Produced, ProducerError, produce};

/// See `refs_resolution.rs`'s `CLANG_SINGLETON` doc: `clang::Clang` allows
/// only one live instance per process, and `cargo test` runs every `#[test]`
/// in this binary concurrently on its own thread. Serialize acquisition
/// across this whole file's tests (there is only one, but the guard is
/// still required in case a future test is added here).
static CLANG_SINGLETON: std::sync::Mutex<()> = std::sync::Mutex::new(());

const PACKAGE: &str = "clang-ref-snapshot-fixture";

const FIXTURE_H: &str = r#"#ifndef FIXTURE_H
#define FIXTURE_H
int helper(int x);
#endif
"#;

const HELPER_IMPL_CPP: &str = r#"#include "fixture.h"

int helper(int x) { return x + 1; }
"#;

const MAIN_CPP: &str = r#"#include "fixture.h"
#include <string>

// Intra-package struct field naming another same-package struct.
struct Inner { int x; };
struct Outer { Inner in; };

// Self-referential pointer field.
struct Node { int val; Node* next; };

// Base-class inheritance.
class Base {};
class Derived : public Base {};

// `using` alias naming a same-package struct.
struct Widget {};
using WidgetAlias = Widget;

// Enum-typed field.
enum class Color { Red };
struct Pixel { Color c; };

// A genuine class-template application. `Box`'s primary-template USR is
// normalized against by `extract.rs`'s `named_decl_usr` (see its doc), so the
// `Apply` base resolves to `Ref::Intro(Box)` and the argument resolves to
// `Ref::Intro(Widget)` — mirrors `refs_resolution.rs`'s `cc10`.
template<class T> struct Box { T inner; };
struct Registry { Box<Widget> box_; };

// `std::string` — must resolve to a named Ref::Foreign in the "std"
// namespace, never a bare unresolved gap.
struct Row { std::string name; };

// Cross-TU call: `helper` is defined in helper_impl.cpp, declared via the
// shared header, and called here.
int caller(int x) { return helper(x); }
"#;

fn lineage(name: &str) -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cpp"), PackageName::new(name))
}

/// Write `files` (each a package-relative path + contents) into a fresh
/// tempdir and run the full pipeline over it. Mirrors
/// `refs_resolution.rs`'s `produce_fixture`.
fn produce_fixture(files: &[(&str, &str)], name: &str) -> Result<Produced, ProducerError> {
    let dir = tempfile::tempdir().expect("tempdir");
    for (rel, body) in files {
        let path = dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture subdir");
        }
        std::fs::write(&path, body).expect("write fixture file");
    }
    let source = PackageSource::new(dir.path(), name, "0.0.0");
    produce(&ClangProducer::new(), &source, &lineage(name), &Unlinked)
}

/// The whole `#[source]` chain, so a hermetic parse failure is diagnosable
/// rather than a bare `LoweringFailed`. Mirrors `refs_resolution.rs`'s
/// `chain`.
fn chain(err: &ProducerError) -> String {
    let mut links: Vec<String> = vec![err.to_string()];
    let mut cause = std::error::Error::source(err);
    while let Some(c) = cause {
        links.push(c.to_string());
        cause = c.source();
    }
    links.join(" <- ")
}

fn lower_fixture(files: &[(&str, &str)], name: &str) -> Produced {
    produce_fixture(files, name)
        .unwrap_or_else(|err| panic!("clang producer failed on `{name}`: {}", chain(&err)))
}

// ── Renderer: resolved Ref/Type shape, never a raw hash, never a count ──────
//
// This exact renderer (render_type/render_ref/render_generic/render_table)
// is duplicated verbatim across tests/{rust,go,typescript,java,csharp,clang}
// ref_snapshots.rs — each `[[test]]` binary is compiled independently and
// this crate's test tree has no shared test-support module, so duplication
// here is deliberate, not an oversight.

fn render_primitive(p: &Primitive, table: &PristineIntroTable) -> String {
    match p {
        Primitive::Integer { signed, width } => format!("Integer(signed={signed}, {width:?})"),
        Primitive::Float(w) => format!("Float({w:?})"),
        Primitive::Bool => "Bool".to_owned(),
        Primitive::Char => "Char".to_owned(),
        Primitive::Str => "Str".to_owned(),
        Primitive::MutPointer(inner) => format!("MutPointer({})", render_type(inner, table)),
        Primitive::ConstPointer(inner) => format!("ConstPointer({})", render_type(inner, table)),
        Primitive::Reference { lifetime, mutable, ty } => format!(
            "Reference(lifetime={lifetime:?}, mutable={mutable}, {})",
            render_type(ty, table)
        ),
        Primitive::Builtin(name) => format!("Builtin({name:?})"),
    }
}

fn render_ref<T: Indexable>(r: &Ref<T>, table: &PristineIntroTable) -> String {
    match r {
        Ref::Local(_) => "BUG:UNSEALED-LOCAL-REF".to_owned(),
        Ref::Intro(id) => match table.get(*id) {
            Some(entry) => format!("Intro(`{}` {:?})", entry.sym().name, entry.kind().discriminant()),
            None => format!("Intro(DANGLING {})", id.to_hex()),
        },
        Ref::Foreign { key, target } => {
            let origin = match &key.origin {
                ForeignOrigin::Package(l) => format!("Package({}/{})", l.ecosystem.as_str(), l.name.as_str()),
                ForeignOrigin::Namespace { ecosystem, namespace } => format!("Namespace({}::{})", ecosystem.as_str(), namespace),
                ForeignOrigin::Universe { ecosystem } => format!("Universe({})", ecosystem.as_str()),
            };
            let linked = match target {
                Some(t) => format!("linked(package={}/{}, intro={})", t.package.ecosystem.as_str(), t.package.name.as_str(), t.intro.to_hex()),
                None => "unlinked".to_owned(),
            };
            format!("Foreign(origin={origin}, path={:?}, display={:?}, kind={:?}, {linked})", key.path, key.display, key.kind)
        }
    }
}

fn render_type(ty: &Type, table: &PristineIntroTable) -> String {
    match ty {
        Type::SelfType => "SelfType".to_owned(),
        Type::Primitive(p) => render_primitive(p, table),
        Type::Tuple(elems) => {
            let parts: Vec<String> = elems.iter().map(|e| match e {
                TupleElement::Positional(t) => render_type(t, table),
                TupleElement::Named { label, ty } => format!("{label}: {}", render_type(ty, table)),
            }).collect();
            format!("Tuple({})", parts.join(", "))
        }
        Type::Slice(inner) => format!("Slice({})", render_type(inner, table)),
        Type::Array { ty, length } => format!("Array({}; {length})", render_type(ty, table)),
        Type::Union(list) => format!("Union({})", list.iter().map(|t| render_type(t, table)).collect::<Vec<_>>().join(" | ")),
        Type::Intersection(list) => format!("Intersection({})", list.iter().map(|t| render_type(t, table)).collect::<Vec<_>>().join(" & ")),
        Type::Never => "Never".to_owned(),
        Type::Any => "Any".to_owned(),
        Type::Unknown(u) => match u {
            UnknownType::Unannotated => "Unknown(Unannotated)".to_owned(),
            UnknownType::DynamicallyTyped => "Unknown(DynamicallyTyped)".to_owned(),
            UnknownType::UnresolvedLocalName { name } => format!("Unknown(UnresolvedLocalName {name:?})"),
            UnknownType::UnresolvedExternal { name } => format!("Unknown(UnresolvedExternal {name:?})"),
            UnknownType::TruncatedAtDepthLimit => "Unknown(TruncatedAtDepthLimit)".to_owned(),
            UnknownType::OracleGap => "Unknown(OracleGap)".to_owned(),
            UnknownType::NoIrRepresentation { construct } => format!("Unknown(NoIrRepresentation {construct:?})"),
        },
        Type::Nominal(r) => render_ref(r, table),
        Type::Apply { base, args } => format!(
            "Apply({}<{}>)", render_type(base, table),
            args.iter().map(|a| render_type(a, table)).collect::<Vec<_>>().join(", ")
        ),
        Type::TypeVar(name) => format!("TypeVar({name:?})"),
        Type::Wildcard { variance, bound } => format!(
            "Wildcard({variance:?}, {})",
            bound.as_deref().map(|b| render_type(b, table)).unwrap_or_else(|| "none".to_owned())
        ),
        Type::FunctionPointer { params, ret, abi } => format!(
            "FunctionPointer(({}) -> {}, abi={abi:?})",
            params.iter().map(|p| render_type(p, table)).collect::<Vec<_>>().join(", "),
            ret.as_deref().map(|r| render_type(r, table)).unwrap_or_else(|| "void".to_owned())
        ),
        Type::Annotated { inner, annotation } => format!(
            "Annotated({}, {}{})", render_type(inner, table), annotation.token,
            annotation.arg.as_deref().map(|a| format!("({a})")).unwrap_or_default()
        ),
        Type::Conditional { check, extends_ty, then_ty, else_ty } => format!(
            "Conditional({} extends {} ? {} : {})",
            render_type(check, table), render_type(extends_ty, table),
            render_type(then_ty, table), render_type(else_ty, table)
        ),
        Type::Mapped { key_var, source, value, readonly, optional } => format!(
            "Mapped(key_var={key_var:?}, source={}, value={}, readonly={readonly:?}, optional={optional:?})",
            render_type(source, table), render_type(value, table)
        ),
        Type::TemplateLiteral(parts) => format!(
            "TemplateLiteral({})",
            parts.iter().map(|p| match p {
                TemplatePart::Literal(s) => format!("{s:?}"),
                TemplatePart::Interpolated(t) => format!("${{{}}}", render_type(t, table)),
            }).collect::<Vec<_>>().join("")
        ),
        Type::AnonymousRecord { form, members } => format!(
            "AnonymousRecord({form:?}, [{}])",
            members.iter().map(|m| format!(
                "{}{}{}: {}",
                if m.readonly { "readonly " } else { "" }, m.name,
                if m.optional { "?" } else { "" }, render_type(&m.ty, table)
            )).collect::<Vec<_>>().join(", ")
        ),
        Type::ImplTrait(bounds) => format!("ImplTrait({})", bounds.iter().map(|b| render_type(b, table)).collect::<Vec<_>>().join(" + ")),
        Type::DynTrait(bounds) => format!("DynTrait({})", bounds.iter().map(|b| render_type(b, table)).collect::<Vec<_>>().join(" + ")),
        Type::Inferred => "Inferred".to_owned(),
        Type::QualifiedPath { self_ty, trait_ref, assoc } => format!(
            "QualifiedPath(<{} as {}>::{assoc})", render_type(self_ty, table),
            trait_ref.as_deref().map(|t| render_type(t, table)).unwrap_or_else(|| "_".to_owned())
        ),
    }
}

fn render_generic(g: &GenericParam, table: &PristineIntroTable) -> String {
    match g {
        GenericParam::Lifetime { name } => format!("Lifetime({name:?})"),
        GenericParam::Type { name, bounds, default, variance } => format!(
            "Type({name:?}, bounds=[{}], default={}, variance={variance:?})",
            bounds.iter().map(|b| render_type(b, table)).collect::<Vec<_>>().join(", "),
            default.as_ref().map(|d| render_type(d, table)).unwrap_or_else(|| "None".to_owned())
        ),
        GenericParam::Const { name, ty } => format!("Const({name:?}, {})", render_type(ty, table)),
    }
}

fn render_table(table: &PristineIntroTable) -> String {
    let mut entries: Vec<_> = table.iter_sorted().collect();
    entries.sort_by(|(_, a), (_, b)| {
        a.sym().name.cmp(&b.sym().name).then(a.kind().discriminant().cmp(&b.kind().discriminant()))
    });
    let mut out = String::new();
    for (_, entry) in &entries {
        let kind = entry.kind();
        out.push_str(&format!("== {} :: {:?} ==\n", entry.sym().name, kind.discriminant()));
        match kind {
            EntryInner::Owned(Kind::Record(r)) => {
                out.push_str(&format!("  form: {:?}\n", r.form));
                for field_ref in r.fields.iter() { out.push_str(&format!("  field: {}\n", render_ref(field_ref, table))); }
                for st in r.super_types.iter() { out.push_str(&format!("  super_type: {}\n", render_type(st, table))); }
                for g in r.generics.iter() { out.push_str(&format!("  generic: {}\n", render_generic(g, table))); }
            }
            EntryInner::Owned(Kind::Field(f)) => {
                out.push_str(&format!("  ty: {}\n", f.ty.as_ref().map(|t| render_type(t, table)).unwrap_or_else(|| "None".to_owned())));
            }
            EntryInner::Owned(Kind::Function(f)) => {
                out.push_str(&format!("  receiver: {}\n", f.receiver.is_some()));
                for p in f.input_params.iter() { out.push_str(&format!("  input_param: {}\n", render_ref(p, table))); }
                for p in f.output_params.iter() { out.push_str(&format!("  output_param: {}\n", render_ref(p, table))); }
                for t in f.throws.iter() { out.push_str(&format!("  throws: {}\n", render_type(t, table))); }
                for g in f.generics.iter() { out.push_str(&format!("  generic: {}\n", render_generic(g, table))); }
            }
            EntryInner::Owned(Kind::Param(p)) => {
                out.push_str(&format!("  ty: {}\n", p.ty.as_ref().map(|t| render_type(t, table)).unwrap_or_else(|| "None".to_owned())));
            }
            EntryInner::Owned(Kind::Alias(a)) => {
                out.push_str(&format!("  target: {}\n", a.target.as_ref().map(|t| render_type(t, table)).unwrap_or_else(|| "None".to_owned())));
                for g in a.generics.iter() { out.push_str(&format!("  generic: {}\n", render_generic(g, table))); }
                for b in a.bounds.iter() { out.push_str(&format!("  bound: {}\n", render_type(b, table))); }
            }
            EntryInner::Owned(Kind::Trait(t)) => {
                for s in t.supers.iter() { out.push_str(&format!("  super: {}\n", render_type(s, table))); }
                for g in t.generics.iter() { out.push_str(&format!("  generic: {}\n", render_generic(g, table))); }
            }
            EntryInner::Owned(Kind::Enum(_) | Kind::Variant(_) | Kind::Module(_) | Kind::Impl(_) | Kind::Const(_) | Kind::Static(_) | Kind::Reexport(_)) => {}
            EntryInner::Reference(r) => { out.push_str(&format!("  reference: {}\n", render_ref(r, table))); }
        }
    }
    out
}

fn render_occurrences(produced: &Produced, table: &PristineIntroTable) -> String {
    let name_of = |id: nudox_ir::change::IntroId| -> String {
        table.get(id).map(|e| e.sym().name.clone()).unwrap_or_else(|| format!("<DANGLING {}>", id.to_hex()))
    };
    let mut occs: Vec<_> = produced.occurrences.iter().collect();
    occs.sort_by(|(ow1, o1), (ow2, o2)| {
        name_of(*ow1).cmp(&name_of(*ow2))
            .then_with(|| format!("{:?}", o1.kind).cmp(&format!("{:?}", o2.kind)))
            .then_with(|| name_of(o1.target.intro).cmp(&name_of(o2.target.intro)))
    });
    let mut out = String::new();
    for (owner, occ) in occs {
        out.push_str(&format!(
            "{} --{:?}({:?})--> package={}/{} target={}\n",
            name_of(*owner), occ.kind, occ.confidence,
            occ.target.package.ecosystem.as_str(), occ.target.package.name.as_str(),
            name_of(occ.target.intro),
        ));
    }
    out
}

#[test]
fn clang_reference_graph_snapshot() {
    let _guard = CLANG_SINGLETON
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let produced = lower_fixture(
        &[
            ("fixture.h", FIXTURE_H),
            ("helper_impl.cpp", HELPER_IMPL_CPP),
            ("main.cpp", MAIN_CPP),
        ],
        PACKAGE,
    );

    let table_render = render_table(&produced.table);
    let occurrence_render = render_occurrences(&produced, &produced.table);
    insta::assert_snapshot!(
        "clang_reference_graph",
        format!("── TABLE ──\n{table_render}\n── OCCURRENCES ──\n{occurrence_render}")
    );
}
