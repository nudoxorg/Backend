//! Locks the FULL lowered-IR reference-graph shape for the TypeScript
//! producer, in one dense fixture, via `insta`.
//!
//! `tests/typescript/refs_resolution.rs` (T1-T10) each probe ONE reference
//! kind in isolation by asserting on a single field — a count-based or
//! single-field test cannot see a field swap *inside* a resolved reference
//! (e.g. a `Ref::Foreign`'s `key.path` silently changing while `target`
//! stays populated, or a `Ref::Intro` id silently pointing at the wrong
//! declaration while the enclosing `Option` stays `Some`). This is the
//! project's hard rule on why snapshot tests must render the SEALED
//! reference graph itself — see this crate's `python/refs_resolution.rs`
//! sibling doc and the top of `typescript/refs_resolution.rs` for the
//! historical defect this guards against (the extract layer classifying
//! every named type reference as a generic `TypeVar`, and a sink-less
//! `lower_type` that could only ever emit `UnresolvedExternal`).
//!
//! This test builds ONE npm-style package combining every reference shape
//! `refs_resolution.rs` proves individually — aliased/default/namespace/
//! relative/bare imports, TypeScript's dynamic `import(...)` type syntax, a
//! locally-declared `namespace` with a qualified member reference, and
//! qualified heritage (`extends ns.Base`) — and snapshots the WHOLE
//! resulting intro table plus occurrence list in one shot, so a future
//! change that silently swaps or drops a field anywhere in that graph shows
//! up as a diff.

use std::fs;

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef},
    entry::EntryInner,
    foreign::{ForeignKey, ForeignOrigin, ForeignResolver, Resolution},
    index::{Indexable, Ref},
    kind::Kind,
    kinds::{
        generics::GenericParam,
        ty::{Primitive, TemplatePart, TupleElement, Type, UnknownType},
    },
};
use nudox_languages::typescript::TypescriptProducer;
use nudox_languages::{PackageSource, Produced, produce};

// ── renderer (verbatim, duplicated across all per-language ref_snapshots.rs
// files — no shared test-support module exists in this crate's test tree) ──

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

// ── fixture ──────────────────────────────────────────────────────────────

fn write_pkg(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create npm package tempdir");
    fs::write(
        dir.path().join("package.json"),
        r#"{"name":"refs-fixture","version":"1.0.0","types":"index.d.ts"}"#,
    )
    .expect("write package.json");
    for (name, body) in files {
        fs::write(dir.path().join(name), body).expect("write source file");
    }
    dir
}

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("refs-fixture"))
}

/// Resolves every `external-dep` foreign key this fixture produces to a
/// distinct, stable target — so the snapshot shows several different
/// *linked* `Foreign(...)` entries (one per import shape) rather than one
/// uniform blob, the way `ResolveEveryForeign` in `refs_resolution.rs` would
/// if reused verbatim with a single fixed target. The exact `path` strings
/// matched below come straight from `typescript/emit.rs::ts_foreign_key`
/// (`"{module_request}#{name}"`, keyed on the *export* name, never the
/// use-site local binding — see T1/T2 in `refs_resolution.rs`). Note `ns.Thing`
/// and `import("external-dep").Thing` deliberately produce the SAME key
/// (`"external-dep#Thing"`): both name the identical foreign symbol through
/// different TypeScript syntax, so they resolve to the identical target —
/// that is the join key working as designed, not a fixture bug.
struct KeyedForeignResolver;

impl ForeignResolver for KeyedForeignResolver {
    fn resolve(&self, key: &ForeignKey) -> Resolution {
        let package = PackageLineageId::new(EcosystemId::new("npm"), PackageName::new("external-dep"));
        let byte: u8 = match key.path.as_ref() {
            "external-dep#Foo" => 0x11,
            "external-dep#default" => 0x22,
            "external-dep#Thing" => 0x33,
            "external-dep#Base" => 0x55,
            other => panic!(
                "fixture drifted from `ts_foreign_key`'s path format: unexpected ForeignKey \
                 path {other:?} (display={:?}, origin={:?})",
                key.display, key.origin
            ),
        };
        Resolution::Resolved(StableRef::new(package, IntroId::from_raw([byte; 32])))
    }
}

#[test]
fn typescript_reference_graph_snapshot() {
    let dir = write_pkg(&[
        (
            "sibling.ts",
            "export interface SiblingLocal { id: number; }\n",
        ),
        (
            "index.ts",
            "import { Foo as Bar } from \"external-dep\";\n\
             import Widget from \"external-dep\";\n\
             import * as ns from \"external-dep\";\n\
             import * as m from \"./sibling\";\n\
             \n\
             export namespace NS {\n\
             \x20\x20export interface Foo { id: number; }\n\
             }\n\
             \n\
             export interface Holder {\n\
             \x20\x20aliased: Bar;\n\
             \x20\x20defaulted: Widget;\n\
             \x20\x20namespaced: ns.Thing;\n\
             \x20\x20sibling: m.SiblingLocal;\n\
             \x20\x20dynamic: import(\"external-dep\").Thing;\n\
             \x20\x20nested: NS.Foo;\n\
             }\n\
             \n\
             export class C extends ns.Base {}\n",
        ),
    ]);
    let source = PackageSource::new(dir.path(), "refs-fixture", "1.0.0");
    let produced = produce(
        &TypescriptProducer::new(),
        &source,
        &lineage(),
        &KeyedForeignResolver,
    )
    .expect("the dense TypeScript reference-graph fixture must lower");

    let table_render = render_table(&produced.table);
    let occurrence_render = render_occurrences(&produced, &produced.table);
    insta::assert_snapshot!(
        "typescript_reference_graph",
        format!("── TABLE ──\n{table_render}\n── OCCURRENCES ──\n{occurrence_render}")
    );
}
