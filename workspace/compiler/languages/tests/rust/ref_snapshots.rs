//! GUARD snapshot: locks the full lowered-IR reference-graph shape for the
//! Rust producer.
//!
//! # Why a snapshot, and why THIS renderer
//!
//! A count-based assertion ("N occurrences", "table has M entries") cannot
//! see a field swap inside a resolved `Ref` — see
//! `docs/AGENTS-DOCTRINE.md`'s "count-based tests cannot see field loss".
//! This test instead renders EVERY `Type` reachable from the sealed table
//! (fields, params, returns, supertypes, alias targets) plus every resolved
//! occurrence, explicitly spelling out `Ref::Intro` (by the target's
//! resolved NAME, not raw `IntroId` hex) and `Ref::Foreign` (origin + path +
//! display + link state) so any regression in a resolved reference surfaces
//! as a snapshot diff, not a passing count.
//!
//! `IntroId` is a content hash, so it IS stable across runs given identical
//! source — but the renderer still avoids printing it directly (except as a
//! last-resort fallback for a dangling ref, which should never happen and
//! would be a loud snapshot diff if it ever did) so the snapshot stays
//! human-reviewable.
//!
//! # The fixture
//!
//! One tiny hermetic crate (built via the same `produce(&RustProducer{
//! direct_repo:false}, ..)` harness as `tests/rust/universal_pipeline.rs`)
//! exercising:
//!   - prelude generics: `Option<Vec<Result<T, E>>>`
//!   - a `dyn Foo` trait-object return type (same-crate `Ref::Intro`)
//!   - `<T as Iterator>::Item` (a `QualifiedPath` over a foreign trait)
//!   - a tuple-struct constructor call (`Widget(x)`)
//!   - a same-crate nominal type reference (`Holder::widget: Widget`)
//!
//! rust-analyzer is slow, so the fixture is deliberately tiny.

use nudox_ir::{
    apply::PristineIntroTable,
    entry::EntryInner,
    foreign::ForeignOrigin,
    index::{Indexable, Ref},
    kind::Kind,
    kinds::{
        generics::GenericParam,
        ty::{Primitive, TemplatePart, TupleElement, Type, UnknownType},
    },
};
use nudox_languages::rust::RustProducer;
use nudox_languages::{PackageSource, Produced, produce};

const PACKAGE: &str = "nudox_ref_snapshot_fixture";

const LIB_RS: &str = r#"
pub trait Foo {
    fn bar(&self) -> i32;
}

pub struct Impl;

impl Foo for Impl {
    fn bar(&self) -> i32 {
        0
    }
}

pub fn make_dyn() -> Box<dyn Foo> {
    Box::new(Impl)
}

pub fn parse_all(input: Vec<String>) -> Option<Vec<Result<i32, std::num::ParseIntError>>> {
    Some(input.iter().map(|s| s.parse::<i32>()).collect())
}

pub fn first_item<T: Iterator>(mut it: T) -> Option<<T as Iterator>::Item> {
    it.next()
}

pub struct Widget(pub u32);

pub fn make_widget(x: u32) -> Widget {
    Widget(x)
}

pub struct Holder {
    pub widget: Widget,
}
"#;

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create temporary Rust package");
    std::fs::create_dir_all(dir.path().join("src")).expect("create fixture src/");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        format!(
            "[package]\nname = \"{PACKAGE}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n"
        ),
    )
    .expect("write fixture manifest");
    std::fs::write(dir.path().join("src/lib.rs"), LIB_RS).expect("write fixture source");
    dir
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
        Primitive::Reference {
            lifetime,
            mutable,
            ty,
        } => format!(
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
            Some(entry) => format!(
                "Intro(`{}` {:?})",
                entry.sym().name,
                entry.kind().discriminant()
            ),
            None => format!("Intro(DANGLING {})", id.to_hex()),
        },
        Ref::Foreign { key, target } => {
            let origin = match &key.origin {
                ForeignOrigin::Package(l) => {
                    format!("Package({}/{})", l.ecosystem.as_str(), l.name.as_str())
                }
                ForeignOrigin::Namespace {
                    ecosystem,
                    namespace,
                } => format!("Namespace({}::{})", ecosystem.as_str(), namespace),
                ForeignOrigin::Universe { ecosystem } => {
                    format!("Universe({})", ecosystem.as_str())
                }
            };
            let linked = match target {
                Some(t) => format!(
                    "linked(package={}/{}, intro={})",
                    t.package.ecosystem.as_str(),
                    t.package.name.as_str(),
                    t.intro.to_hex()
                ),
                None => "unlinked".to_owned(),
            };
            format!(
                "Foreign(origin={origin}, path={:?}, display={:?}, kind={:?}, {linked})",
                key.path, key.display, key.kind
            )
        }
    }
}

fn render_type(ty: &Type, table: &PristineIntroTable) -> String {
    match ty {
        Type::SelfType => "SelfType".to_owned(),
        Type::Primitive(p) => render_primitive(p, table),
        Type::Tuple(elems) => {
            let parts: Vec<String> = elems
                .iter()
                .map(|e| match e {
                    TupleElement::Positional(t) => render_type(t, table),
                    TupleElement::Named { label, ty } => {
                        format!("{label}: {}", render_type(ty, table))
                    }
                })
                .collect();
            format!("Tuple({})", parts.join(", "))
        }
        Type::Slice(inner) => format!("Slice({})", render_type(inner, table)),
        Type::Array { ty, length } => format!("Array({}; {length})", render_type(ty, table)),
        Type::Union(list) => format!(
            "Union({})",
            list.iter()
                .map(|t| render_type(t, table))
                .collect::<Vec<_>>()
                .join(" | ")
        ),
        Type::Intersection(list) => format!(
            "Intersection({})",
            list.iter()
                .map(|t| render_type(t, table))
                .collect::<Vec<_>>()
                .join(" & ")
        ),
        Type::Never => "Never".to_owned(),
        Type::Any => "Any".to_owned(),
        Type::Unknown(u) => match u {
            UnknownType::Unannotated => "Unknown(Unannotated)".to_owned(),
            UnknownType::DynamicallyTyped => "Unknown(DynamicallyTyped)".to_owned(),
            UnknownType::UnresolvedLocalName { name } => {
                format!("Unknown(UnresolvedLocalName {name:?})")
            }
            UnknownType::UnresolvedExternal { name } => {
                format!("Unknown(UnresolvedExternal {name:?})")
            }
            UnknownType::TruncatedAtDepthLimit => "Unknown(TruncatedAtDepthLimit)".to_owned(),
            UnknownType::OracleGap => "Unknown(OracleGap)".to_owned(),
            UnknownType::NoIrRepresentation { construct } => {
                format!("Unknown(NoIrRepresentation {construct:?})")
            }
        },
        Type::Nominal(r) => render_ref(r, table),
        Type::Apply { base, args } => format!(
            "Apply({}<{}>)",
            render_type(base, table),
            args.iter()
                .map(|a| render_type(a, table))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::TypeVar(name) => format!("TypeVar({name:?})"),
        Type::Wildcard { variance, bound } => format!(
            "Wildcard({variance:?}, {})",
            bound
                .as_deref()
                .map(|b| render_type(b, table))
                .unwrap_or_else(|| "none".to_owned())
        ),
        Type::FunctionPointer { params, ret, abi } => format!(
            "FunctionPointer(({}) -> {}, abi={abi:?})",
            params
                .iter()
                .map(|p| render_type(p, table))
                .collect::<Vec<_>>()
                .join(", "),
            ret.as_deref()
                .map(|r| render_type(r, table))
                .unwrap_or_else(|| "void".to_owned())
        ),
        Type::Annotated { inner, annotation } => format!(
            "Annotated({}, {}{})",
            render_type(inner, table),
            annotation.token,
            annotation
                .arg
                .as_deref()
                .map(|a| format!("({a})"))
                .unwrap_or_default()
        ),
        Type::Conditional {
            check,
            extends_ty,
            then_ty,
            else_ty,
        } => format!(
            "Conditional({} extends {} ? {} : {})",
            render_type(check, table),
            render_type(extends_ty, table),
            render_type(then_ty, table),
            render_type(else_ty, table)
        ),
        Type::Mapped {
            key_var,
            source,
            value,
            readonly,
            optional,
        } => format!(
            "Mapped(key_var={key_var:?}, source={}, value={}, readonly={readonly:?}, optional={optional:?})",
            render_type(source, table),
            render_type(value, table)
        ),
        Type::TemplateLiteral(parts) => format!(
            "TemplateLiteral({})",
            parts
                .iter()
                .map(|p| match p {
                    TemplatePart::Literal(s) => format!("{s:?}"),
                    TemplatePart::Interpolated(t) => format!("${{{}}}", render_type(t, table)),
                })
                .collect::<Vec<_>>()
                .join("")
        ),
        Type::AnonymousRecord { form, members } => format!(
            "AnonymousRecord({form:?}, [{}])",
            members
                .iter()
                .map(|m| format!(
                    "{}{}{}: {}",
                    if m.readonly { "readonly " } else { "" },
                    m.name,
                    if m.optional { "?" } else { "" },
                    render_type(&m.ty, table)
                ))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Type::ImplTrait(bounds) => format!(
            "ImplTrait({})",
            bounds
                .iter()
                .map(|b| render_type(b, table))
                .collect::<Vec<_>>()
                .join(" + ")
        ),
        Type::DynTrait(bounds) => format!(
            "DynTrait({})",
            bounds
                .iter()
                .map(|b| render_type(b, table))
                .collect::<Vec<_>>()
                .join(" + ")
        ),
        Type::Inferred => "Inferred".to_owned(),
        Type::QualifiedPath {
            self_ty,
            trait_ref,
            assoc,
        } => format!(
            "QualifiedPath(<{} as {}>::{assoc})",
            render_type(self_ty, table),
            trait_ref
                .as_deref()
                .map(|t| render_type(t, table))
                .unwrap_or_else(|| "_".to_owned())
        ),
    }
}

fn render_generic(g: &GenericParam, table: &PristineIntroTable) -> String {
    match g {
        GenericParam::Lifetime { name } => format!("Lifetime({name:?})"),
        GenericParam::Type {
            name,
            bounds,
            default,
            variance,
        } => format!(
            "Type({name:?}, bounds=[{}], default={}, variance={variance:?})",
            bounds
                .iter()
                .map(|b| render_type(b, table))
                .collect::<Vec<_>>()
                .join(", "),
            default
                .as_ref()
                .map(|d| render_type(d, table))
                .unwrap_or_else(|| "None".to_owned())
        ),
        GenericParam::Const { name, ty } => format!("Const({name:?}, {})", render_type(ty, table)),
    }
}

/// Every entry in the sealed table, sorted by (name, kind) — NOT by
/// `IntroId` — so the snapshot's line order stays stable even if an
/// unrelated content-hash bit shifts; only real field content should ever
/// move a line.
fn render_table(table: &PristineIntroTable) -> String {
    let mut entries: Vec<_> = table.iter_sorted().collect();
    entries.sort_by(|(_, a), (_, b)| {
        a.sym()
            .name
            .cmp(&b.sym().name)
            .then(a.kind().discriminant().cmp(&b.kind().discriminant()))
    });

    let mut out = String::new();
    for (_, entry) in &entries {
        let kind = entry.kind();
        out.push_str(&format!(
            "== {} :: {:?} ==\n",
            entry.sym().name,
            kind.discriminant()
        ));
        match kind {
            EntryInner::Owned(Kind::Record(r)) => {
                out.push_str(&format!("  form: {:?}\n", r.form));
                for field_ref in r.fields.iter() {
                    out.push_str(&format!("  field: {}\n", render_ref(field_ref, table)));
                }
                for st in r.super_types.iter() {
                    out.push_str(&format!("  super_type: {}\n", render_type(st, table)));
                }
                for g in r.generics.iter() {
                    out.push_str(&format!("  generic: {}\n", render_generic(g, table)));
                }
            }
            EntryInner::Owned(Kind::Field(f)) => {
                out.push_str(&format!(
                    "  ty: {}\n",
                    f.ty.as_ref()
                        .map(|t| render_type(t, table))
                        .unwrap_or_else(|| "None".to_owned())
                ));
            }
            EntryInner::Owned(Kind::Function(f)) => {
                out.push_str(&format!("  receiver: {}\n", f.receiver.is_some()));
                for p in f.input_params.iter() {
                    out.push_str(&format!("  input_param: {}\n", render_ref(p, table)));
                }
                for p in f.output_params.iter() {
                    out.push_str(&format!("  output_param: {}\n", render_ref(p, table)));
                }
                for t in f.throws.iter() {
                    out.push_str(&format!("  throws: {}\n", render_type(t, table)));
                }
                for g in f.generics.iter() {
                    out.push_str(&format!("  generic: {}\n", render_generic(g, table)));
                }
            }
            EntryInner::Owned(Kind::Param(p)) => {
                out.push_str(&format!(
                    "  ty: {}\n",
                    p.ty.as_ref()
                        .map(|t| render_type(t, table))
                        .unwrap_or_else(|| "None".to_owned())
                ));
            }
            EntryInner::Owned(Kind::Alias(a)) => {
                out.push_str(&format!(
                    "  target: {}\n",
                    a.target
                        .as_ref()
                        .map(|t| render_type(t, table))
                        .unwrap_or_else(|| "None".to_owned())
                ));
                for g in a.generics.iter() {
                    out.push_str(&format!("  generic: {}\n", render_generic(g, table)));
                }
                for b in a.bounds.iter() {
                    out.push_str(&format!("  bound: {}\n", render_type(b, table)));
                }
            }
            EntryInner::Owned(Kind::Trait(t)) => {
                for s in t.supers.iter() {
                    out.push_str(&format!("  super: {}\n", render_type(s, table)));
                }
                for g in t.generics.iter() {
                    out.push_str(&format!("  generic: {}\n", render_generic(g, table)));
                }
            }
            EntryInner::Owned(
                Kind::Enum(_)
                | Kind::Variant(_)
                | Kind::Module(_)
                | Kind::Impl(_)
                | Kind::Const(_)
                | Kind::Static(_)
                | Kind::Reexport(_),
            ) => {
                // No type-bearing fields rendered for this fixture's guard
                // scope; extend here if a future fixture needs to lock one
                // of these too.
            }
            EntryInner::Reference(r) => {
                out.push_str(&format!("  reference: {}\n", render_ref(r, table)));
            }
        }
    }
    out
}

/// Resolved occurrences (body-reference edges), sorted by (owner name, kind,
/// target name) for the same reshuffle-resistance reason as `render_table`.
fn render_occurrences(produced: &Produced, table: &PristineIntroTable) -> String {
    let name_of = |id: nudox_ir::change::IntroId| -> String {
        table
            .get(id)
            .map(|e| e.sym().name.clone())
            .unwrap_or_else(|| format!("<DANGLING {}>", id.to_hex()))
    };
    let mut occs: Vec<_> = produced.occurrences.iter().collect();
    occs.sort_by(|(ow1, o1), (ow2, o2)| {
        name_of(*ow1)
            .cmp(&name_of(*ow2))
            .then_with(|| format!("{:?}", o1.kind).cmp(&format!("{:?}", o2.kind)))
            .then_with(|| name_of(o1.target.intro).cmp(&name_of(o2.target.intro)))
    });
    let mut out = String::new();
    for (owner, occ) in occs {
        out.push_str(&format!(
            "{} --{:?}({:?})--> package={}/{} target={}\n",
            name_of(*owner),
            occ.kind,
            occ.confidence,
            occ.target.package.ecosystem.as_str(),
            occ.target.package.name.as_str(),
            name_of(occ.target.intro),
        ));
    }
    out
}

#[test]
fn rust_reference_graph_snapshot() {
    let fixture = fixture();
    let source = PackageSource::new(fixture.path(), PACKAGE, "0.1.0");
    let lineage = nudox_ir::change::PackageLineageId::new(
        nudox_ir::change::EcosystemId::new("cargo"),
        nudox_ir::change::PackageName::new(PACKAGE.to_owned()),
    );
    let produced = produce(
        &RustProducer { direct_repo: false },
        &source,
        &lineage,
        &nudox_ir::foreign::Unlinked,
    )
    .expect("the self-contained ref-snapshot Rust fixture must lower");

    let table_render = render_table(&produced.table);
    let occurrence_render = render_occurrences(&produced, &produced.table);

    insta::assert_snapshot!(
        "rust_reference_graph",
        format!(
            "── TABLE ──\n{table_render}\n── OCCURRENCES ──\n{occurrence_render}"
        )
    );
}
