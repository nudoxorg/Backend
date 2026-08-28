//! Locks the full lowered-IR reference-graph shape for the Go producer via a
//! single dense insta snapshot, rendering the SEALED reference graph
//! (resolved `Ref::Intro` / `Ref::Foreign` variants) rather than counts — a
//! count-based test cannot see a field swap inside a resolved reference. See
//! `tests/clang/refs_resolution.rs`'s module doc for the rationale this
//! mirrors for Go.
//!
//! The fixture is a real two-package, single-module Go module built and run
//! through the actual `nudox-go-oracle` binary (same pattern as
//! `tests/go/cross_package_implements.rs` and `tests/go/reference_hardening.rs`),
//! covering in one shot:
//!
//! - a `sync.Mutex` FIELD on a struct (stdlib type reference)
//! - a struct EMBEDDING `sync.Mutex` (anonymous/embedded field)
//! - a newtype (`Circle`) in package `core` implementing an interface
//!   (`Shape`) declared in a DIFFERENT package (`shapes`) of the same module
//! - a function returning the builtin `error` type
//! - a plain same-package function call

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};

use nudox_ir::{
    apply::PristineIntroTable,
    change::{EcosystemId, PackageLineageId, PackageName},
    entry::EntryInner,
    foreign::ForeignOrigin,
    index::{Indexable, Ref},
    kind::Kind,
    kinds::{
        generics::GenericParam,
        ty::{Primitive, TemplatePart, TupleElement, Type, UnknownType},
    },
};
use nudox_languages::go::producer::GoProducer;
use nudox_languages::{PackageSource, Produced, produce};

// ── Renderer (verbatim, duplicated across all per-language ref_snapshots.rs) ─

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

// ── Fixture: single Go module, two packages (core + shapes) ─────────────────
//
// - `core.Config.mu`: a plain `sync.Mutex` FIELD (stdlib type reference).
// - `core.Cache`: a struct EMBEDDING `sync.Mutex` (anonymous field).
// - `core.Circle`: a struct satisfying `shapes.Shape` (declared in a
//   DIFFERENT package of the same module) purely structurally, no import —
//   mirrors `cross_package_implements.rs`'s Shape/Circle pattern.
// - `core.Validate`: a function returning the builtin `error` type.
// - `core.CallValidate`: a plain same-package function call to `Validate`.

const MODULE: &str = "example.test/nudox-go-ref-snapshot";

const CORE_GO: &str = r#"package core

import "sync"

// Config has a plain (non-embedded) sync.Mutex field.
type Config struct {
	mu sync.Mutex
}

// Cache embeds sync.Mutex anonymously.
type Cache struct {
	sync.Mutex
	items int
}

// Circle satisfies shapes.Shape purely structurally: a matching
// Area() float64 method, no import of the shapes package.
type Circle struct {
	Radius float64
}

func (c Circle) Area() float64 {
	return 3.14159 * c.Radius * c.Radius
}

// Validate returns the builtin error type.
func Validate(c Config) error {
	return nil
}

// CallValidate is a plain same-package function call to Validate.
func CallValidate(c Config) error {
	return Validate(c)
}
"#;

const SHAPES_GO: &str = r#"package shapes

// Shape is implemented by any type reporting an area. Declared in its own
// package, deliberately separate from anything that satisfies it.
type Shape interface {
	Area() float64
}
"#;

/// Build the oracle binary once, into Cargo's per-test-binary scratch dir —
/// same pattern as `tests/go/cross_package_implements.rs`.
fn oracle_bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let oracle_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("oracle/go");
        let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("nudox-go-oracle-ref-snapshots");
        let status = Command::new("go")
            .current_dir(&oracle_dir)
            .args(["build", "-o"])
            .arg(&out)
            .arg("./...")
            .status()
            .expect("`go` must be available to build the committed Go oracle");
        assert!(status.success(), "go build of the committed oracle failed");
        out
    })
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("create temporary Go module");
    fs::write(
        dir.path().join("go.mod"),
        format!("module {MODULE}\n\ngo 1.22\n"),
    )
    .expect("write go.mod");
    fs::create_dir_all(dir.path().join("core")).expect("create core/ package dir");
    fs::write(dir.path().join("core/core.go"), CORE_GO).expect("write core/core.go");
    fs::create_dir_all(dir.path().join("shapes")).expect("create shapes/ package dir");
    fs::write(dir.path().join("shapes/shape.go"), SHAPES_GO).expect("write shapes/shape.go");
    dir
}

#[test]
fn go_reference_graph_snapshot() {
    let fixture = fixture();
    // SAFETY: this test process owns the oracle invocation and writes one
    // immutable binary path before the producer starts; no other thread in
    // this test binary touches this variable concurrently.
    unsafe { std::env::set_var("NUDOX_GO_ORACLE_BIN", oracle_bin()) };

    let source = PackageSource::new(fixture.path(), MODULE, "0.1.0");
    let lineage = PackageLineageId::new(EcosystemId::new("go"), PackageName::new(MODULE));
    let produced = produce(&GoProducer, &source, &lineage, &nudox_ir::foreign::Unlinked)
        .expect("the self-contained two-package Go module must lower through the oracle");

    let table_render = render_table(&produced.table);
    let occurrence_render = render_occurrences(&produced, &produced.table);
    insta::assert_snapshot!(
        "go_reference_graph",
        format!("── TABLE ──\n{table_render}\n── OCCURRENCES ──\n{occurrence_render}")
    );
}
