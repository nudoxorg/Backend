//! Locks the full lowered-IR *reference graph* shape for the C# producer —
//! the sealed `Ref::Intro` / `Ref::Foreign` variants, not just entry counts.
//!
//! `csharp::lower()` is pre-seal (its `Ref`s are `Ref::Local`), so this test
//! seals the lowered package itself (mirroring what `produce()` does
//! internally for every language) to get at the real resolved graph.
//!
//! The fixture below exercises, in one dense `Extraction`:
//!   - a nested/applied generic type (`Box<Box<System.String>>`) — a
//!     `Type::Apply` whose own type argument is itself a `Type::Apply`
//!   - a constructor-body call (`.ctor` -> `Helper`), recorded via the
//!     `references` array as a `FunctionCall` occurrence
//!   - an attribute application (`[MyValidation]` on `Widget`), recorded as
//!     a `TypeReference` occurrence to the attribute class
//!   - a where-clause type-parameter constraint (`Find<T> where T :
//!     IAnimal`) resolving to a same-package `Ref::Intro`
//!   - a `System.*` BCL type reference (`System.String`), resolving as a
//!     named `Ref::Foreign` (dotnet-bcl namespace origin, unlinked)

use nudox_languages::csharp::{lower, parse_extraction};

const FIXTURE: &str = r#"{
  "format": 1,
  "dotnetVersion": "10.0",
  "roslyn": "5.6.0",
  "mode": "source",
  "assembly": { "name": "Combined", "version": "1.0.0", "tfm": "net10.0" },
  "diagnostics": { "errorTypeCount": 0, "errorCount": 0 },
  "namespaces": [
    { "name": "Combined", "doc": null }
  ],
  "types": [
    {
      "docId": "T:Combined.IAnimal",
      "qualifiedName": "Combined.IAnimal",
      "simpleName": "IAnimal",
      "kind": "INTERFACE",
      "namespace": "Combined",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": { "fields": [], "properties": [], "events": [], "constructors": [], "methods": [], "operators": [], "conversions": [], "indexers": [], "nested": [] }
    },
    {
      "docId": "T:Combined.Dog",
      "qualifiedName": "Combined.Dog",
      "simpleName": "Dog",
      "kind": "CLASS",
      "namespace": "Combined",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [
        { "kind": "named", "name": "Combined.IAnimal", "args": [], "owner": null, "nullable": "none", "typeKind": "Interface" }
      ],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": {
        "fields": [
          {
            "name": "_source",
            "docId": "F:Combined.Dog._source",
            "type": { "kind": "named", "name": "System.IO.Stream", "args": [], "owner": null, "nullable": "notAnnotated", "typeKind": "Class" },
            "accessibility": "private",
            "isConst": false,
            "constant": null,
            "isReadonly": true,
            "isVolatile": false,
            "isRequired": false,
            "isStatic": false,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          }
        ],
        "properties": [],
        "events": [],
        "constructors": [],
        "methods": [
          {
            "name": "Find",
            "docId": "M:Combined.Dog.Find``1(``0)",
            "methodKind": "Ordinary",
            "accessibility": "public",
            "isStatic": false,
            "isAbstract": false,
            "isVirtual": false,
            "isOverride": false,
            "isSealed": false,
            "isExtern": false,
            "isAsync": false,
            "isIterator": false,
            "isExtensionMethod": false,
            "isReadonly": false,
            "typeParams": [
              {
                "name": "T",
                "variance": "none",
                "constraints": {
                  "referenceType": true,
                  "valueType": false,
                  "notNull": false,
                  "unmanaged": false,
                  "constructor": false,
                  "allowsRefLike": false,
                  "types": [
                    { "kind": "named", "name": "Combined.IAnimal", "args": [], "owner": null, "nullable": "none", "typeKind": "Interface" }
                  ]
                }
              }
            ],
            "parameters": [
              {
                "name": "criteria",
                "type": { "kind": "typeParam", "name": "T", "ownerKind": "method", "nullable": "none" },
                "refKind": "none",
                "isParams": false,
                "hasDefault": false,
                "default": null,
                "scoped": false,
                "attributes": []
              }
            ],
            "returnType": { "kind": "typeParam", "name": "T", "ownerKind": "method", "nullable": "none" },
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "explicitInterface": null,
            "operatorKind": null,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          }
        ],
        "operators": [],
        "conversions": [],
        "indexers": [],
        "nested": []
      }
    },
    {
      "docId": "T:Combined.Box`1",
      "qualifiedName": "Combined.Box`1",
      "simpleName": "Box",
      "kind": "CLASS",
      "namespace": "Combined",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [
        {
          "name": "T",
          "variance": "none",
          "constraints": {
            "referenceType": false, "valueType": false, "notNull": false,
            "unmanaged": false, "constructor": false, "allowsRefLike": false, "types": []
          }
        }
      ],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": { "fields": [], "properties": [], "events": [], "constructors": [], "methods": [], "operators": [], "conversions": [], "indexers": [], "nested": [] }
    },
    {
      "docId": "T:Combined.Wrapper",
      "qualifiedName": "Combined.Wrapper",
      "simpleName": "Wrapper",
      "kind": "CLASS",
      "namespace": "Combined",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": {
        "fields": [],
        "properties": [
          {
            "name": "Nested",
            "docId": "P:Combined.Wrapper.Nested",
            "type": {
              "kind": "named",
              "name": "Combined.Box`1",
              "args": [
                {
                  "kind": "named",
                  "name": "Combined.Box`1",
                  "args": [
                    { "kind": "named", "name": "System.IO.Stream", "args": [], "owner": null, "nullable": "notAnnotated", "typeKind": "Class" }
                  ],
                  "owner": null,
                  "nullable": "notAnnotated",
                  "typeKind": "Class"
                }
              ],
              "owner": null,
              "nullable": "notAnnotated",
              "typeKind": "Class"
            },
            "accessibility": "public",
            "getAccessibility": null,
            "setAccessibility": null,
            "setKind": "set",
            "isRequired": false,
            "isStatic": false,
            "isIndexer": false,
            "parameters": [],
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          }
        ],
        "events": [],
        "constructors": [],
        "methods": [],
        "operators": [],
        "conversions": [],
        "indexers": [],
        "nested": []
      }
    },
    {
      "docId": "T:Combined.MyValidationAttribute",
      "qualifiedName": "Combined.MyValidationAttribute",
      "simpleName": "MyValidationAttribute",
      "kind": "CLASS",
      "namespace": "Combined",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": { "fields": [], "properties": [], "events": [], "constructors": [], "methods": [], "operators": [], "conversions": [], "indexers": [], "nested": [] }
    },
    {
      "docId": "T:Combined.Widget",
      "qualifiedName": "Combined.Widget",
      "simpleName": "Widget",
      "kind": "CLASS",
      "namespace": "Combined",
      "enclosing": null,
      "modifiers": ["public"],
      "typeParams": [],
      "baseType": null,
      "interfaces": [],
      "enumUnderlying": null,
      "delegateSig": null,
      "attributes": [ { "type": "Combined.MyValidationAttribute", "args": [], "named": {} } ],
      "deprecated": null,
      "hidden": false,
      "forwarded": false,
      "doc": null,
      "docInherited": false,
      "docLinks": null,
      "extensionReceiver": null,
      "members": {
        "fields": [],
        "properties": [],
        "events": [],
        "constructors": [
          {
            "name": ".ctor",
            "docId": "M:Combined.Widget.#ctor",
            "methodKind": "Constructor",
            "accessibility": "public",
            "isStatic": false,
            "isAbstract": false,
            "isVirtual": false,
            "isOverride": false,
            "isSealed": false,
            "isExtern": false,
            "isAsync": false,
            "isIterator": false,
            "isExtensionMethod": false,
            "isReadonly": false,
            "typeParams": [],
            "parameters": [],
            "returnType": null,
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "explicitInterface": null,
            "operatorKind": null,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null,
            "location": { "file": "src/Widget.cs", "start": 0, "end": 30, "startLine": 1, "startColumn": 0, "endLine": 1, "endColumn": 30 }
          }
        ],
        "methods": [
          {
            "name": "Helper",
            "docId": "M:Combined.Widget.Helper",
            "methodKind": "Ordinary",
            "accessibility": "private",
            "isStatic": false,
            "isAbstract": false,
            "isVirtual": false,
            "isOverride": false,
            "isSealed": false,
            "isExtern": false,
            "isAsync": false,
            "isIterator": false,
            "isExtensionMethod": false,
            "isReadonly": false,
            "typeParams": [],
            "parameters": [],
            "returnType": { "kind": "named", "name": "System.Void", "args": [], "owner": null, "nullable": "none", "typeKind": "Void" },
            "returnsByRef": false,
            "returnsByRefReadonly": false,
            "explicitInterface": null,
            "operatorKind": null,
            "attributes": [],
            "deprecated": null,
            "hidden": false,
            "doc": null,
            "docInherited": false,
            "docLinks": null
          }
        ],
        "operators": [],
        "conversions": [],
        "indexers": [],
        "nested": []
      }
    }
  ],
  "references": [
    {
      "owner": "M:Combined.Widget.#ctor",
      "target": "M:Combined.Widget.Helper",
      "file": "src/Widget.cs",
      "start": 10,
      "end": 16
    }
  ]
}"#;

// ---------------------------------------------------------------------------
// Renderer — duplicated verbatim across per-language ref_snapshots.rs files.
// See tests/csharp/ref_snapshots.rs's sibling copies (Rust/Go/TypeScript) for
// the shared design; no test-support module exists in this crate's test
// tree, so duplication here is deliberate.
// ---------------------------------------------------------------------------

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
                // `wheres` (where-clause predicates) is a separate field from
                // `generics` — a C# `where T : IFoo` constraint lands here,
                // not in `GenericParam::Type::bounds`. Rendered here (a small,
                // additive extension to the shared renderer, kept local to
                // this file) so the where-clause bound's resolved `Ref` is
                // actually visible in the locked snapshot.
                for w in f.wheres.iter() {
                    out.push_str(&format!(
                        "  where: {} : {}\n",
                        render_type(&w.target, table),
                        w.bounds.iter().map(|b| render_type(b, table)).collect::<Vec<_>>().join(" + ")
                    ));
                }
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

#[test]
fn csharp_reference_graph() {
    let extraction = parse_extraction(FIXTURE.as_bytes()).expect("fixture must parse");
    let ir_package = lower(&extraction).expect("fixture must lower");

    let lineage = nudox_ir::change::PackageLineageId::new(
        nudox_ir::change::EcosystemId::new("nuget"),
        nudox_ir::change::PackageName::new("combined-corpus"),
    );
    let outcome = ir_package.seal(&lineage, &nudox_ir::foreign::Unlinked);

    let table_render = render_table(&outcome.table);
    let occurrence_render = render_occurrences(&outcome.occurrences, &outcome.table);
    insta::assert_snapshot!(
        "csharp_reference_graph",
        format!("── TABLE ──\n{table_render}\n── OCCURRENCES ──\n{occurrence_render}")
    );
}
