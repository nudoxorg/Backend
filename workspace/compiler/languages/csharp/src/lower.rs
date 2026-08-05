//! Lower a [`schema::Extraction`] into an `nudox-ir` [`Lowering`] in one pass.
//!
//! # Design
//!
//! The oracle emits a **flat** list of type declarations with parent pointers
//! (`doc_id` / `enclosing`). [`Lowering`] accepts declarations in any order
//! with forward references, so we can iterate `extraction.types` once without
//! building any intermediate tree or path map.
//!
//! ## ID choice: Roslyn DocumentationCommentId
//!
//! `Self::Id = String` where the value is the Roslyn doc-id form
//! (`T:Ns.Type`, `M:Ns.Type.Method(…)`).  Rationale:
//!
//! 1. The oracle already emits it as a stable, unique key on every symbol.
//! 2. It is the natural key for `<see cref=…>` resolution — `refer(doc_id)`
//!    makes forward cref links resolve for free without a separate phase.
//! 3. It is cross-referenceable across packages (same format as NuGet XML
//!    docs).
//!
//! ## One pass, no intermediate tree
//!
//! ```text
//! for type_decl in extraction.types {
//!     let parent = decl.enclosing.as_ref() → refer(parent_doc_id)
//!     declare(doc_id, parent, sym, kind)
//!     for member in type_decl.members {
//!         declare(member_doc_id, Some(type_doc_id), sym, kind)
//!     }
//! }
//! ```
//!
//! `refer` is called before `declare` may have run for the parent; that is
//! exactly what the Lowering API is designed for.
//!
//! ## What becomes a separate entry vs. inline data
//!
//! | C# construct          | IR entry kind                         |
//! |-----------------------|---------------------------------------|
//! | class/struct/record   | `Record`                              |
//! | interface             | `Trait`                               |
//! | enum                  | `Enum` + `Variant` per member         |
//! | delegate              | `Alias` (target = `Type::FunctionPointer`) |
//! | field (non-const)     | `Field` (child of Record)             |
//! | field `const`         | `Const` (child of Record/Enum)        |
//! | property/indexer      | `Field` (child of Record/Trait)       |
//! | event                 | `Field` with `Static` attribute       |
//! | method/ctor/op        | `Function` (child of Record/Trait)    |
//! | enum member           | `Variant` (child of Enum)             |

use std::{collections::HashMap, path::PathBuf};

use nudox_ir::{
    build::{
        Alias, Const, Enum, Field, FieldAttribute, FieldKey, FnModifier, Function, Param,
        ParamAttribute, Primitive, Receiver, Record, RecordForm, Trait, Type, Variant, VariantForm,
    },
    entry::{AttrTok, Deprecation, DocLink, Symbol, Visibility},
    index::Ref,
    lower::Lowering,
};

use crate::{
    schema::{self, Extraction, TypeDecl},
    types,
    xmldoc::{self, ParsedDoc, strip_doc_id_prefix},
};

// ---------------------------------------------------------------------------
// The lowering entry point
// ---------------------------------------------------------------------------

/// Lower a full [`Extraction`] into the provided [`Lowering`].
///
/// All type declarations are processed in one pass.  Members of each type are
/// declared as children in the same pass.  The caller calls
/// `lowering.finish()` after this returns.
///
/// A `name_to_doc_id` map (FQN → doc-id) is built from the extraction up
/// front so that named type references inside `TypeSig` trees can be resolved
/// to `Type::Nominal(RawRef)` rather than falling back to `Type::Any`.
pub fn lower_extraction(extraction: &Extraction, out: &mut Lowering<String>) {
    // Build a FQN → doc-id lookup for all types in this extraction.
    // Key: `TypeDecl::qualified_name` (metadata name with arity, e.g.
    //   `"System.Collections.Generic.List\`1"`).
    // Value: `TypeDecl::doc_id` (e.g. `"T:System.Collections.Generic.List\`1"`).
    // Types with an empty doc_id are omitted (they have no stable key).
    let name_to_doc_id: HashMap<String, String> = extraction
        .types
        .iter()
        .filter(|t| !t.doc_id.is_empty())
        .map(|t| (t.qualified_name.clone(), t.doc_id.clone()))
        .collect();

    for decl in &extraction.types {
        lower_type_decl(decl, &name_to_doc_id, out);
    }
}

// ---------------------------------------------------------------------------
// Shared symbol helpers
// ---------------------------------------------------------------------------

/// Build the IR [`Symbol`] for a type-level entry.
fn type_symbol(
    decl: &TypeDecl,
    parsed: Option<&ParsedDoc>,
    extra_doc_sections: &[String],
) -> Symbol {
    let visibility = type_visibility(decl);
    let documentation = build_documentation(parsed, extra_doc_sections, decl.deprecated.as_ref());
    let aliases = aliases_from_doc_id(&decl.doc_id);
    let doc_links = doc_links_from_map(decl.doc_links.as_ref(), parsed);
    let deprecation = deprecation_of(decl.deprecated.as_ref());
    let attrs = render_attrs(&decl.attributes);

    Symbol {
        name: decl.simple_name.clone(),
        visibility,
        documentation,
        source: PathBuf::new(),
        span: 0..0,
        aliases,
        deprecation,
        doc_links,
        attrs,
        cfg: None,
    }
}

/// Build the IR [`Symbol`] for a member-level entry.
fn member_symbol(
    name: &str,
    accessibility: &str,
    parsed: Option<&ParsedDoc>,
    deprecated: Option<&schema::Deprecated>,
    attrs: &[schema::Attr],
    extra_sections: &[String],
) -> Symbol {
    let visibility = types::map_visibility(accessibility);
    let deprecation = deprecation_of(deprecated);
    let rendered_attrs = render_attrs(attrs);
    let documentation = build_documentation(parsed, extra_sections, deprecated);

    Symbol {
        name: name.to_string(),
        visibility,
        documentation,
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation,
        doc_links: Box::new([]),
        attrs: rendered_attrs,
        cfg: None,
    }
}

/// Aliases: the doc_id itself, stored as a single alias for cref lookup.
fn aliases_from_doc_id(doc_id: &str) -> Box<[String]> {
    if doc_id.is_empty() {
        Box::new([])
    } else {
        Box::new([doc_id.to_string()])
    }
}

/// Build [`DocLink`]s from an oracle `cref → doc_id` map and from
/// `<seealso>` references in the parsed doc.
fn doc_links_from_map(
    map: Option<&std::collections::BTreeMap<String, String>>,
    parsed: Option<&ParsedDoc>,
) -> Box<[DocLink]> {
    let mut out: Vec<DocLink> = Vec::new();

    if let Some(m) = map {
        for (cref, doc_id) in m {
            out.push(DocLink {
                target: doc_id.clone(),
                label: Some(cref.clone()),
            });
        }
    }

    // <seealso cref=…> references from XML doc comments.
    if let Some(p) = parsed {
        for sa in &p.see_also {
            out.push(DocLink {
                target: sa.clone(),
                label: None,
            });
        }
    }

    out.into_boxed_slice()
}

/// Render [`schema::Attr`]s as [`AttrTok`]s.
fn render_attrs(attrs: &[schema::Attr]) -> Box<[AttrTok]> {
    attrs
        .iter()
        .map(|a| {
            let rendered = types::render_attribute(a);
            AttrTok {
                token: rendered,
                arg: None,
            }
        })
        .collect()
}

/// Map an `[Obsolete]` marker to an IR [`Deprecation`].
fn deprecation_of(dep: Option<&schema::Deprecated>) -> Option<Deprecation> {
    let dep = dep?;
    let note = match (&dep.message, dep.is_error) {
        (Some(msg), true) if !msg.is_empty() => Some(format!("error: {msg}")),
        (Some(msg), false) if !msg.is_empty() => Some(msg.clone()),
        (_, true) => Some("error".to_string()),
        (_, false) => None,
    };
    Some(Deprecation { note, since: None })
}

/// Assemble a documentation string from parsed doc, extra sections, and
/// deprecation notice.
fn build_documentation(
    parsed: Option<&ParsedDoc>,
    extra: &[String],
    deprecated: Option<&schema::Deprecated>,
) -> String {
    let mut sections: Vec<String> = Vec::new();

    if let Some(p) = parsed {
        if let Some(text) = p.documentation() {
            sections.push(text);
        }
        if !p.type_params.is_empty() {
            let mut lines: Vec<String> = p
                .type_params
                .iter()
                .map(|(n, d)| format!("- `{n}` — {d}"))
                .collect();
            lines.sort();
            sections.push(format!("Type parameters:\n{}", lines.join("\n")));
        }
        if !p.params.is_empty() {
            let mut lines: Vec<String> = p
                .params
                .iter()
                .map(|(n, d)| format!("- `{n}` — {d}"))
                .collect();
            lines.sort();
            sections.push(format!("Parameters:\n{}", lines.join("\n")));
        }
    }

    sections.extend(extra.iter().cloned());

    if let Some(dep) = deprecated {
        let label = if dep.is_error {
            "Deprecated (error)"
        } else {
            "Deprecated"
        };
        let note = match &dep.message {
            Some(msg) if !msg.is_empty() => format!("{label}: {msg}"),
            _ => format!("{label}."),
        };
        sections.push(note);
    }

    sections.join("\n\n")
}

/// The accessibility of a type declaration: the first access-modifier token
/// in `modifiers`, defaulting to `"internal"` (C# top-level / namespace
/// default).
fn type_visibility(decl: &TypeDecl) -> Visibility {
    for m in &decl.modifiers {
        if matches!(
            m.as_str(),
            "public"
                | "protected"
                | "internal"
                | "protectedInternal"
                | "privateProtected"
                | "private"
        ) {
            // Stamp a declared note so collapsed pairs stay recoverable.
            return types::map_visibility(m.as_str());
        }
    }
    Visibility::Internal
}

/// The `C# form` keyword (e.g. `class` / `struct` / `record class`) plus
/// non-access modifiers — stamped into documentation.
fn declared_note(kind: &str, modifiers: &[String]) -> String {
    const INTERESTING: &[&str] = &[
        "static", "sealed", "abstract", "readonly", "ref", "partial", "unsafe", "new",
    ];
    let mut parts: Vec<&str> = modifiers
        .iter()
        .map(String::as_str)
        .filter(|m| INTERESTING.contains(m))
        .collect();
    let form = match kind {
        "STRUCT" => "struct",
        "RECORD" => "record class",
        "RECORD_STRUCT" => "record struct",
        "INTERFACE" => "interface",
        "ENUM" => "enum",
        "DELEGATE" => "delegate",
        _ => "class",
    };
    parts.push(form);
    format!("Declared: `{}`", parts.join(" "))
}

// ---------------------------------------------------------------------------
// Top-level type dispatch
// ---------------------------------------------------------------------------

fn lower_type_decl(
    decl: &TypeDecl,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    match decl.kind.as_str() {
        "INTERFACE" => lower_interface(decl, name_to_doc_id, out),
        "ENUM" => lower_enum(decl, name_to_doc_id, out),
        "DELEGATE" => lower_delegate(decl, name_to_doc_id, out),
        // CLASS / STRUCT / RECORD / RECORD_STRUCT share the Record shape.
        _ => lower_class_like(decl, name_to_doc_id, out),
    }
}

/// Parent ref: `refer` the enclosing type's doc_id if present, else `None`
/// (top-level type under the implicit root module).
fn parent_ref(decl: &TypeDecl, out: &mut Lowering<String>) -> Option<String> {
    decl.enclosing.as_ref().map(|enc| {
        // Ensure the parent slot exists (may be declared in the same pass later).
        let _: Ref<Record> = out.refer(enc.clone());
        enc.clone()
    })
}

// ---------------------------------------------------------------------------
// Class / struct / record / record struct
// ---------------------------------------------------------------------------

fn lower_class_like(
    decl: &TypeDecl,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    let parsed = xmldoc::parse_opt(decl.doc.as_deref());
    let parent = parent_ref(decl, out);

    // Extra documentation sections.
    let mut extra: Vec<String> = Vec::new();
    extra.push(declared_note(&decl.kind, &decl.modifiers));
    if decl.forwarded {
        extra.push("Forwarded type (re-exported from a dependency).".to_string());
    }
    if decl.hidden {
        extra.push("Hidden (`EditorBrowsable(Never)`).".to_string());
    }
    if let Some(receiver) = &decl.extension_receiver {
        extra.push(format!(
            "Extension block for `{}`.",
            types::type_display(receiver)
        ));
    }
    if !decl.interfaces.is_empty() {
        let ifaces: Vec<String> = decl.interfaces.iter().map(types::type_display).collect();
        extra.push(format!("Implements: {}", ifaces.join(", ")));
    }

    let sym = type_symbol(decl, parsed.as_ref(), &extra);

    // Lower generic params and where-preds.
    let (generics, wheres) = types::lower_type_params(&decl.type_params, name_to_doc_id, out);

    // Super-types (base class + interfaces), excluding implicit ones.
    const IMPLICIT_BASES: &[&str] = &[
        "System.Object",
        "System.ValueType",
        "System.Enum",
        "System.Delegate",
        "System.MulticastDelegate",
    ];
    // Collect super-types into a Box<[Type]> (what Record::super_types expects).
    let super_types_list: Box<[Type]> = {
        let mut list = Vec::new();
        if let Some(base) = &decl.base_type {
            let implicit = base
                .named_name()
                .is_some_and(|n| IMPLICIT_BASES.contains(&n));
            if !implicit {
                list.push(types::lower_type(base, name_to_doc_id, out));
            }
        }
        for iface in &decl.interfaces {
            list.push(types::lower_type(iface, name_to_doc_id, out));
        }
        list.into_boxed_slice()
    };

    // Determine record form: structs are Struct; others Struct (no Tuple/Unit
    // needed).
    let form = match decl.kind.as_str() {
        "STRUCT" | "RECORD_STRUCT" => RecordForm::Struct,
        _ => RecordForm::Struct,
    };

    // --- Declare the Record entry itself first (children refer to it). ---
    let type_doc_id = decl.doc_id.clone();

    // We build the field and method refs before calling declare to get the refs.
    // However, Lowering::declare takes the kind by value. Since fields/methods are
    // children (separate entries), we declare them after declaring the parent,
    // using Lowering::refer to get refs we embed in Record::fields.

    // Step 1: collect field refs (forward-refers; fields will be declared after).
    let mut field_refs: Vec<Ref<Field>> = Vec::new();
    for f in &decl.members.fields {
        if !f.is_const {
            let field_id = member_id(&type_doc_id, "F", &f.name);
            let r: Ref<Field> = out.refer(field_id);
            field_refs.push(r);
        }
    }
    for p in &decl.members.properties {
        let field_id = member_id(&type_doc_id, "P", &p.name);
        let r: Ref<Field> = out.refer(field_id);
        field_refs.push(r);
    }
    for idx in &decl.members.indexers {
        let field_id = member_id(&type_doc_id, "IDX", &idx.name);
        let r: Ref<Field> = out.refer(field_id);
        field_refs.push(r);
    }
    for e in &decl.members.events {
        let field_id = member_id(&type_doc_id, "E", &e.name);
        let r: Ref<Field> = out.refer(field_id);
        field_refs.push(r);
    }

    let record = Record::builder()
        .form(form)
        .fields(field_refs)
        .super_types(super_types_list)
        .generics(generics)
        .wheres(wheres)
        .build();

    out.declare(type_doc_id.clone(), parent.clone(), sym, record);

    // Step 2: declare const fields as Const entries.
    for f in &decl.members.fields {
        if f.is_const {
            lower_const_field(f, &type_doc_id, name_to_doc_id, out);
        } else {
            lower_field(f, &type_doc_id, name_to_doc_id, out);
        }
    }
    for p in &decl.members.properties {
        lower_property(p, &type_doc_id, name_to_doc_id, out);
    }
    for idx in &decl.members.indexers {
        lower_indexer(idx, &type_doc_id, name_to_doc_id, out);
    }
    for e in &decl.members.events {
        lower_event(e, &type_doc_id, name_to_doc_id, out);
    }

    // Step 3: declare constructors and methods as Function entries.
    for m in &decl.members.constructors {
        lower_method(m, &type_doc_id, false, name_to_doc_id, out);
    }
    for m in &decl.members.methods {
        lower_method(m, &type_doc_id, false, name_to_doc_id, out);
    }
    for m in &decl.members.operators {
        lower_method(m, &type_doc_id, false, name_to_doc_id, out);
    }
    for m in &decl.members.conversions {
        lower_method(m, &type_doc_id, false, name_to_doc_id, out);
    }
}

// ---------------------------------------------------------------------------
// Interface
// ---------------------------------------------------------------------------

fn lower_interface(
    decl: &TypeDecl,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    let parsed = xmldoc::parse_opt(decl.doc.as_deref());
    let parent = parent_ref(decl, out);

    let mut extra: Vec<String> = Vec::new();
    extra.push(declared_note(&decl.kind, &decl.modifiers));
    if decl.hidden {
        extra.push("Hidden (`EditorBrowsable(Never)`).".to_string());
    }
    if !decl.interfaces.is_empty() {
        let bases: Vec<String> = decl.interfaces.iter().map(types::type_display).collect();
        extra.push(format!("Extends: {}", bases.join(", ")));
    }

    let sym = type_symbol(decl, parsed.as_ref(), &extra);
    let (generics, wheres) = types::lower_type_params(&decl.type_params, name_to_doc_id, out);

    let supers: Box<[Type]> = decl
        .interfaces
        .iter()
        .map(|iface| types::lower_type(iface, name_to_doc_id, out))
        .collect::<Vec<_>>()
        .into_boxed_slice();

    let type_doc_id = decl.doc_id.clone();

    let trait_kind = Trait::builder()
        .supers(supers)
        .generics(generics)
        .wheres(wheres)
        .build();

    out.declare(type_doc_id.clone(), parent.clone(), sym, trait_kind);

    // Properties as Field children.
    for p in &decl.members.properties {
        lower_property(p, &type_doc_id, name_to_doc_id, out);
    }
    for idx in &decl.members.indexers {
        lower_indexer(idx, &type_doc_id, name_to_doc_id, out);
    }
    for e in &decl.members.events {
        lower_event(e, &type_doc_id, name_to_doc_id, out);
    }

    // Methods (abstract = required, body-bearing = provided via is_defaulted).
    for m in &decl.members.methods {
        lower_method(m, &type_doc_id, true, name_to_doc_id, out);
    }
    for m in &decl.members.operators {
        lower_method(m, &type_doc_id, true, name_to_doc_id, out);
    }
}

// ---------------------------------------------------------------------------
// Enum
// ---------------------------------------------------------------------------

fn lower_enum(
    decl: &TypeDecl,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    let parsed = xmldoc::parse_opt(decl.doc.as_deref());
    let parent = parent_ref(decl, out);

    let mut extra: Vec<String> = Vec::new();
    extra.push(declared_note(&decl.kind, &decl.modifiers));
    if let Some(underlying) = &decl.enum_underlying {
        let name = types::type_display(underlying);
        if name != "System.Int32" && name != "Int32" {
            extra.push(format!("Underlying type: `{name}`."));
        }
    }
    if decl
        .attributes
        .iter()
        .any(|a| a.ty.ends_with("FlagsAttribute"))
    {
        extra.push("`[Flags]` — a bit-field enumeration.".to_string());
    }
    if decl.hidden {
        extra.push("Hidden (`EditorBrowsable(Never)`).".to_string());
    }

    let sym = type_symbol(decl, parsed.as_ref(), &extra);
    let (generics, wheres) = types::lower_type_params(&decl.type_params, name_to_doc_id, out);

    let type_doc_id = decl.doc_id.clone();

    // Forward-refer all variants.
    let variant_refs: Vec<Ref<Variant>> = decl
        .members
        .fields
        .iter()
        .filter(|f| f.is_const || f.constant.is_some())
        .map(|f| {
            let vid = member_id(&type_doc_id, "V", &f.name);
            out.refer(vid)
        })
        .collect();

    let enum_kind = Enum::builder()
        .variants(variant_refs)
        .generics(generics)
        .wheres(wheres)
        .build();

    out.declare(type_doc_id.clone(), parent.clone(), sym, enum_kind);

    // Declare each variant.
    for f in decl
        .members
        .fields
        .iter()
        .filter(|f| f.is_const || f.constant.is_some())
    {
        lower_enum_variant(f, &type_doc_id, out);
    }

    // Enum methods (rare but possible, e.g. extension methods).
    for m in &decl.members.methods {
        lower_method(m, &type_doc_id, false, name_to_doc_id, out);
    }
}

// ---------------------------------------------------------------------------
// Delegate
// ---------------------------------------------------------------------------

fn lower_delegate(
    decl: &TypeDecl,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    let parsed = xmldoc::parse_opt(decl.doc.as_deref());
    let parent = parent_ref(decl, out);

    let mut extra: Vec<String> = Vec::new();
    extra.push("A `delegate` type.".to_string());
    extra.push(declared_note(&decl.kind, &decl.modifiers));
    if decl.hidden {
        extra.push("Hidden (`EditorBrowsable(Never)`).".to_string());
    }

    // Document the delegate's invoke signature in the doc string (kept for
    // rendering, complementary to the structural target below).
    // Also build a `Type::FunctionPointer` target for the `Alias` entry.
    let delegate_target: Option<Type> = if let Some(sig) = &decl.delegate_sig {
        let params_text: Vec<String> = sig
            .params
            .iter()
            .map(|p| format!("{} {}", types::type_display(&p.ty), p.name))
            .collect();
        let ret_text = sig
            .return_type
            .as_ref()
            .map(types::type_display)
            .unwrap_or_else(|| "void".to_string());
        extra.push(format!(
            "Invoke signature: `({}) → {}`",
            params_text.join(", "),
            ret_text
        ));

        // Structural FunctionPointer target: delegates are managed callable
        // types, so `abi = None` (no unmanaged calling convention).
        let ir_params: Box<[Type]> = sig
            .params
            .iter()
            .map(|p| types::lower_type(&p.ty, name_to_doc_id, out))
            .collect();

        let ir_ret = sig.return_type.as_ref().and_then(|r| {
            let t = types::lower_type(r, name_to_doc_id, out);
            // `void` (empty Tuple) return → None (no return type).
            match &t {
                Type::Tuple(elems) if elems.is_empty() => None,
                _ => Some(Box::new(t)),
            }
        });

        Some(Type::FunctionPointer {
            params: ir_params,
            ret: ir_ret,
            abi: None, // managed delegate; no unmanaged calling convention.
        })
    } else {
        None
    };

    let sym = type_symbol(decl, parsed.as_ref(), &extra);
    let (generics, wheres) = types::lower_type_params(&decl.type_params, name_to_doc_id, out);

    let alias_kind = Alias::builder()
        .maybe_target(delegate_target)
        .generics(generics)
        .wheres(wheres)
        .build();

    let type_doc_id = decl.doc_id.clone();
    out.declare(type_doc_id, parent, sym, alias_kind);
}

// ---------------------------------------------------------------------------
// Field members
// ---------------------------------------------------------------------------

fn lower_field(
    f: &schema::Field,
    parent_id: &str,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    let field_id = member_id(parent_id, "F", &f.name);
    let parsed = xmldoc::parse_opt(f.doc.as_deref());

    let mut extra: Vec<String> = Vec::new();
    if !f.accessibility.is_empty() {
        extra.push(format!("Declared: `{}`", f.accessibility));
    }
    if f.is_readonly {
        extra.push("Declared: `readonly`".to_string());
    }
    if f.is_volatile {
        extra.push("Declared: `volatile`".to_string());
    }
    if f.is_required {
        extra.push("Declared: `required`".to_string());
    }
    if f.hidden {
        extra.push("Hidden (`EditorBrowsable(Never)`).".to_string());
    }

    let sym = member_symbol(
        &f.name,
        &f.accessibility,
        parsed.as_ref(),
        f.deprecated.as_ref(),
        &f.attributes,
        &extra,
    );

    let mut attrs: Vec<FieldAttribute> = Vec::new();
    if !(f.is_const || f.is_readonly) {
        attrs.push(FieldAttribute::Mutable);
    }
    if f.is_static || f.is_const {
        attrs.push(FieldAttribute::Static);
    }

    let field_kind = Field::builder()
        .key(FieldKey::Named)
        .ty(types::lower_type(&f.ty, name_to_doc_id, out))
        .attributes(attrs)
        .build();

    out.declare(field_id, Some(parent_id.to_string()), sym, field_kind);
}

fn lower_const_field(
    f: &schema::Field,
    parent_id: &str,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    let const_id = member_id(parent_id, "CF", &f.name);
    let parsed = xmldoc::parse_opt(f.doc.as_deref());

    let mut extra: Vec<String> = Vec::new();
    if !f.accessibility.is_empty() {
        extra.push(format!("Declared: `{}`", f.accessibility));
    }
    if f.hidden {
        extra.push("Hidden (`EditorBrowsable(Never)`).".to_string());
    }

    let sym = member_symbol(
        &f.name,
        &f.accessibility,
        parsed.as_ref(),
        f.deprecated.as_ref(),
        &f.attributes,
        &extra,
    );

    // The constant value is stored as a rendered display string (the oracle's
    // `constant` field).  A structured const-expression representation is a
    // Track B item: it would require a `ConstExpr` AST in the IR.  The
    // rendered text is sufficient for display and basic search.
    let const_kind = Const::builder()
        .ty(types::lower_type(&f.ty, name_to_doc_id, out))
        .maybe_value(f.constant.clone())
        .build();

    out.declare(const_id, Some(parent_id.to_string()), sym, const_kind);
}

fn lower_property(
    p: &schema::Property,
    parent_id: &str,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    let field_id = member_id(parent_id, "P", &p.name);
    let parsed = xmldoc::parse_opt(p.doc.as_deref());

    let mut extra: Vec<String> = Vec::new();
    extra.push(accessor_note(p));
    if !p.accessibility.is_empty() {
        extra.push(format!("Declared: `{}`", p.accessibility));
    }
    if p.is_required {
        extra.push("Declared: `required`".to_string());
    }
    if p.hidden {
        extra.push("Hidden (`EditorBrowsable(Never)`).".to_string());
    }
    if let Some(val) = parsed.as_ref().and_then(|d| d.value.clone()) {
        extra.push(format!("Value: {val}"));
    }

    let sym = member_symbol(
        &p.name,
        &p.accessibility,
        parsed.as_ref(),
        p.deprecated.as_ref(),
        &p.attributes,
        &extra,
    );

    let mut attrs: Vec<FieldAttribute> = Vec::new();
    // Mutable if there is any setter/init.
    if p.set_kind != "none" && !p.set_kind.is_empty() {
        attrs.push(FieldAttribute::Mutable);
    }
    if p.is_static {
        attrs.push(FieldAttribute::Static);
    }

    let field_kind = Field::builder()
        .key(FieldKey::Named)
        .ty(types::lower_type(&p.ty, name_to_doc_id, out))
        .attributes(attrs)
        .build();

    out.declare(field_id, Some(parent_id.to_string()), sym, field_kind);
}

fn lower_indexer(
    p: &schema::Property,
    parent_id: &str,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    // Indexers share the same member_id space as properties, but get a
    // distinct prefix to avoid collisions with a property of the same name.
    let field_id = member_id(parent_id, "IDX", &p.name);
    let parsed = xmldoc::parse_opt(p.doc.as_deref());

    let params_note: Vec<String> = p
        .parameters
        .iter()
        .map(|param| format!("{} {}", types::type_display(&param.ty), param.name))
        .collect();

    let mut extra: Vec<String> = Vec::new();
    extra.push(format!("Indexer[{}]", params_note.join(", ")));
    if !p.accessibility.is_empty() {
        extra.push(format!("Declared: `{}`", p.accessibility));
    }
    if p.hidden {
        extra.push("Hidden (`EditorBrowsable(Never)`).".to_string());
    }

    let sym = member_symbol(
        &p.name,
        &p.accessibility,
        parsed.as_ref(),
        p.deprecated.as_ref(),
        &p.attributes,
        &extra,
    );

    let mut attrs: Vec<FieldAttribute> = Vec::new();
    if p.set_kind != "none" && !p.set_kind.is_empty() {
        attrs.push(FieldAttribute::Mutable);
    }
    if p.is_static {
        attrs.push(FieldAttribute::Static);
    }

    let field_kind = Field::builder()
        .key(FieldKey::Named)
        .ty(types::lower_type(&p.ty, name_to_doc_id, out))
        .attributes(attrs)
        .build();

    out.declare(field_id, Some(parent_id.to_string()), sym, field_kind);
}

fn lower_event(
    e: &schema::Event,
    parent_id: &str,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    let field_id = member_id(parent_id, "E", &e.name);
    let parsed = xmldoc::parse_opt(e.doc.as_deref());

    let mut extra: Vec<String> = Vec::new();
    extra.push("Declared: `event`".to_string());
    if e.is_static {
        extra.push("Declared: `static`".to_string());
    }
    if !e.accessibility.is_empty() {
        extra.push(format!("Declared: `{}`", e.accessibility));
    }
    // Surface asymmetric accessor accessibilities (add/remove can differ from
    // the event's own declared accessibility).
    if let Some(add_vis) = &e.add_accessibility
        && add_vis != &e.accessibility
    {
        extra.push(format!("Add accessor: `{add_vis}`"));
    }
    if let Some(rem_vis) = &e.remove_accessibility
        && rem_vis != &e.accessibility
    {
        extra.push(format!("Remove accessor: `{rem_vis}`"));
    }
    if e.hidden {
        extra.push("Hidden (`EditorBrowsable(Never)`).".to_string());
    }

    let sym = member_symbol(
        &e.name,
        &e.accessibility,
        parsed.as_ref(),
        e.deprecated.as_ref(),
        &e.attributes,
        &extra,
    );

    let mut attrs: Vec<FieldAttribute> = Vec::new();
    attrs.push(FieldAttribute::Mutable); // events are always add/remove mutable
    if e.is_static {
        attrs.push(FieldAttribute::Static);
    }

    let field_kind = Field::builder()
        .key(FieldKey::Named)
        .ty(types::lower_type(&e.ty, name_to_doc_id, out))
        .attributes(attrs)
        .build();

    out.declare(field_id, Some(parent_id.to_string()), sym, field_kind);
}

/// The accessor decorator string for a property.
fn accessor_note(p: &schema::Property) -> String {
    let mut parts: Vec<String> = Vec::new();
    match &p.get_accessibility {
        Some(vis) if vis != &p.accessibility => parts.push(format!("{vis} get")),
        Some(_) | None => parts.push("get".to_string()),
    }
    match p.set_kind.as_str() {
        "set" => match &p.set_accessibility {
            Some(vis) if vis != &p.accessibility => parts.push(format!("{vis} set")),
            _ => parts.push("set".to_string()),
        },
        "init" => match &p.set_accessibility {
            Some(vis) if vis != &p.accessibility => parts.push(format!("{vis} init")),
            _ => parts.push("init".to_string()),
        },
        _ => {}
    }
    format!("Accessor: {}", parts.join("; "))
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

/// Lower a method, constructor, operator, or conversion to a Function entry.
/// `in_interface` is true when the parent is an interface (so `is_abstract`
/// drives `is_defaulted` inversion).
fn lower_method(
    m: &schema::Method,
    parent_id: &str,
    in_interface: bool,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    let method_id = method_doc_id(m, parent_id);
    let parsed = xmldoc::parse_opt(m.doc.as_deref());

    // --- Parameters (inputs). ---
    // Forward-refer each parameter before declaring it, so the Function kind
    // body can embed the ref. The declare call in lower_param fills the slot.
    let mut input_refs: Vec<Ref<Param>> = Vec::new();
    for (i, _) in m.parameters.iter().enumerate() {
        let param_id = format!("{method_id}#p{i}");
        let r: Ref<Param> = out.refer(param_id);
        input_refs.push(r);
    }
    for (i, p) in m.parameters.iter().enumerate() {
        lower_param(p, i, m.is_extension_method, &method_id, name_to_doc_id, out);
    }

    // --- Output parameters (return value). ---
    let mut output_refs: Vec<Ref<Param>> = Vec::new();
    if let Some(ret) = &m.return_type
        && !is_void(ret)
    {
        let ret_id = format!("{method_id}#ret");
        // Forward-refer first so the ref is stable.
        let r: Ref<Param> = out.refer(ret_id.clone());
        output_refs.push(r);
        let parsed_returns = parsed.as_ref().and_then(|p| p.returns.clone());
        lower_return_param(ret, m, &parsed_returns, &method_id, name_to_doc_id, out);
    }

    // Exception documentation from `<exception cref>` XML tags.
    //
    // These do NOT go into output_params.  Adding them there was the Java
    // producer's defect: a consumer reading `output_params` would think the
    // method *returns* its exceptions, which is wrong — C# exceptions are
    // untyped at the call site.
    //
    // Exceptions are wired to THREE places:
    //   1. `Function::throws` — structured `Type::Nominal` for each thrown
    //      exception type (the cref is used directly as the lowering key, since
    //      the oracle emits Roslyn doc-ids as the cref value and those are the
    //      same keys we use for `Lowering::refer`).  We keep the `doc_links`
    //      alongside because `throws` carries only the type, not the prose.
    //   2. `doc_links` with `label = "throws"` (cref, for link resolution).
    //   3. Prose in `documentation` (the description text, for display).
    let mut throws_types: Vec<Type> = Vec::new();
    let mut exception_doc_links: Vec<DocLink> = Vec::new();
    let mut exception_notes: Vec<String> = Vec::new();
    if let Some(p) = &parsed {
        for (cref, text) in &p.exceptions {
            // Wire the exception type into Function::throws.
            //
            // Strategy: the cref is a Roslyn doc-id of the form `T:FQN`.
            // Strip the `T:` prefix to get the FQN, look it up in
            // `name_to_doc_id` (which maps FQN → doc-id).  If found, the
            // type is in the same extraction and we can produce a live
            // Type::Nominal(Ref).  If not found (cross-package type, e.g.
            // System.ArgumentException in a foreign assembly), fall back to
            // Type::Any — consistent with how lower_type handles cross-package
            // named types.
            let fqn = cref.strip_prefix("T:").unwrap_or(cref.as_str());
            let throws_ty = if let Some(doc_id) = name_to_doc_id.get(fqn) {
                out.nominal::<Record>(doc_id.clone())
            } else {
                Type::Any
            };
            throws_types.push(throws_ty);

            exception_doc_links.push(DocLink {
                target: cref.clone(),
                label: Some("throws".to_string()),
            });
            // For prose use the stripped display name (simple name without prefix).
            let display = strip_doc_id_prefix(cref);
            if text.is_empty() {
                exception_notes.push(format!("Throws `{display}`."));
            } else {
                exception_notes.push(format!("Throws `{display}`: {text}"));
            }
        }
    }

    let receiver = if m.is_static {
        None
    } else {
        Some(Receiver::SharedRef)
    };

    let mut modifiers: Vec<FnModifier> = Vec::new();
    if m.is_async {
        modifiers.push(FnModifier::Async);
    }
    if m.is_iterator {
        modifiers.push(FnModifier::Generator);
    }
    if signature_is_unsafe(m) {
        modifiers.push(FnModifier::Unsafe);
    }

    // For interface methods: body-bearing = is_defaulted.
    let is_defaulted = in_interface && !m.is_abstract;

    let (generics, wheres) = types::lower_type_params(&m.type_params, name_to_doc_id, out);

    let fn_kind = Function::builder()
        .maybe_receiver(receiver)
        .input_params(input_refs)
        .output_params(output_refs)
        .modifiers(modifiers)
        .generics(generics)
        .wheres(wheres)
        .is_defaulted(is_defaulted)
        .throws(throws_types)
        .build();

    // Documentation: method modifiers + exception notes.
    let mut extra: Vec<String> = Vec::new();
    extra.extend(method_declaration_notes(m));
    extra.extend(exception_notes);
    if m.hidden {
        extra.push("Hidden (`EditorBrowsable(Never)`).".to_string());
    }

    // Build doc_links: merge oracle-provided links with exception crefs.
    let mut method_doc_links: Vec<DocLink> = Vec::new();
    if let Some(dl_map) = &m.doc_links {
        for (cref, doc_id) in dl_map {
            method_doc_links.push(DocLink {
                target: doc_id.clone(),
                label: Some(cref.clone()),
            });
        }
    }
    method_doc_links.extend(exception_doc_links);

    let visibility = types::map_visibility(&m.accessibility);
    let deprecation = deprecation_of(m.deprecated.as_ref());
    let rendered_attrs = render_attrs(&m.attributes);
    let documentation = build_documentation(parsed.as_ref(), &extra, m.deprecated.as_ref());

    let sym = Symbol {
        name: method_display_name(m),
        visibility,
        documentation,
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation,
        doc_links: method_doc_links.into_boxed_slice(),
        attrs: rendered_attrs,
        cfg: None,
    };

    out.declare(method_id, Some(parent_id.to_string()), sym, fn_kind);
}

fn lower_param(
    p: &schema::Param,
    idx: usize,
    is_extension: bool,
    parent_method_id: &str,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    let param_id = format!("{parent_method_id}#p{idx}");

    let mut attrs: Vec<ParamAttribute> = Vec::new();
    match p.ref_kind.as_str() {
        "ref" => attrs.push(ParamAttribute::Inout),
        "out" => attrs.push(ParamAttribute::Inout),
        "in" | "refReadonly" => attrs.push(ParamAttribute::Borrowing),
        _ => {}
    }
    if p.is_params {
        attrs.push(ParamAttribute::Variadic);
    }
    if p.has_default {
        attrs.push(ParamAttribute::Optional);
    }

    let mut doc_parts: Vec<String> = Vec::new();
    if is_extension && idx == 0 {
        doc_parts.push("(extension receiver)".to_string());
    }
    if p.ref_kind == "out" {
        doc_parts.push("out parameter".to_string());
    }
    if p.ref_kind == "refReadonly" {
        doc_parts.push("ref readonly parameter".to_string());
    }
    if p.scoped {
        doc_parts.push("scoped".to_string());
    }
    // Capture the default-value text when the oracle provides it.
    // The IR has no structured `ConstExpr` slot for default values yet.
    // KNOWN IR GAP: `Param` needs a `default: Option<String>` field (or a
    // structured `default: Option<ConstExpr>` once the const-expression
    // subsystem exists) to carry default-value text without encoding it in
    // the doc string.  For now we append it as a documentation note; the
    // `ParamAttribute::Optional` flag already signals that a default exists.
    if let Some(default_text) = &p.default {
        doc_parts.push(format!("default: `{default_text}`"));
    }

    let sym = Symbol {
        name: p.name.clone(),
        visibility: Visibility::Public,
        documentation: doc_parts.join("; "),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: render_attrs(&p.attributes),
        cfg: None,
    };

    let param_kind = Param::builder()
        .ty(types::lower_type(&p.ty, name_to_doc_id, out))
        .attributes(attrs)
        .build();

    out.declare(
        param_id,
        Some(parent_method_id.to_string()),
        sym,
        param_kind,
    );
}

fn lower_return_param(
    ret: &schema::TypeSig,
    m: &schema::Method,
    returns_doc: &Option<String>,
    parent_method_id: &str,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) {
    let ret_id = format!("{parent_method_id}#ret");

    let ty = if m.returns_by_ref {
        // ref return → Reference type.
        let inner = types::lower_type(ret, name_to_doc_id, out);
        Type::Primitive(Primitive::Reference {
            lifetime: None,
            mutable: !m.returns_by_ref_readonly,
            ty: Box::new(inner),
        })
    } else {
        types::lower_type(ret, name_to_doc_id, out)
    };

    let sym = Symbol {
        name: String::new(),
        visibility: Visibility::Public,
        documentation: returns_doc.clone().unwrap_or_default(),
        source: PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    };

    let param_kind = Param::builder().ty(ty).build();
    out.declare(ret_id, Some(parent_method_id.to_string()), sym, param_kind);
}

// ---------------------------------------------------------------------------
// Enum variants
// ---------------------------------------------------------------------------

fn lower_enum_variant(f: &schema::Field, parent_id: &str, out: &mut Lowering<String>) {
    let vid = member_id(parent_id, "V", &f.name);
    let parsed = xmldoc::parse_opt(f.doc.as_deref());

    let mut doc_parts: Vec<String> = Vec::new();
    if let Some(text) = parsed.as_ref().and_then(ParsedDoc::documentation) {
        doc_parts.push(text);
    }
    if let Some(val) = &f.constant {
        doc_parts.push(format!("Value: `{val}`"));
    }

    let sym = Symbol {
        name: f.name.clone(),
        visibility: Visibility::Public,
        documentation: doc_parts.join("\n\n"),
        source: PathBuf::new(),
        span: 0..0,
        aliases: if f.doc_id.is_empty() {
            Box::new([])
        } else {
            Box::new([f.doc_id.clone()])
        },
        deprecation: deprecation_of(f.deprecated.as_ref()),
        doc_links: Box::new([]),
        attrs: render_attrs(&f.attributes),
        cfg: None,
    };

    let variant_kind = Variant::builder()
        .form(VariantForm::Unit)
        .maybe_discr(f.constant.clone())
        .build();

    out.declare(vid, Some(parent_id.to_string()), sym, variant_kind);
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Synthetic doc-id for a member (when the oracle does not emit one).
///
/// Format: `{type_doc_id}#{kind_prefix}:{name}`
/// This is not a Roslyn doc-id — it is only used as a Lowering key.
fn member_id(type_doc_id: &str, kind: &str, name: &str) -> String {
    format!("{type_doc_id}#{kind}:{name}")
}

/// Use the oracle's `doc_id` for the method if non-empty; otherwise fall back
/// to a synthetic key.  The oracle doc_id for methods is already unique per
/// overload (it includes parameter types).
fn method_doc_id(m: &schema::Method, parent_id: &str) -> String {
    if !m.doc_id.is_empty() {
        m.doc_id.clone()
    } else {
        member_id(parent_id, "M", &m.name)
    }
}

/// The display name for a method: explicit-interface form if applicable.
fn method_display_name(m: &schema::Method) -> String {
    if let Some(iface) = &m.explicit_interface {
        return format!("{}.{}", types::simple_name(iface), m.name);
    }
    m.name.clone()
}

/// Whether a return type is `void` (the unit type).
fn is_void(t: &schema::TypeSig) -> bool {
    matches!(t, schema::TypeSig::Named { name, .. } if name == "System.Void")
}

/// Whether any pointer / function-pointer occurs in the method signature.
fn signature_is_unsafe(m: &schema::Method) -> bool {
    fn ty_has_pointer(t: &schema::TypeSig) -> bool {
        match t {
            schema::TypeSig::Pointer { .. } | schema::TypeSig::FuncPtr { .. } => true,
            schema::TypeSig::Named { args, .. } => args.iter().any(ty_has_pointer),
            schema::TypeSig::Array { element, .. } => ty_has_pointer(element),
            schema::TypeSig::NullableValue { inner } => ty_has_pointer(inner),
            schema::TypeSig::Tuple { elements, .. } => {
                elements.iter().any(|e| ty_has_pointer(&e.ty))
            }
            _ => false,
        }
    }
    m.parameters.iter().any(|p| ty_has_pointer(&p.ty))
        || m.return_type.as_ref().is_some_and(ty_has_pointer)
}

/// Declaration notes for a method: non-access modifiers, explicit interface,
/// attributes.
fn method_declaration_notes(m: &schema::Method) -> Vec<String> {
    let mut notes = Vec::new();
    let mut mods: Vec<&str> = Vec::new();
    if m.is_static {
        mods.push("static");
    }
    if m.is_abstract {
        mods.push("abstract");
    }
    if m.is_virtual {
        mods.push("virtual");
    }
    if m.is_override {
        mods.push("override");
    }
    if m.is_sealed {
        mods.push("sealed");
    }
    if m.is_extern {
        mods.push("extern");
    }
    if m.is_readonly {
        mods.push("readonly");
    }
    if m.is_extension_method {
        mods.push("extension");
    }
    if m.returns_by_ref {
        mods.push(if m.returns_by_ref_readonly {
            "ref readonly return"
        } else {
            "ref return"
        });
    }
    match m.operator_kind.as_str() {
        "implicit" => mods.push("implicit operator"),
        "explicit" => mods.push("explicit operator"),
        "checked" => mods.push("checked operator"),
        _ => {}
    }
    if !mods.is_empty() {
        notes.push(format!("Declared: `{}`", mods.join(" ")));
    }
    if let Some(iface) = &m.explicit_interface {
        notes.push(format!(
            "Explicit implementation of `{iface}` (callable via the interface only)."
        ));
    }
    notes
}
