//! Locks the full lowered-IR reference-graph shape for the Java producer.
//!
//! Unlike `producer_tests.rs` (which shells out to the real javac/javadoc
//! oracle), this test builds a hand-written JSON `Extraction` — a fabricated
//! stand-in for what the real oracle would emit — and deserializes, lowers,
//! and *seals* it directly, so the assertions run against the real resolved
//! reference graph (`Ref::Intro` / `Ref::Foreign`), not a pre-seal
//! `Ref::Local`. See `tests/csharp/lowering.rs`'s `FIXTURE` const for the
//! sibling pattern this follows in another language.
//!
//! The fixture exercises:
//! - a bounded type-variable method: `<T extends Comparable<T>> T max(T a, T b)`
//!   plus a call-site `Reference` that hits it (`Container#useMax()` ->
//!   `MathUtils#max(T,T)`), proving the raw/erased occurrence-alias
//!   reconciliation resolves through to a real `Occurrence`.
//! - a same-package generic type reference: `Container.boxOfShape: Box<Shape>`,
//!   which must resolve `Shape` to a same-package `Ref::Intro`, not `Type::Any`.
//! - a wildcard type: `Container.shapes: List<? extends Shape>`.
//! - a nested/member type: `Outer.Inner`, with `enclosing` set, verified to
//!   keep a parent link through `PristineIntroTable::parent_of`.
//! - cross-package (JDK) type references: `java.util.List` and
//!   `java.lang.Object`, which must resolve as named `Ref::Foreign` entries
//!   (not erased to `Type::Any`), matching the shape
//!   `gson_type_adapter_and_annotation_richness`'s `write_with_throws`
//!   assertion documents for `java.io.IOException`.

use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
use nudox_ir::entry::{Symbol, Visibility};
use nudox_ir::lower::Lowering;
use nudox_ir::package::{IrPackage, PackageId};
use nudox_languages::java::lower::{JavaId, LoweringCtx, lower_extraction};
use nudox_languages::java::schema::Extraction;

// ── Fabricated oracle JSON (fixture) ────────────────────────────────────────

const FIXTURE: &str = r#"{
  "format": 1,
  "javaVersion": "21.0.1",
  "modules": [],
  "packages": [
    { "name": "com.example.shapes", "doc": null, "docKind": null, "annotations": [], "position": null }
  ],
  "types": [
    {
      "qualifiedName": "com.example.shapes.Shape",
      "simpleName": "Shape",
      "kind": "CLASS",
      "package": "com.example.shapes",
      "module": null,
      "enclosing": null,
      "nesting": "TOP_LEVEL",
      "modifiers": ["public"],
      "typeParams": [],
      "superclass": null,
      "interfaces": [],
      "permits": [],
      "recordComponents": [],
      "annotations": [],
      "deprecated": false,
      "doc": null,
      "docKind": null,
      "position": null,
      "fields": [],
      "enumConstants": [],
      "constructors": [],
      "methods": [],
      "nested": []
    },
    {
      "qualifiedName": "com.example.shapes.Box",
      "simpleName": "Box",
      "kind": "CLASS",
      "package": "com.example.shapes",
      "module": null,
      "enclosing": null,
      "nesting": "TOP_LEVEL",
      "modifiers": ["public"],
      "typeParams": [
        { "name": "T", "bounds": [], "annotations": [] }
      ],
      "superclass": null,
      "interfaces": [],
      "permits": [],
      "recordComponents": [],
      "annotations": [],
      "deprecated": false,
      "doc": null,
      "docKind": null,
      "position": null,
      "fields": [
        {
          "name": "value",
          "type": { "kind": "typevar", "name": "T", "annotations": [] },
          "modifiers": ["private"],
          "constant": null,
          "annotations": [],
          "deprecated": false,
          "origin": "EXPLICIT",
          "doc": null,
          "docKind": null,
          "position": null
        }
      ],
      "enumConstants": [],
      "constructors": [],
      "methods": [],
      "nested": []
    },
    {
      "qualifiedName": "com.example.shapes.MathUtils",
      "simpleName": "MathUtils",
      "kind": "CLASS",
      "package": "com.example.shapes",
      "module": null,
      "enclosing": null,
      "nesting": "TOP_LEVEL",
      "modifiers": ["public"],
      "typeParams": [],
      "superclass": null,
      "interfaces": [],
      "permits": [],
      "recordComponents": [],
      "annotations": [],
      "deprecated": false,
      "doc": null,
      "docKind": null,
      "position": null,
      "fields": [],
      "enumConstants": [],
      "constructors": [],
      "methods": [
        {
          "name": "max",
          "modifiers": ["public", "static"],
          "typeParams": [
            {
              "name": "T",
              "bounds": [
                {
                  "kind": "declared",
                  "name": "java.lang.Comparable",
                  "args": [
                    { "kind": "typevar", "name": "T", "annotations": [] }
                  ],
                  "owner": null,
                  "annotations": []
                }
              ],
              "annotations": []
            }
          ],
          "params": [
            { "name": "a", "type": { "kind": "typevar", "name": "T", "annotations": [] }, "annotations": [] },
            { "name": "b", "type": { "kind": "typevar", "name": "T", "annotations": [] }, "annotations": [] }
          ],
          "return": { "kind": "typevar", "name": "T", "annotations": [] },
          "thrown": [],
          "varargs": false,
          "default": false,
          "receiver": null,
          "annotationDefault": null,
          "annotations": [],
          "deprecated": false,
          "origin": "EXPLICIT",
          "doc": null,
          "docKind": null,
          "position": null
        }
      ],
      "nested": []
    },
    {
      "qualifiedName": "com.example.shapes.Container",
      "simpleName": "Container",
      "kind": "CLASS",
      "package": "com.example.shapes",
      "module": null,
      "enclosing": null,
      "nesting": "TOP_LEVEL",
      "modifiers": ["public"],
      "typeParams": [],
      "superclass": null,
      "interfaces": [],
      "permits": [],
      "recordComponents": [],
      "annotations": [],
      "deprecated": false,
      "doc": null,
      "docKind": null,
      "position": null,
      "fields": [
        {
          "name": "boxOfShape",
          "type": {
            "kind": "declared",
            "name": "com.example.shapes.Box",
            "args": [
              { "kind": "declared", "name": "com.example.shapes.Shape", "args": [], "owner": null, "annotations": [] }
            ],
            "owner": null,
            "annotations": []
          },
          "modifiers": ["private"],
          "constant": null,
          "annotations": [],
          "deprecated": false,
          "origin": "EXPLICIT",
          "doc": null,
          "docKind": null,
          "position": null
        },
        {
          "name": "shapes",
          "type": {
            "kind": "declared",
            "name": "java.util.List",
            "args": [
              {
                "kind": "wildcard",
                "extends": { "kind": "declared", "name": "com.example.shapes.Shape", "args": [], "owner": null, "annotations": [] },
                "super": null
              }
            ],
            "owner": null,
            "annotations": []
          },
          "modifiers": ["private"],
          "constant": null,
          "annotations": [],
          "deprecated": false,
          "origin": "EXPLICIT",
          "doc": null,
          "docKind": null,
          "position": null
        },
        {
          "name": "anything",
          "type": { "kind": "declared", "name": "java.lang.Object", "args": [], "owner": null, "annotations": [] },
          "modifiers": ["private"],
          "constant": null,
          "annotations": [],
          "deprecated": false,
          "origin": "EXPLICIT",
          "doc": null,
          "docKind": null,
          "position": null
        }
      ],
      "enumConstants": [],
      "constructors": [],
      "methods": [
        {
          "name": "useMax",
          "modifiers": ["public"],
          "typeParams": [],
          "params": [],
          "return": { "kind": "void" },
          "thrown": [],
          "varargs": false,
          "default": false,
          "receiver": null,
          "annotationDefault": null,
          "annotations": [],
          "deprecated": false,
          "origin": "EXPLICIT",
          "doc": null,
          "docKind": null,
          "position": null
        }
      ],
      "nested": []
    },
    {
      "qualifiedName": "com.example.shapes.Outer",
      "simpleName": "Outer",
      "kind": "CLASS",
      "package": "com.example.shapes",
      "module": null,
      "enclosing": null,
      "nesting": "TOP_LEVEL",
      "modifiers": ["public"],
      "typeParams": [],
      "superclass": null,
      "interfaces": [],
      "permits": [],
      "recordComponents": [],
      "annotations": [],
      "deprecated": false,
      "doc": null,
      "docKind": null,
      "position": null,
      "fields": [],
      "enumConstants": [],
      "constructors": [],
      "methods": [],
      "nested": ["com.example.shapes.Outer.Inner"]
    },
    {
      "qualifiedName": "com.example.shapes.Outer.Inner",
      "simpleName": "Inner",
      "kind": "CLASS",
      "package": "com.example.shapes",
      "module": null,
      "enclosing": "com.example.shapes.Outer",
      "nesting": "MEMBER",
      "modifiers": ["public", "static"],
      "typeParams": [],
      "superclass": null,
      "interfaces": [],
      "permits": [],
      "recordComponents": [],
      "annotations": [],
      "deprecated": false,
      "doc": null,
      "docKind": null,
      "position": null,
      "fields": [],
      "enumConstants": [],
      "constructors": [],
      "methods": [],
      "nested": []
    }
  ],
  "references": [
    {
      "owner": "com.example.shapes.Container#useMax()",
      "target": "com.example.shapes.MathUtils#max(T,T)",
      "file": "Container.java",
      "start": 100,
      "end": 120
    }
  ]
}"#;

// ── Renderer (duplicated verbatim across per-language ref_snapshots.rs files;
//    no shared test-support module exists in this crate's test tree) ───────

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

fn render_occurrences(occurrences: &[(nudox_ir::change::IntroId, nudox_ir::vocab::Occurrence)], table: &PristineIntroTable) -> String {
    let name_of = |id: nudox_ir::change::IntroId| -> String {
        table.get(id).map(|e| e.sym().name.clone()).unwrap_or_else(|| format!("<DANGLING {}>", id.to_hex()))
    };
    let mut occs: Vec<_> = occurrences.iter().collect();
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

// ── Test ─────────────────────────────────────────────────────────────────

#[test]
fn java_reference_graph_snapshot() {
    let extraction: Extraction =
        serde_json::from_str(FIXTURE).expect("fabricated Extraction fixture must parse");

    let root = std::path::PathBuf::from("com/example/shapes");
    let pkg_id = PackageId::path(&root);
    let root_sym = Symbol {
        name: "shapes".to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: root,
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };
    let mut low: Lowering<JavaId> = Lowering::new(pkg_id, root_sym);
    let mut ctx = LoweringCtx::new(&mut low, &extraction.types);
    lower_extraction(&mut ctx, &extraction);
    let ir_package: IrPackage<JavaId> = low
        .finish()
        .expect("fixture must lower without structural errors");

    let lineage = PackageLineageId::new(EcosystemId::new("maven"), PackageName::new("shapes"));
    let outcome = ir_package.seal(&lineage, &nudox_ir::foreign::Unlinked);

    // A nested/member type (`Outer.Inner`) must retain a parent link once
    // sealed into a `PristineIntroTable` — same check as
    // `producer_tests.rs`'s `gson_nested_class_keeps_its_parent_through_the_full_pipeline`,
    // just against the fabricated fixture instead of a real oracle run.
    let (inner_id, _) = outcome
        .table
        .iter()
        .find(|(_, e)| e.sym().name == "Inner")
        .expect("Outer.Inner must be lowered");
    let outer_parent_id = outcome
        .table
        .parent_of(inner_id)
        .expect("Inner must have a parent (Outer)");
    let outer_parent = outcome
        .table
        .get(outer_parent_id)
        .expect("Inner's parent must be a live entry");
    assert_eq!(
        outer_parent.sym().name,
        "Outer",
        "Inner's enclosing type must survive sealing as a real parent link"
    );

    let table_render = render_table(&outcome.table);
    let occurrence_render = render_occurrences(&outcome.occurrences, &outcome.table);
    insta::assert_snapshot!(
        "java_reference_graph",
        format!("── TABLE ──\n{table_render}\n── OCCURRENCES ──\n{occurrence_render}")
    );
}
