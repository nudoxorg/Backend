//! NixOS / home-manager module & options extraction — the single
//! highest-value target for real users, and something no doc-search tool
//! unifies with functions.
//!
//! The ideal path (per the plan) evaluates `lib.evalModules { modules = [ the
//! module ]; }` against the materialized nixpkgs `lib` and walks the resulting
//! option tree the way `lib.nixosOptionsDoc` does. That second evaluation is a
//! reconciliation point once snix is vendored; until then this walks an
//! already-evaluated options tree (`_type = "option"` leaves) — the shape
//! `evalModules` produces — and lowers it structurally.
//!
//! Each module becomes an `Entry::RecordType` whose fields are its options;
//! each option lowers to a `Field` (name path, structured type, default /
//! example / description, declaration positions).

use ir::entry::NudoxPath;
use ir::kind::{Entry, Symbol, Visibility};
use ir::record::{
    Field, FieldAttributes, FieldKey, KnownField, Record,
};
use ir::ty::{Type, TypeReference};
use snix_eval::{SourceCode, Value};

use super::item::path_of;
use super::syntax::StaticTable;

/// Extract one `nixosModules`/`homeModules` output into IR entries: a
/// `RecordType` for the module plus its option fields.
pub fn extract_module(
    module: &Value,
    segs: &[String],
    _source: &SourceCode,
    _table: &StaticTable,
) -> Vec<(NudoxPath, Entry)> {
    let path = path_of(segs);
    let name = segs.last().cloned().unwrap_or_default();

    // Look for an already-evaluated `options` attrset (what evalModules yields).
    let options = select(module, "options");
    let mut fields = Vec::new();
    if let Some(options) = &options {
        collect_options(options, &mut Vec::new(), &mut fields);
    }

    let record = Record {
        name:                  Some(name.clone()),
        generics:              None,
        fields,
        call_signatures:       None,
        constructors:          None,
        methods:               None,
        index_signatures:      None,
        super_types:           None,
        members:               None,
        implemented_protocols: None,
    };

    let symbol = Symbol {
        name,
        path: path.clone(),
        aliases: None,
        visibility: Visibility::Public,
        documentation: Some("A NixOS/home-manager module. Fields are its declared options.".to_string()),
        deprecation: None,
        doc_links: None,
        inner: record,
    };

    vec![(path, Entry::RecordType(symbol))]
}

/// Recursively walk an options attrset, flattening nested option sets into
/// dotted field names (`services.nginx.enable`) and lowering each leaf option.
fn collect_options(value: &Value, prefix: &mut Vec<String>, out: &mut Vec<Field>) {
    let Some(attrs) = as_attrs(value) else { return };
    for (key, child) in attrs {
        if is_option(&child) {
            prefix.push(key);
            out.push(lower_option(&child, prefix));
            prefix.pop();
        } else if as_attrs(&child).is_some() {
            prefix.push(key);
            collect_options(&child, prefix, out);
            prefix.pop();
        }
    }
}

/// Lower a single `_type = "option"` leaf into a `Field`.
fn lower_option(option: &Value, name_path: &[String]) -> Field {
    let name = name_path.join(".");

    let ty = select(option, "type")
        .map(|t| lower_option_type(&t))
        .unwrap_or(Type::Any);

    let mut doc = String::new();
    if let Some(Value::String(s)) = select(option, "description") {
        doc.push_str(&nix_string(&s));
        doc.push_str("\n\n");
    }
    if let Some(dt) = select(option, "defaultText").or_else(|| select(option, "default")) {
        if let Some(rendered) = render_scalar(&dt) {
            doc.push_str(&format!("**Default:** `{rendered}`\n\n"));
        }
    }
    if let Some(ex) = select(option, "example") {
        if let Some(rendered) = render_scalar(&ex) {
            doc.push_str(&format!("**Example:** `{rendered}`\n\n"));
        }
    }
    if let Some(decls) = select(option, "declarations") {
        if let Some(files) = as_list_strings(&decls) {
            if !files.is_empty() {
                doc.push_str(&format!("**Declared in:** {}\n\n", files.join(", ")));
            }
        }
    }

    let read_only = matches!(select(option, "readOnly"), Some(Value::Bool(true)));

    Field::Known(KnownField {
        key:           FieldKey::Ident(name),
        r#type:        Some(Box::new(ty)),
        default_value: None,
        attributes:    FieldAttributes {
            decorators:  Vec::new(),
            is_mutable:  !read_only,
            is_optional: true, // options are (almost) always optional
            is_static:   false,
        },
        visibility:    Some(Visibility::Public),
        documentation: (!doc.trim().is_empty()).then(|| doc.trim().to_string()),
    })
}

/// Structurally lower a module-system `type` (an attrset with a `name` like
/// `"bool"`, `"str"`, `"enum"`, `"listOf"`, …) into an IR [`Type`].
fn lower_option_type(ty: &Value) -> Type {
    let name = match select(ty, "name") {
        Some(Value::String(s)) => nix_string(&s),
        _ => return Type::Any,
    };
    match name.as_str() {
        "bool" => Type::Primitive(ir::primitives::Primitive::Bool),
        "int" | "signedInt" | "unsignedInt" | "positiveInt" | "port" => {
            Type::Primitive(ir::primitives::Primitive::Int(ir::primitives::Width::W64))
        }
        "float" => Type::Primitive(ir::primitives::Primitive::Float(ir::primitives::Width::W64)),
        "str" | "string" | "lines" | "singleLineStr" | "passwdEntry" | "commas" => {
            Type::Primitive(ir::primitives::Primitive::String)
        }
        "listOf" => {
            let inner = nested_type(ty);
            Type::Slice(Box::new(inner))
        }
        "attrsOf" | "lazyAttrsOf" => {
            let inner = nested_type(ty);
            Type::TypeReference(TypeReference {
                identifier:   "AttrSet".to_string(),
                generic_args: Some(vec![ir::generics::GenericArg::Type(inner)]),
            })
        }
        "nullOr" => Type::Union(vec![
            nested_type(ty),
            Type::TypeReference(TypeReference { identifier: "Null".to_string(), generic_args: None }),
        ]),
        "enum" => {
            // Enum values live under `functor.payload` or `values`; render each
            // as a `Type::Literal` (string spelling), unioned — not opaque
            // TypeReferences, so the renderer / docs can show the exact values.
            let members = enum_members(ty)
                .into_iter()
                .map(|v| {
                    Type::Literal(ir::ty::LiteralValue {
                        kind:  ir::ty::LiteralKind::String,
                        value: v,
                    })
                })
                .collect::<Vec<_>>();
            if members.is_empty() {
                Type::Any
            } else {
                Type::Union(members)
            }
        }
        // Additional lib.types mappings used heavily by NixOS modules.
        "package" => Type::TypeReference(TypeReference {
            identifier:   "Derivation".to_string(),
            generic_args: None,
        }),
        "path" => Type::TypeReference(TypeReference {
            identifier:   "Path".to_string(),
            generic_args: None,
        }),
        "uniq" | "unique" => nested_type(ty),
        "either" => {
            // `either a b` stashes both under nestedTypes.
            let left = select(ty, "nestedTypes")
                .and_then(|n| select(&n, "left"))
                .map(|t| lower_option_type(&t))
                .unwrap_or(Type::Any);
            let right = select(ty, "nestedTypes")
                .and_then(|n| select(&n, "right"))
                .map(|t| lower_option_type(&t))
                .unwrap_or(Type::Any);
            Type::Union(vec![left, right])
        }
        "oneOf" => {
            // `oneOf [ t1 t2 … ]` — payload list of nested types.
            let members = select(ty, "functor")
                .and_then(|f| select(&f, "payload"))
                .and_then(|p| as_list_values(&p))
                .unwrap_or_default()
                .into_iter()
                .map(|t| lower_option_type(&t))
                .collect::<Vec<_>>();
            if members.is_empty() {
                Type::Any
            } else {
                Type::Union(members)
            }
        }
        "raw" | "unspecified" => Type::Any,
        "submodule" => {
            // A nested module: represent as an (opaque) record reference.
            Type::TypeReference(TypeReference { identifier: "Submodule".to_string(), generic_args: None })
        }
        other => Type::TypeReference(TypeReference { identifier: other.to_string(), generic_args: None }),
    }
}

/// The element type of a parametric module-system type (`listOf T`, `nullOr T`).
/// The module system stashes it under `nestedTypes.elemType`.
fn nested_type(ty: &Value) -> Type {
    select(ty, "nestedTypes")
        .and_then(|n| select(&n, "elemType"))
        .map(|t| lower_option_type(&t))
        .unwrap_or(Type::Any)
}

fn enum_members(ty: &Value) -> Vec<String> {
    // `functor.payload.values` (newer) or a top-level `values` list of strings.
    let via_functor = select(ty, "functor")
        .and_then(|f| select(&f, "payload"))
        .and_then(|p| select(&p, "values"));
    let values = via_functor.or_else(|| select(ty, "values"));
    values.and_then(|v| as_list_strings(&v)).unwrap_or_default()
}

// ───────────────────────────────────────────────────────────────────────────
// snix Value helpers (shared shape with walker; kept local to avoid a cycle)
// ───────────────────────────────────────────────────────────────────────────

fn as_attrs(value: &Value) -> Option<Vec<(String, Value)>> {
    match value {
        Value::Attrs(attrs) => {
            let mut out: Vec<(String, Value)> =
                attrs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
            out.sort_by(|a, b| a.0.cmp(&b.0));
            Some(out)
        }
        _ => None,
    }
}

fn select(value: &Value, key: &str) -> Option<Value> {
    as_attrs(value)?.into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn is_option(value: &Value) -> bool {
    matches!(select(value, "_type"), Some(Value::String(ref s)) if s.to_string() == "option")
}

fn nix_string(s: &snix_eval::NixString) -> String {
    s.to_string()
}

/// Render a scalar value to a short display string (for default/example blocks).
fn render_scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(nix_string(s)),
        Value::Bool(b) => Some(b.to_string()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Float(f) => Some(f.to_string()),
        Value::Null => Some("null".to_string()),
        // Structured defaults are often thunks or attrsets — skip rather than
        // force (never force `default` thunks blindly; see the plan's guard).
        _ => None,
    }
}

fn as_list_strings(value: &Value) -> Option<Vec<String>> {
    match value {
        Value::List(list) => Some(
            list.iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(nix_string(s)),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

fn as_list_values(value: &Value) -> Option<Vec<Value>> {
    match value {
        Value::List(list) => Some(list.iter().cloned().collect()),
        _ => None,
    }
}

