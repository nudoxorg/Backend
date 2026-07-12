//! IR → Nix surface syntax.
//!
//! Nix is a purely functional, lazily-evaluated expression language.  It has
//! no type system of its own, so every "type" we produce is a documentation
//! convention rather than a checked annotation.  We follow RFC-145 for block
//! doc-comments (`/** … */`) and the `::` convention for inline type
//! signatures (Haskell-inspired, widely used in Nixpkgs documentation):
//!
//! ```nix
//! # Type :: Int -> String -> Bool
//! greet = age: name: …
//! ```
//!
//! Records are attrset literals; sum types are documented with a `# one of:`
//! comment because Nix has no native ADTs; traits/interfaces become an
//! attrset contract listing required attribute names.

use ir::function::Function;
use ir::generics::{GenericArg, Generics};
use ir::kind::Visibility;
use ir::parameter::Parameter;
use ir::primitives::Primitive;
use ir::protocols::{TraitDef, TraitMethod};
use ir::record::{Field, FieldKey, Record, SumField, SumVariant};
use ir::ty::{FunctionPointer, Type, TypeReference};

use super::super::backend::*;
use super::super::doc::Doc;
use super::analyze_generics;

/// The Nix backend.  Zero-sized; all state lives in the [`RenderCtx`].
pub struct Nix;

impl Backend for Nix {
    // -----------------------------------------------------------------------
    // doc_comment
    // -----------------------------------------------------------------------

    /// RFC-145 block doc-comment: `/** … */` with CommonMark body.
    fn doc_comment(&self, text: &str) -> Rendered {
        let body = text
            .lines()
            .map(|l| txt("  ").annotate(Annotation::Comment) + txt(l));
        (txt("/**")
            + Doc::hardline()
            + Doc::join(Doc::hardline(), body)
            + Doc::hardline()
            + txt("*/"))
        .annotate(Annotation::Comment)
    }

    // -----------------------------------------------------------------------
    // ty
    // -----------------------------------------------------------------------

    fn ty(&self, t: &Type, cx: &RenderCtx) -> Rendered {
        nix_ty(t, cx)
    }

    // -----------------------------------------------------------------------
    // record
    // -----------------------------------------------------------------------

    /// Render as a commented attrset contract:
    ///
    /// ```nix
    /// Point = {
    ///     # x :: Float
    ///     x,
    ///     # y :: Float
    ///     y,
    /// };
    /// ```
    fn record(&self, name: &str, _vis: &Visibility, rec: &Record, cx: &RenderCtx) -> Rendered {
        let fields: Vec<Rendered> = rec.fields.iter().filter_map(|f| record_field(f, cx)).collect();
        let body = if fields.is_empty() {
            punct("{}")
        } else {
            block("{", fields, "}")
        };
        ident(name) + sp() + punct("=") + sp() + body + punct(";")
    }

    // -----------------------------------------------------------------------
    // sum
    // -----------------------------------------------------------------------

    /// Nix has no native sum type.  Emit a `# one of:` comment contract
    /// followed by an attrset enumerating variant names.
    ///
    /// ```nix
    /// # Shape :: one of: Circle | Rectangle | Triangle
    /// Shape = {
    ///     Circle,
    ///     Rectangle,
    ///     Triangle,
    /// };
    /// ```
    fn sum(
        &self,
        name: &str,
        _generics: Option<&Generics>,
        variants: &[SumVariant],
        _vis: &Visibility,
        cx: &RenderCtx,
    ) -> Rendered {
        let tags = Doc::join(
            txt(" | ").annotate(Annotation::Comment),
            variants
                .iter()
                .map(|v| txt(&v.name).annotate(Annotation::Comment)),
        );
        let comment = txt("# ").annotate(Annotation::Comment)
            + txt(name).annotate(Annotation::Comment)
            + txt(" :: one of: ").annotate(Annotation::Comment)
            + tags;

        let arms: Vec<Rendered> = variants.iter().map(|v| sum_variant(v, cx)).collect();
        let body = block("{", arms, "}");
        comment + Doc::hardline() + ident(name) + sp() + punct("=") + sp() + body + punct(";")
    }

    // -----------------------------------------------------------------------
    // function
    // -----------------------------------------------------------------------

    /// Render a function as a Nix binding.
    ///
    /// When all parameters are simple (no defaults), use curried form:
    /// ```nix
    /// # Type :: A -> B -> Result
    /// name = a: b: <body-placeholder>;
    /// ```
    ///
    /// When any parameter has a default value, use attrset-pattern form:
    /// ```nix
    /// # Type :: { a :: A, b :: B } -> Result
    /// name = { a, b ? <default>, ... }: <body-placeholder>;
    /// ```
    fn function(&self, name: &str, f: &Function, _vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let literal_params: Vec<_> = f
            .input_parameters
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter_map(|p| match p {
                Parameter::Literal(lp) => Some(lp),
                _ => None,
            })
            .collect();

        let has_defaults = literal_params.iter().any(|lp| lp.default_value.is_some());

        // Build `# Type :: <sig>` annotation line.
        let type_sig = type_signature_comment(f, cx);

        let body_placeholder = txt("<body>");

        let binding = if has_defaults || literal_params.is_empty() && f.input_parameters.is_some() {
            // Attrset-pattern form: `{ a, b ? default, ... }: body`
            let mut members: Vec<Rendered> = literal_params
                .iter()
                .map(|lp| {
                    let base = ident(&lp.name);
                    if lp.default_value.is_some() {
                        base + sp() + punct("?") + sp() + txt("<default>")
                    } else {
                        base
                    }
                })
                .collect();
            // add open rest pattern when there are params
            if !members.is_empty() {
                members.push(txt("..."));
            }
            let pattern = arglist("{", members, "}");
            ident(name)
                + sp()
                + punct("=")
                + sp()
                + pattern
                + punct(":")
                + sp()
                + body_placeholder
                + punct(";")
        } else {
            // Curried form: `name = a: b: body`
            let mut lhs = ident(name) + sp() + punct("=");
            for lp in &literal_params {
                lhs = lhs + sp() + ident(&lp.name) + punct(":");
            }
            lhs + sp() + body_placeholder + punct(";")
        };

        type_sig + Doc::hardline() + binding
    }

    // -----------------------------------------------------------------------
    // interface
    // -----------------------------------------------------------------------

    /// Render a trait/interface as an attrset contract listing required attrs.
    ///
    /// ```nix
    /// # Iterator :: interface
    /// Iterator = {
    ///     next,
    ///     reset,
    /// };
    /// ```
    fn interface(&self, name: &str, def: &TraitDef, _vis: &Visibility, cx: &RenderCtx) -> Rendered {
        let comment = txt("# ").annotate(Annotation::Comment)
            + txt(name).annotate(Annotation::Comment)
            + txt(" :: interface").annotate(Annotation::Comment);

        let methods: Vec<Rendered> = def
            .required_methods
            .iter()
            .flatten()
            .chain(def.provided_methods.iter().flatten())
            .map(|m| interface_method(m, cx))
            .collect();

        let body = if methods.is_empty() {
            punct("{}")
        } else {
            block("{", methods, "}")
        };

        comment + Doc::hardline() + ident(name) + sp() + punct("=") + sp() + body + punct(";")
    }
}

// ---------------------------------------------------------------------------
// Type rendering (the `::` convention)
// ---------------------------------------------------------------------------

/// Render a [`Type`] in Nix's `::` documentation convention.
fn nix_ty(t: &Type, cx: &RenderCtx) -> Rendered {
    match t {
        Type::TypeReference(r) => nix_type_reference(r, cx),
        Type::Primitive(Primitive::Bytes) => punct("[") + tyname("Int") + punct("]"),
        Type::Primitive(p) => tyname(nix_primitive(p)),
        Type::GenericParam(g) => tyname(&g.name),
        Type::SelfType => tyname("Self"),
        // Slices and arrays → `[T]`
        Type::Slice(inner) | Type::Array { r#type: inner, .. } => {
            punct("[") + nix_ty(inner, cx) + punct("]")
        }
        // Tuples → `(A * B * C)` (type-product notation)
        Type::Tuple(ts) if ts.is_empty() => tyname("Null"),
        Type::Tuple(ts) => {
            punct("(") + Doc::join(txt(" * "), ts.iter().map(|t| nix_ty(t, cx))) + punct(")")
        }
        // Union: normalise `[T, Null]` → `T?`; otherwise `A | B`
        Type::Union(ts) => nix_union(ts, cx),
        // Intersection → `A & B`
        Type::Intersection(ts) => Doc::join(txt(" & "), ts.iter().map(|t| nix_ty(t, cx))),
        // Function pointer → `A -> B`
        Type::FunctionPointer(fp) => nix_fn_ptr(fp, cx),
        // Record literal → `{ field :: T; … }`
        Type::RecordLiteral(rec) => nix_record_literal(rec, cx),
        // Transparent wrappers — strip reference/pointer indirection
        Type::BorrowedRef { r#type, .. } | Type::RawPointer { r#type, .. } => nix_ty(r#type, cx),
        Type::Variadic(inner) => nix_ty(inner, cx) + txt("*"),
        Type::Any => tyname("Any"),
        Type::Infer => tyname("_"),
        Type::Never => tyname("Never"),
        // Fallback
        _ => tyname("Any"),
    }
}

/// Render a [`TypeReference`] in the Nix convention: `Name arg1 arg2` (space-separated).
fn nix_type_reference(r: &TypeReference, cx: &RenderCtx) -> Rendered {
    let sn = short_name(&r.identifier, cx);
    let type_args: Vec<Rendered> = r
        .generic_args
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|a| match a {
            GenericArg::Type(t) => Some(nix_ty(t, cx)),
            _ => None,
        })
        .collect();
    // Known mappings for Nix (attrset = record, list = slice).
    let name_lc = sn.to_ascii_lowercase();
    match (name_lc.as_str(), type_args.len()) {
        ("vec" | "hashset" | "btreeset", 1) => {
            return punct("[") + type_args[0].clone() + punct("]");
        }
        ("option", 1) => {
            return type_args[0].clone() + tyname("?");
        }
        ("hashmap" | "btreemap", 2) => {
            return punct("{") + type_args[0].clone() + txt(" :: ") + type_args[1].clone() + punct("}");
        }
        _ => {}
    }
    let base = tyname(sn);
    if type_args.is_empty() {
        base
    } else {
        base + sp() + Doc::join(sp(), type_args)
    }
}

/// Normalise a union: `[T, Null]` → `T?`; otherwise render `A | B`.
fn nix_union(ts: &[Type], cx: &RenderCtx) -> Rendered {
    // Check for a two-element union containing Null/Never (option-like).
    if ts.len() == 2 {
        let null_idx = ts.iter().position(|t| is_null_type(t));
        if let Some(ni) = null_idx {
            let other = &ts[1 - ni];
            return nix_ty(other, cx) + tyname("?");
        }
    }
    Doc::join(txt(" | "), ts.iter().map(|t| nix_ty(t, cx)))
}

/// Returns `true` if the type represents the absence of a value (Null/Never/unit).
fn is_null_type(t: &Type) -> bool {
    match t {
        Type::Never => true,
        Type::Tuple(v) if v.is_empty() => true,
        Type::TypeReference(r)
            if matches!(
                r.identifier.to_ascii_lowercase().as_str(),
                "null" | "none" | "option::none" | "nil"
            ) =>
        {
            true
        }
        _ => false,
    }
}

/// Render a function-pointer type as `A -> B -> C`.
fn nix_fn_ptr(fp: &FunctionPointer, cx: &RenderCtx) -> Rendered {
    let mut parts: Vec<Rendered> = fp
        .inputs
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|p| match p {
            Parameter::Literal(lp) => lp.r#type.as_ref().map(|t| nix_ty(t, cx)),
            _ => None,
        })
        .collect();

    let ret = fp
        .outputs
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .find_map(|p| match p {
            Parameter::Literal(lp) => lp.r#type.as_ref().map(|t| nix_ty(t, cx)),
            _ => None,
        })
        .unwrap_or_else(|| tyname("Null"));

    parts.push(ret);
    Doc::join(txt(" -> "), parts)
}

/// Render an inline record literal type: `{ field :: T; … }`.
fn nix_record_literal(rec: &Record, cx: &RenderCtx) -> Rendered {
    let entries: Vec<Rendered> = rec
        .fields
        .iter()
        .filter_map(|f| match f {
            Field::Known(kf) => {
                let name = field_key_name(&kf.key);
                let t = kf
                    .r#type
                    .as_ref()
                    .map(|t| nix_ty(t, cx))
                    .unwrap_or_else(|| tyname("Any"));
                Some(ident(&name) + txt(" :: ") + t + punct(";"))
            }
            _ => None,
        })
        .collect();
    if entries.is_empty() {
        return punct("{") + punct("}");
    }
    let inner = Doc::join(sp(), entries);
    punct("{") + sp() + inner + sp() + punct("}")
}

/// Primitive type names in Nix documentation conventions.
///
/// Note: `Bytes` is handled before this function is called in [`nix_ty`]
/// (it renders as `[Int]` with bracket syntax), so this arm is unreachable
/// in practice but kept for exhaustiveness.
fn nix_primitive(p: &Primitive) -> &'static str {
    match p {
        Primitive::Int(_) | Primitive::UInt(_) => "Int",
        Primitive::Float(_) => "Float",
        Primitive::Bool => "Bool",
        Primitive::String | Primitive::Char => "String",
        Primitive::Bytes => "Int",
        Primitive::Date => "String",
        Primitive::Address => "Int",
    }
}

// ---------------------------------------------------------------------------
// Record field helper
// ---------------------------------------------------------------------------

/// Render a single record field as a commented attrset entry:
/// ```nix
/// # x :: Float
/// x,
/// ```
fn record_field(f: &Field, cx: &RenderCtx) -> Option<Rendered> {
    let Field::Known(kf) = f else { return None };
    let name = field_key_name(&kf.key);
    let t = kf
        .r#type
        .as_ref()
        .map(|t| nix_ty(t, cx))
        .unwrap_or_else(|| tyname("Any"));
    let opt_marker = if kf.attributes.is_optional {
        tyname("?")
    } else {
        Doc::nil()
    };
    let type_comment = txt("# ").annotate(Annotation::Comment)
        + ident(&name).annotate(Annotation::Comment)
        + txt(" :: ").annotate(Annotation::Comment)
        + t.annotate(Annotation::Comment)
        + opt_marker.annotate(Annotation::Comment);
    Some(type_comment + Doc::hardline() + ident(&name) + punct(","))
}

// ---------------------------------------------------------------------------
// Sum variant helper
// ---------------------------------------------------------------------------

/// Render a sum variant as an attrset entry with optional field comment.
fn sum_variant(v: &SumVariant, cx: &RenderCtx) -> Rendered {
    match &v.data {
        None => ident(&v.name) + punct(","),
        Some(SumField::Tuple(tys)) => {
            let types_doc = Doc::join(
                txt(" * "),
                tys.iter().map(|t| nix_ty(t, cx)),
            );
            let comment = txt("# ").annotate(Annotation::Comment)
                + txt(&v.name).annotate(Annotation::Comment)
                + txt(" :: ").annotate(Annotation::Comment)
                + types_doc.annotate(Annotation::Comment);
            comment + Doc::hardline() + ident(&v.name) + punct(",")
        }
        Some(SumField::StructLike(fields)) => {
            let field_docs: Vec<Rendered> = fields
                .iter()
                .filter_map(|f| match f {
                    Field::Known(kf) => {
                        let fname = field_key_name(&kf.key);
                        let t = kf
                            .r#type
                            .as_ref()
                            .map(|t| nix_ty(t, cx))
                            .unwrap_or_else(|| tyname("Any"));
                        Some(
                            txt("# ").annotate(Annotation::Comment)
                                + txt(&fname).annotate(Annotation::Comment)
                                + txt(" :: ").annotate(Annotation::Comment)
                                + t.annotate(Annotation::Comment),
                        )
                    }
                    _ => None,
                })
                .collect();
            let comment = if field_docs.is_empty() {
                Doc::nil()
            } else {
                Doc::join(Doc::hardline(), field_docs) + Doc::hardline()
            };
            comment + ident(&v.name) + punct(",")
        }
    }
}

// ---------------------------------------------------------------------------
// Interface method helper
// ---------------------------------------------------------------------------

/// Render a trait method as an attrset entry with a type-signature comment.
fn interface_method(m: &TraitMethod, cx: &RenderCtx) -> Rendered {
    // Build a simplified `# name :: A -> B -> Ret` comment.
    let mut parts: Vec<Rendered> = m
        .parameters
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|p| match p {
            Parameter::Literal(lp) => lp.r#type.as_ref().map(|t| nix_ty(t, cx)),
            _ => None,
        })
        .collect();
    let ret = m
        .return_type
        .as_ref()
        .map(|t| nix_ty(t, cx))
        .unwrap_or_else(|| tyname("Null"));
    parts.push(ret);
    let sig = Doc::join(txt(" -> "), parts);
    let comment = txt("# ").annotate(Annotation::Comment)
        + txt(&m.name).annotate(Annotation::Comment)
        + txt(" :: ").annotate(Annotation::Comment)
        + sig.annotate(Annotation::Comment);
    comment + Doc::hardline() + ident(&m.name) + punct(",")
}

// ---------------------------------------------------------------------------
// Type-signature comment for functions
// ---------------------------------------------------------------------------

/// Build a `# Type :: <sig>` comment for a function.
fn type_signature_comment(f: &Function, cx: &RenderCtx) -> Rendered {
    let info = analyze_generics(f.generics.as_ref());

    // Render generic type params as a leading `forall a b.` prefix when present.
    let generics_prefix = if info.types.is_empty() {
        Doc::nil()
    } else {
        txt("forall ").annotate(Annotation::Comment)
            + Doc::join(
                sp(),
                info.types.iter().map(|n| tyname(n).annotate(Annotation::Comment)),
            )
            + txt(". ").annotate(Annotation::Comment)
    };

    let mut parts: Vec<Rendered> = f
        .input_parameters
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter_map(|p| match p {
            Parameter::Literal(lp) => lp.r#type.as_ref().map(|t| nix_ty(t, cx)),
            _ => None,
        })
        .collect();

    let ret = f
        .output_parameters
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .find_map(|p| match p {
            Parameter::Literal(lp) => lp.r#type.as_ref().map(|t| nix_ty(t, cx)),
            _ => None,
        })
        .unwrap_or_else(|| tyname("Null"));

    parts.push(ret);

    let sig = Doc::join(txt(" -> "), parts);

    txt("# Type :: ").annotate(Annotation::Comment)
        + generics_prefix
        + sig.annotate(Annotation::Comment)
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn field_key_name(key: &FieldKey) -> String {
    match key {
        FieldKey::Ident(n) => n.clone(),
        FieldKey::Index(i) => format!("field{i}"),
        FieldKey::Computed(_) => "field".into(),
    }
}
