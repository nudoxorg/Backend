//! Signature rendering: `nudox_ir::entry::Entry` → `Vec<SigToken>`.
//!
//! # LR-4: one renderer, everywhere
//!
//! This module is the **only** place that converts an `Entry`'s kind data into
//! typed signature tokens. Every surface that shows a signature — the symbol
//! page header, search hit rows, quick-peek, MCP output, and the diff view —
//! uses the `tokens` function below. Rendering a signature by `format!` or any
//! other means outside this module is a review-blocking violation of LR-4.
//!
//! # Design
//!
//! Tokens are built by walking the `Kind` and recursively walking `Type`
//! values. The only allocation is the returned `Vec<SigToken>`; each token
//! either borrows a `&'static str` (keywords and punctuation) or wraps a
//! `SharedStr` (dynamic text).
//!
//! `Type::Nominal(RawRef)` produces a `SigToken::Ty` with `target = Some(key)`
//! when the ref is resolved (`Intro` or `Foreign`), and `target = None` for
//! unresolved `Local` refs (which should not appear in sealed tables).

use nudox_ir::{
    entry::{Entry, Visibility},
    index::Ref,
    kind::Kind,
    kinds::{
        FieldKey, FnModifier, Function, GenericParam, Param, Receiver, RecordForm, Type,
        ty::{Primitive, TemplatePart, TupleElement, Variance, Width},
    },
};
use nudox_store::package::PackageView;

use crate::wire::{SharedStr, SigToken, SymbolKey};

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Render `entry`'s kind as a sequence of typed signature tokens.
///
/// This is the single source of signature rendering in the system (LR-4).
/// The result is consumed identically by the symbol-page header, search rows,
/// MCP output, and any other surface that needs a typed signature.
///
/// # Invariants
///
/// * No token has empty text. The fallback is `"?"` rather than `""`.
/// * `SigToken::Ty { target: Some(_) }` only when the underlying
///   `Type::Nominal` resolves through the corpus.
/// * Modifiers are emitted in declaration order; keywords precede identifiers.
pub fn tokens(entry: &Entry, package: &PackageView) -> Vec<SigToken> {
    let name = entry.sym().name.as_str();

    let inner = match entry.kind().as_owned_kind() {
        Some(k) => k,
        None => {
            // EntryInner::Reference — re-export; render as `pub use <name>`
            return vec![
                SigToken::Kw("pub"),
                SigToken::Ws,
                SigToken::Kw("use"),
                SigToken::Ws,
                SigToken::Ident(SharedStr::from(name)),
            ];
        }
    };

    match inner {
        Kind::Module(_) => vec![
            SigToken::Kw("mod"),
            SigToken::Ws,
            SigToken::Ident(SharedStr::from(name)),
        ],

        Kind::Reexport(_) => vec![
            SigToken::Kw("pub"),
            SigToken::Ws,
            SigToken::Kw("use"),
            SigToken::Ws,
            SigToken::Ident(SharedStr::from(name)),
        ],

        Kind::Record(r) => {
            let kw = match r.form {
                RecordForm::Union => "union",
                _ => "struct",
            };
            let mut toks = vec![
                SigToken::Kw(kw),
                SigToken::Ws,
                SigToken::Ident(SharedStr::from(name)),
            ];
            push_generics(&mut toks, &r.generics, package);
            toks
        }

        Kind::Enum(e) => {
            let mut toks = vec![
                SigToken::Kw("enum"),
                SigToken::Ws,
                SigToken::Ident(SharedStr::from(name)),
            ];
            push_generics(&mut toks, &e.generics, package);
            toks
        }

        Kind::Trait(t) => {
            let mut toks = Vec::new();
            if t.flags.is_unsafe {
                toks.push(SigToken::Kw("unsafe"));
                toks.push(SigToken::Ws);
            }
            if t.flags.is_auto {
                toks.push(SigToken::Kw("auto"));
                toks.push(SigToken::Ws);
            }
            toks.push(SigToken::Kw("trait"));
            toks.push(SigToken::Ws);
            toks.push(SigToken::Ident(SharedStr::from(name)));
            push_generics(&mut toks, &t.generics, package);
            if !t.supers.is_empty() {
                toks.push(SigToken::Punct(":"));
                toks.push(SigToken::Ws);
                let mut first = true;
                for sup in t.supers.iter() {
                    if !first {
                        toks.push(SigToken::Ws);
                        toks.push(SigToken::Punct("+"));
                        toks.push(SigToken::Ws);
                    }
                    first = false;
                    push_type(&mut toks, sup, package);
                }
            }
            toks
        }

        Kind::Impl(i) => {
            let mut toks = Vec::new();
            toks.push(SigToken::Kw("impl"));
            push_generics(&mut toks, &i.generics, package);
            toks.push(SigToken::Ws);
            if i.flags.negative {
                toks.push(SigToken::Punct("!"));
            }
            if let Some(of) = &i.of {
                push_type(&mut toks, of, package);
                toks.push(SigToken::Ws);
                toks.push(SigToken::Kw("for"));
                toks.push(SigToken::Ws);
            }
            push_type(&mut toks, &i.self_ty, package);
            toks
        }

        Kind::Alias(a) => {
            let mut toks = vec![
                SigToken::Kw("type"),
                SigToken::Ws,
                SigToken::Ident(SharedStr::from(name)),
            ];
            push_generics(&mut toks, &a.generics, package);
            if let Some(target) = &a.target {
                toks.push(SigToken::Ws);
                toks.push(SigToken::Punct("="));
                toks.push(SigToken::Ws);
                push_type(&mut toks, target, package);
            }
            toks
        }

        Kind::Const(c) => {
            let mut toks = vec![
                SigToken::Kw("const"),
                SigToken::Ws,
                SigToken::Ident(SharedStr::from(name)),
            ];
            toks.push(SigToken::Punct(":"));
            toks.push(SigToken::Ws);
            push_type(&mut toks, &c.ty, package);
            if let Some(val) = &c.value {
                toks.push(SigToken::Ws);
                toks.push(SigToken::Punct("="));
                toks.push(SigToken::Ws);
                toks.push(SigToken::Ident(SharedStr::from(val.as_str())));
            }
            toks
        }

        Kind::Static(s) => {
            let mut toks = vec![SigToken::Kw("static"), SigToken::Ws];
            if s.mutable {
                toks.push(SigToken::Kw("mut"));
                toks.push(SigToken::Ws);
            }
            toks.push(SigToken::Ident(SharedStr::from(name)));
            toks.push(SigToken::Punct(":"));
            toks.push(SigToken::Ws);
            push_type(&mut toks, &s.ty, package);
            toks
        }

        Kind::Function(f) => render_function(f, name, package),

        Kind::Field(field) => {
            let mut toks = Vec::new();
            let vis = entry.sym().visibility;
            push_visibility(&mut toks, vis);
            match field.key {
                FieldKey::Positional(idx) => {
                    toks.push(SigToken::Ident(SharedStr::from(idx.to_string().as_str())));
                }
                FieldKey::Named => {
                    toks.push(SigToken::Ident(SharedStr::from(name)));
                }
            }
            if let Some(ty) = &field.ty {
                toks.push(SigToken::Punct(":"));
                toks.push(SigToken::Ws);
                push_type(&mut toks, ty, package);
            }
            toks
        }

        Kind::Variant(v) => {
            let mut toks = vec![SigToken::Ident(SharedStr::from(name))];
            if let Some(discr) = &v.discr {
                toks.push(SigToken::Ws);
                toks.push(SigToken::Punct("="));
                toks.push(SigToken::Ws);
                toks.push(SigToken::Ident(SharedStr::from(discr.as_str())));
            }
            toks
        }

        Kind::Param(p) => {
            let mut toks = vec![SigToken::Ident(SharedStr::from(name))];
            if let Some(ty) = &p.ty {
                toks.push(SigToken::Punct(":"));
                toks.push(SigToken::Ws);
                push_type(&mut toks, ty, package);
            }
            toks
        }
    }
}

// ---------------------------------------------------------------------------
// Function rendering
// ---------------------------------------------------------------------------

fn render_function(f: &Function, name: &str, package: &PackageView) -> Vec<SigToken> {
    let mut toks = Vec::new();

    // Modifiers in standard order
    for m in f.modifiers.iter() {
        match m {
            FnModifier::Async => {
                toks.push(SigToken::Kw("async"));
                toks.push(SigToken::Ws);
            }
            FnModifier::Const => {
                toks.push(SigToken::Kw("const"));
                toks.push(SigToken::Ws);
            }
            FnModifier::Unsafe => {
                toks.push(SigToken::Kw("unsafe"));
                toks.push(SigToken::Ws);
            }
            FnModifier::Pure | FnModifier::Generator => {}
        }
    }

    // ABI
    if let Some(abi) = &f.abi {
        toks.push(SigToken::Kw("extern"));
        toks.push(SigToken::Ws);
        toks.push(SigToken::Ident(SharedStr::from(
            format!("\"{}\"", abi).as_str(),
        )));
        toks.push(SigToken::Ws);
    }

    toks.push(SigToken::Kw("fn"));
    toks.push(SigToken::Ws);
    toks.push(SigToken::Ident(SharedStr::from(name)));
    push_generics(&mut toks, &f.generics, package);

    // Parameters
    toks.push(SigToken::Punct("("));
    let mut first = true;

    // Receiver
    if let Some(recv) = &f.receiver {
        first = false;
        match recv {
            Receiver::Owned => toks.push(SigToken::Kw("self")),
            Receiver::SharedRef => {
                toks.push(SigToken::Punct("&"));
                toks.push(SigToken::Kw("self"));
            }
            Receiver::MutRef => {
                toks.push(SigToken::Punct("&"));
                toks.push(SigToken::Kw("mut"));
                toks.push(SigToken::Ws);
                toks.push(SigToken::Kw("self"));
            }
            Receiver::Arbitrary => {
                toks.push(SigToken::Kw("self"));
                toks.push(SigToken::Punct(":"));
                toks.push(SigToken::Ws);
                toks.push(SigToken::Ident(SharedStr::from("_")));
            }
        }
    }

    // Input params — resolve each through the view if possible
    let view = package.view();
    for param_ref in f.input_params.iter() {
        if !first {
            toks.push(SigToken::Punct(","));
            toks.push(SigToken::Ws);
        }
        first = false;

        match param_ref {
            Ref::Intro(id) => {
                if let Some(param_entry) = view.entry(*id) {
                    toks.push(SigToken::Ident(SharedStr::from(
                        param_entry.sym().name.as_str(),
                    )));
                    if let Some(Kind::Param(p)) = param_entry.kind().as_owned_kind() {
                        if let Some(ty) = &p.ty {
                            toks.push(SigToken::Punct(":"));
                            toks.push(SigToken::Ws);
                            push_type(&mut toks, ty, package);
                        }
                    }
                } else {
                    toks.push(SigToken::Ident(SharedStr::from("_")));
                }
            }
            _ => {
                toks.push(SigToken::Ident(SharedStr::from("_")));
            }
        }
    }

    toks.push(SigToken::Punct(")"));

    // Return type
    if !f.output_params.is_empty() {
        toks.push(SigToken::Ws);
        toks.push(SigToken::Punct("->"));
        toks.push(SigToken::Ws);

        if f.output_params.len() == 1 {
            let op_ref = &f.output_params[0];
            push_param_type(&mut toks, op_ref, package);
        } else {
            // Multiple outputs: render as tuple
            toks.push(SigToken::Punct("("));
            let mut first_out = true;
            for op_ref in f.output_params.iter() {
                if !first_out {
                    toks.push(SigToken::Punct(","));
                    toks.push(SigToken::Ws);
                }
                first_out = false;
                push_param_type(&mut toks, op_ref, package);
            }
            toks.push(SigToken::Punct(")"));
        }
    }

    toks
}

/// Append the type of a `Ref<Param>` to `toks`, or `"?"` if unresolvable.
fn push_param_type(toks: &mut Vec<SigToken>, param_ref: &Ref<Param>, package: &PackageView) {
    let view = package.view();
    let mut rendered = false;
    if let Ref::Intro(id) = param_ref {
        if let Some(param_entry) = view.entry(*id) {
            if let Some(Kind::Param(p)) = param_entry.kind().as_owned_kind() {
                if let Some(ty) = &p.ty {
                    push_type(toks, ty, package);
                    rendered = true;
                }
            }
        }
    }
    if !rendered {
        toks.push(SigToken::Ident(SharedStr::from("?")));
    }
}

// ---------------------------------------------------------------------------
// Type rendering
// ---------------------------------------------------------------------------

/// Append tokens for a `Type` onto `toks`.
///
/// This function is recursive for compound types but the recursion depth is
/// bounded by the IR's structural depth (typically < 8 for real signatures).
/// No type variant produces empty token text — the fallback is `"?"`.
pub(crate) fn push_type(toks: &mut Vec<SigToken>, ty: &Type, package: &PackageView) {
    match ty {
        Type::SelfType => {
            toks.push(SigToken::Ty {
                text: SharedStr::from("Self"),
                target: None,
            });
        }

        Type::Never => {
            toks.push(SigToken::Ty {
                text: SharedStr::from("!"),
                target: None,
            });
        }

        Type::Any => {
            toks.push(SigToken::Ty {
                text: SharedStr::from("any"),
                target: None,
            });
        }

        Type::Inferred => {
            toks.push(SigToken::Ty {
                text: SharedStr::from("_"),
                target: None,
            });
        }

        Type::Primitive(prim) => {
            let text = primitive_text(prim, package);
            toks.push(SigToken::Ty { text, target: None });
        }

        Type::TypeVar(name) => {
            toks.push(SigToken::Generic(SharedStr::from(name.as_str())));
        }

        Type::Nominal(raw_ref) => {
            let (text, target) = resolve_nominal(raw_ref, package);
            toks.push(SigToken::Ty { text, target });
        }

        Type::Apply { base, args } => {
            push_type(toks, base, package);
            toks.push(SigToken::Punct("<"));
            let mut first = true;
            for arg in args.iter() {
                if !first {
                    toks.push(SigToken::Punct(","));
                    toks.push(SigToken::Ws);
                }
                first = false;
                push_type(toks, arg, package);
            }
            toks.push(SigToken::Punct(">"));
        }

        Type::Slice(inner) => {
            toks.push(SigToken::Punct("["));
            push_type(toks, inner, package);
            toks.push(SigToken::Punct("]"));
        }

        Type::Array { ty: inner, length } => {
            toks.push(SigToken::Punct("["));
            push_type(toks, inner, package);
            toks.push(SigToken::Punct(";"));
            toks.push(SigToken::Ws);
            toks.push(SigToken::Ident(SharedStr::from(
                length.to_string().as_str(),
            )));
            toks.push(SigToken::Punct("]"));
        }

        Type::Tuple(elems) => {
            toks.push(SigToken::Punct("("));
            let mut first = true;
            for elem in elems.iter() {
                if !first {
                    toks.push(SigToken::Punct(","));
                    toks.push(SigToken::Ws);
                }
                first = false;
                match elem {
                    TupleElement::Positional(ty) => push_type(toks, ty, package),
                    TupleElement::Named { label, ty } => {
                        toks.push(SigToken::Ident(SharedStr::from(label.as_str())));
                        toks.push(SigToken::Punct(":"));
                        toks.push(SigToken::Ws);
                        push_type(toks, ty, package);
                    }
                }
            }
            toks.push(SigToken::Punct(")"));
        }

        Type::Union(tys) => {
            if tys.is_empty() {
                toks.push(SigToken::Ty {
                    text: SharedStr::from("?"),
                    target: None,
                });
            } else {
                let mut first = true;
                for ty in tys.iter() {
                    if !first {
                        toks.push(SigToken::Ws);
                        toks.push(SigToken::Punct("|"));
                        toks.push(SigToken::Ws);
                    }
                    first = false;
                    push_type(toks, ty, package);
                }
            }
        }

        Type::Intersection(tys) => {
            if tys.is_empty() {
                toks.push(SigToken::Ty {
                    text: SharedStr::from("?"),
                    target: None,
                });
            } else {
                let mut first = true;
                for ty in tys.iter() {
                    if !first {
                        toks.push(SigToken::Ws);
                        toks.push(SigToken::Punct("&"));
                        toks.push(SigToken::Ws);
                    }
                    first = false;
                    push_type(toks, ty, package);
                }
            }
        }

        Type::FunctionPointer { params, ret, abi } => {
            if let Some(abi) = abi {
                toks.push(SigToken::Kw("extern"));
                toks.push(SigToken::Ws);
                toks.push(SigToken::Ident(SharedStr::from(
                    format!("\"{}\"", abi).as_str(),
                )));
                toks.push(SigToken::Ws);
            }
            toks.push(SigToken::Kw("fn"));
            toks.push(SigToken::Punct("("));
            let mut first = true;
            for p in params.iter() {
                if !first {
                    toks.push(SigToken::Punct(","));
                    toks.push(SigToken::Ws);
                }
                first = false;
                push_type(toks, p, package);
            }
            toks.push(SigToken::Punct(")"));
            if let Some(r) = ret {
                toks.push(SigToken::Ws);
                toks.push(SigToken::Punct("->"));
                toks.push(SigToken::Ws);
                push_type(toks, r, package);
            }
        }

        Type::Wildcard { variance, bound } => {
            let prefix = match variance {
                Variance::Invariant => "?",
                Variance::Covariant => "out",
                Variance::Contravariant => "in",
            };
            if let Some(b) = bound {
                let mut bound_toks: Vec<SigToken> = Vec::new();
                push_type(&mut bound_toks, b, package);
                let bound_text = tokens_to_text(&bound_toks);
                toks.push(SigToken::Ty {
                    text: SharedStr::from(format!("{} extends {}", prefix, bound_text).as_str()),
                    target: None,
                });
            } else {
                toks.push(SigToken::Ty {
                    text: SharedStr::from(prefix),
                    target: None,
                });
            }
        }

        Type::Annotated { inner, annotation } => {
            let ann_text = if let Some(arg) = &annotation.arg {
                format!("@{}({}) ", annotation.token, arg)
            } else {
                format!("@{} ", annotation.token)
            };
            toks.push(SigToken::Ident(SharedStr::from(ann_text.as_str())));
            push_type(toks, inner, package);
        }

        Type::ImplTrait(bounds) => {
            toks.push(SigToken::Kw("impl"));
            toks.push(SigToken::Ws);
            push_plus_bounds(toks, bounds, package);
        }

        Type::DynTrait(bounds) => {
            toks.push(SigToken::Kw("dyn"));
            toks.push(SigToken::Ws);
            push_plus_bounds(toks, bounds, package);
        }

        Type::QualifiedPath {
            self_ty,
            trait_ref,
            assoc,
        } => {
            toks.push(SigToken::Punct("<"));
            push_type(toks, self_ty, package);
            if let Some(tr) = trait_ref {
                toks.push(SigToken::Ws);
                toks.push(SigToken::Kw("as"));
                toks.push(SigToken::Ws);
                push_type(toks, tr, package);
            }
            toks.push(SigToken::Punct(">"));
            toks.push(SigToken::Punct("::"));
            toks.push(SigToken::Ident(SharedStr::from(assoc.as_str())));
        }

        Type::Conditional {
            check,
            extends_ty,
            then_ty,
            else_ty,
        } => {
            push_type(toks, check, package);
            toks.push(SigToken::Ws);
            toks.push(SigToken::Kw("extends"));
            toks.push(SigToken::Ws);
            push_type(toks, extends_ty, package);
            toks.push(SigToken::Ws);
            toks.push(SigToken::Punct("?"));
            toks.push(SigToken::Ws);
            push_type(toks, then_ty, package);
            toks.push(SigToken::Ws);
            toks.push(SigToken::Punct(":"));
            toks.push(SigToken::Ws);
            push_type(toks, else_ty, package);
        }

        Type::Mapped {
            key_var,
            source,
            value,
            readonly,
            optional,
        } => {
            use nudox_ir::kinds::ty::MappedModifier;
            toks.push(SigToken::Punct("{"));
            if *readonly == MappedModifier::Add {
                toks.push(SigToken::Ws);
                toks.push(SigToken::Kw("readonly"));
            }
            toks.push(SigToken::Ws);
            toks.push(SigToken::Punct("["));
            toks.push(SigToken::Generic(SharedStr::from(key_var.as_str())));
            toks.push(SigToken::Ws);
            toks.push(SigToken::Kw("in"));
            toks.push(SigToken::Ws);
            push_type(toks, source, package);
            toks.push(SigToken::Punct("]"));
            if *optional == MappedModifier::Add {
                toks.push(SigToken::Punct("?"));
            }
            toks.push(SigToken::Punct(":"));
            toks.push(SigToken::Ws);
            push_type(toks, value, package);
            toks.push(SigToken::Ws);
            toks.push(SigToken::Punct("}"));
        }

        Type::TemplateLiteral(parts) => {
            let mut text = String::from("`");
            for part in parts.iter() {
                match part {
                    TemplatePart::Literal(s) => text.push_str(s),
                    TemplatePart::Interpolated(ty) => {
                        let mut inner_toks: Vec<SigToken> = Vec::new();
                        push_type(&mut inner_toks, ty, package);
                        text.push_str("${");
                        text.push_str(&tokens_to_text(&inner_toks));
                        text.push('}');
                    }
                }
            }
            text.push('`');
            toks.push(SigToken::Ty {
                text: SharedStr::from(text.as_str()),
                target: None,
            });
        }

        Type::AnonymousRecord { form: _, members } => {
            toks.push(SigToken::Punct("{"));
            toks.push(SigToken::Ws);
            let mut first = true;
            for m in members.iter() {
                if !first {
                    toks.push(SigToken::Punct(";"));
                    toks.push(SigToken::Ws);
                }
                first = false;
                if m.readonly {
                    toks.push(SigToken::Kw("readonly"));
                    toks.push(SigToken::Ws);
                }
                toks.push(SigToken::Ident(SharedStr::from(m.name.as_str())));
                if m.optional {
                    toks.push(SigToken::Punct("?"));
                }
                toks.push(SigToken::Punct(":"));
                toks.push(SigToken::Ws);
                push_type(toks, &m.ty, package);
            }
            if !members.is_empty() {
                toks.push(SigToken::Ws);
            }
            toks.push(SigToken::Punct("}"));
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Render a list of `Type` bounds joined by ` + `.
fn push_plus_bounds(toks: &mut Vec<SigToken>, bounds: &[Type], package: &PackageView) {
    let mut first = true;
    for b in bounds.iter() {
        if !first {
            toks.push(SigToken::Ws);
            toks.push(SigToken::Punct("+"));
            toks.push(SigToken::Ws);
        }
        first = false;
        push_type(toks, b, package);
    }
}

/// Resolve a `Nominal` reference to (display_text, Option<SymbolKey>).
fn resolve_nominal(
    raw: &nudox_ir::index::RawRef,
    package: &PackageView,
) -> (SharedStr, Option<SymbolKey>) {
    match raw {
        Ref::Intro(id) => {
            // Same-package reference
            let lineage = package.lineage().clone();
            let key = nudox_ir::change::StableRef::new(lineage, *id);
            // Use precomputed path when available, else fallback to hex
            let text = if let Some(path) = package.indexes().path_of(*id) {
                SharedStr::from(path.as_ref())
            } else {
                SharedStr::from(id.to_hex().as_str())
            };
            (text, Some(key))
        }
        Ref::Foreign(sr) => {
            // Cross-package reference — use Display of StableRef
            let text = SharedStr::from(sr.to_string().as_str());
            (text, Some(sr.clone()))
        }
        Ref::Local(_) => {
            // Should not appear in a sealed table
            (SharedStr::from("?"), None)
        }
    }
}

/// Render a `Primitive` type to display text.
fn primitive_text(prim: &Primitive, package: &PackageView) -> SharedStr {
    match prim {
        Primitive::Bool => SharedStr::from("bool"),
        Primitive::Char => SharedStr::from("char"),
        Primitive::Str => SharedStr::from("str"),
        Primitive::Integer { signed, width } => {
            let prefix = if *signed { "i" } else { "u" };
            let w = width_str(width);
            SharedStr::from(format!("{}{}", prefix, w).as_str())
        }
        Primitive::Float(width) => {
            let w = width_str(width);
            SharedStr::from(format!("f{}", w).as_str())
        }
        Primitive::MutPointer(inner) => {
            let mut inner_toks = Vec::new();
            push_type(&mut inner_toks, inner, package);
            let inner_text = tokens_to_text(&inner_toks);
            SharedStr::from(format!("*mut {}", inner_text).as_str())
        }
        Primitive::ConstPointer(inner) => {
            let mut inner_toks = Vec::new();
            push_type(&mut inner_toks, inner, package);
            let inner_text = tokens_to_text(&inner_toks);
            SharedStr::from(format!("*const {}", inner_text).as_str())
        }
        Primitive::Reference {
            lifetime,
            mutable,
            ty,
        } => {
            let mut inner_toks = Vec::new();
            push_type(&mut inner_toks, ty, package);
            let inner_text = tokens_to_text(&inner_toks);
            let lt = lifetime
                .as_deref()
                .map(|l| format!("'{} ", l))
                .unwrap_or_default();
            let mut_kw = if *mutable { "mut " } else { "" };
            SharedStr::from(format!("&{}{}{}", lt, mut_kw, inner_text).as_str())
        }
        Primitive::Builtin(name) => SharedStr::from(name.as_str()),
    }
}

fn width_str(w: &Width) -> String {
    match w {
        Width::Fixed(n) => n.get().to_string(),
        Width::Arch => "size".to_owned(),
    }
}

/// Flatten a token vec to a plain string (for use in nested text contexts).
pub(crate) fn tokens_to_text(toks: &[SigToken]) -> String {
    let mut out = String::new();
    for tok in toks {
        match tok {
            SigToken::Kw(s) | SigToken::Punct(s) => out.push_str(s),
            SigToken::Ident(s)
            | SigToken::Ty { text: s, .. }
            | SigToken::Generic(s)
            | SigToken::Lifetime(s) => out.push_str(s),
            SigToken::Ws => out.push(' '),
        }
    }
    out
}

/// Push generic parameters `<T, U, …>` onto `toks`.
fn push_generics(toks: &mut Vec<SigToken>, generics: &[GenericParam], package: &PackageView) {
    if generics.is_empty() {
        return;
    }
    toks.push(SigToken::Punct("<"));
    let mut first = true;
    for gp in generics.iter() {
        if !first {
            toks.push(SigToken::Punct(","));
            toks.push(SigToken::Ws);
        }
        first = false;
        match gp {
            GenericParam::Lifetime { name } => {
                toks.push(SigToken::Lifetime(SharedStr::from(
                    format!("'{}", name).as_str(),
                )));
            }
            GenericParam::Type { name, bounds, .. } => {
                toks.push(SigToken::Generic(SharedStr::from(name.as_str())));
                if !bounds.is_empty() {
                    toks.push(SigToken::Punct(":"));
                    toks.push(SigToken::Ws);
                    push_plus_bounds(toks, bounds, package);
                }
            }
            GenericParam::Const { name, .. } => {
                toks.push(SigToken::Kw("const"));
                toks.push(SigToken::Ws);
                toks.push(SigToken::Generic(SharedStr::from(name.as_str())));
            }
        }
    }
    toks.push(SigToken::Punct(">"));
}

/// Push a visibility prefix token if it's meaningful.
fn push_visibility(toks: &mut Vec<SigToken>, vis: Visibility) {
    match vis {
        Visibility::Public => {
            toks.push(SigToken::Kw("pub"));
            toks.push(SigToken::Ws);
        }
        Visibility::Crate => {
            toks.push(SigToken::Kw("pub(crate)"));
            toks.push(SigToken::Ws);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::{
        apply::PristineIntroTable,
        change::{EcosystemId, IntroId, PackageLineageId, PackageName},
        entry::{Entry, Node, Symbol, Visibility as IrVis},
        index::Ref,
        kind::Kind,
        kinds::{
            Alias, Const, Field, FieldKey, Function, GenericParam, Module, Record, Type,
            ty::Primitive,
        },
        view::IrView,
    };
    use nudox_store::package::{PackageView, Provenance};

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn lineage() -> PackageLineageId {
        PackageLineageId::new(EcosystemId::new("test"), PackageName::new("sig-tests"))
    }

    fn id(n: u8) -> IntroId {
        IntroId::from_raw([n; 32])
    }

    fn sym(name: &str) -> Symbol {
        Symbol {
            name: name.to_owned(),
            visibility: IrVis::Public,
            documentation: String::new(),
            source: std::path::PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        }
    }

    fn raw_entry(sym: Symbol, kind: Kind) -> Entry {
        Entry::new(sym, Node::build(None::<nudox_ir::index::RawRef>, []), kind)
    }

    /// Build a `PackageView` from a list of (IntroId, Entry, Option<parent>).
    fn make_package(entries: Vec<(IntroId, Entry, Option<IntroId>)>) -> PackageView {
        let mut table = PristineIntroTable::new();
        for (intro, entry, parent) in entries {
            table.insert_live(intro, entry, parent);
        }
        let view = IrView::with_package(lineage(), table);
        PackageView::build(view, Provenance::TrustedLocal)
    }

    // -----------------------------------------------------------------------
    // Helper assertions
    // -----------------------------------------------------------------------

    /// Every token in the result must have non-empty text.
    fn assert_no_empty(toks: &[SigToken], ctx: &str) {
        for tok in toks {
            let text = match tok {
                SigToken::Kw(s) | SigToken::Punct(s) => s.to_string(),
                SigToken::Ident(s)
                | SigToken::Ty { text: s, .. }
                | SigToken::Generic(s)
                | SigToken::Lifetime(s) => s.to_string(),
                SigToken::Ws => " ".to_string(),
            };
            assert!(!text.is_empty(), "empty token in {}: {:?}", ctx, tok);
        }
    }

    fn text_of(toks: &[SigToken]) -> String {
        tokens_to_text(toks)
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn module_renders_mod_keyword() {
        let pkg = make_package(vec![(
            id(1),
            raw_entry(sym("root"), Kind::Module(Module)),
            None,
        )]);
        let e = pkg.view().entry(id(1)).unwrap();
        let toks = tokens(e, &pkg);
        assert_no_empty(&toks, "module");
        let t = text_of(&toks);
        assert!(t.contains("mod"), "expected mod kw: {}", t);
        assert!(t.contains("root"), "expected name: {}", t);
    }

    #[test]
    fn record_struct_renders_struct() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(sym("Point"), Kind::Record(Record::builder().build())),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        assert_no_empty(&toks, "struct");
        let t = text_of(&toks);
        assert!(t.contains("struct"), "expected struct: {}", t);
        assert!(t.contains("Point"), "expected name: {}", t);
    }

    #[test]
    fn record_union_renders_union() {
        use nudox_ir::kinds::RecordForm;
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("U"),
                    Kind::Record(Record::builder().form(RecordForm::Union).build()),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        let t = text_of(&toks);
        assert!(t.contains("union"), "expected union kw: {}", t);
    }

    #[test]
    fn const_renders_value() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("MAX"),
                    Kind::Const(
                        Const::builder()
                            .ty(Type::U64)
                            .value("1024".to_owned())
                            .build(),
                    ),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        assert_no_empty(&toks, "const");
        let t = text_of(&toks);
        assert!(t.contains("const"), "expected const kw: {}", t);
        assert!(t.contains("1024"), "expected value: {}", t);
    }

    #[test]
    fn static_mutable_renders_mut() {
        use nudox_ir::kinds::Static;
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("COUNTER"),
                    Kind::Static(Static {
                        ty: Type::I32,
                        mutable: true,
                    }),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        let t = text_of(&toks);
        assert!(t.contains("mut"), "expected mut: {}", t);
    }

    #[test]
    fn function_no_params_renders_parens() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(sym("noop"), Kind::Function(Function::builder().build())),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        assert_no_empty(&toks, "fn noop");
        let t = text_of(&toks);
        assert!(t.contains("fn"), "expected fn: {}", t);
        assert!(t.contains("noop"), "expected name: {}", t);
        assert!(t.contains("()"), "expected parens: {}", t);
    }

    #[test]
    fn function_shared_ref_receiver_renders_ref_self() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("clone"),
                    Kind::Function(Function::builder().receiver(Receiver::SharedRef).build()),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        let t = text_of(&toks);
        assert!(t.contains("&self"), "expected &self: {}", t);
    }

    #[test]
    fn alias_with_target_renders_eq() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("MyStr"),
                    Kind::Alias(
                        Alias::builder()
                            .target(Type::Primitive(Primitive::Str))
                            .build(),
                    ),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        let t = text_of(&toks);
        assert!(t.contains('='), "expected = in alias: {}", t);
        assert!(t.contains("str"), "expected target str: {}", t);
    }

    #[test]
    fn type_union_renders_pipe() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("StrOrNum"),
                    Kind::Alias(
                        Alias::builder()
                            .target(Type::Union(
                                [Type::Primitive(Primitive::Str), Type::I32].into(),
                            ))
                            .build(),
                    ),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        assert!(
            toks.iter().any(|t| matches!(t, SigToken::Punct("|"))),
            "union should produce pipe"
        );
    }

    #[test]
    fn type_function_pointer_renders_fn_kw() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("Cb"),
                    Kind::Alias(
                        Alias::builder()
                            .target(Type::FunctionPointer {
                                params: [Type::I32].into(),
                                ret: Some(Box::new(Type::Primitive(Primitive::Bool))),
                                abi: None,
                            })
                            .build(),
                    ),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        assert!(
            toks.iter().any(|t| matches!(t, SigToken::Kw("fn"))),
            "fn pointer should produce fn kw"
        );
    }

    #[test]
    fn type_nominal_same_package_resolves_to_target() {
        // x: Point  — Point is id(2) in the same package
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(sym("Point"), Kind::Record(Record::builder().build())),
                Some(id(1)),
            ),
            (
                id(3),
                raw_entry(
                    sym("x"),
                    Kind::Field(
                        Field::builder()
                            .key(FieldKey::Named)
                            .ty(Type::Nominal(Ref::Intro(id(2))))
                            .build(),
                    ),
                ),
                Some(id(2)),
            ),
        ]);
        let e = pkg.view().entry(id(3)).unwrap();
        let toks = tokens(e, &pkg);
        assert_no_empty(&toks, "x");
        let has_target = toks.iter().any(|t| {
            matches!(
                t,
                SigToken::Ty {
                    target: Some(_),
                    ..
                }
            )
        });
        assert!(
            has_target,
            "Nominal ref in same pkg should resolve to target"
        );
    }

    #[test]
    fn type_never_renders_bang() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("N"),
                    Kind::Alias(Alias::builder().target(Type::Never).build()),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        let t = text_of(&toks);
        assert!(t.contains('!'), "Never should render as '!'");
    }

    #[test]
    fn type_impl_trait_renders_impl_kw() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("it"),
                    Kind::Alias(
                        Alias::builder()
                            .target(Type::ImplTrait([Type::Any].into()))
                            .build(),
                    ),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        assert!(
            toks.iter().any(|t| matches!(t, SigToken::Kw("impl"))),
            "ImplTrait should produce impl kw"
        );
    }

    #[test]
    fn type_dyn_trait_renders_dyn_kw() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("dt"),
                    Kind::Alias(
                        Alias::builder()
                            .target(Type::DynTrait([Type::Any].into()))
                            .build(),
                    ),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        assert!(
            toks.iter().any(|t| matches!(t, SigToken::Kw("dyn"))),
            "DynTrait should produce dyn kw"
        );
    }

    #[test]
    fn qualified_path_renders_angle_brackets() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("a"),
                    Kind::Alias(
                        Alias::builder()
                            .target(Type::QualifiedPath {
                                self_ty: Box::new(Type::TypeVar("T".to_owned())),
                                trait_ref: Some(Box::new(Type::Any)),
                                assoc: "Item".to_owned(),
                            })
                            .build(),
                    ),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        let t = text_of(&toks);
        assert!(t.contains('<'), "qualified path should have <");
        assert!(t.contains("Item"), "qualified path should have assoc name");
    }

    #[test]
    fn rich_fixture_all_tokens_non_empty() {
        use nudox_store::source::fixtures::build_rich_view;
        let view = build_rich_view();
        let pkg = PackageView::build(view, Provenance::TrustedLocal);
        for (_intro, entry) in pkg.view().entries() {
            let toks = tokens(entry, &pkg);
            assert_no_empty(&toks, &entry.sym().name);
        }
    }

    #[test]
    fn reference_entry_renders_pub_use() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                Entry::reference(
                    sym("ReExported"),
                    Node::build(None::<nudox_ir::index::RawRef>, []),
                    Ref::Intro(id(1)),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        let t = text_of(&toks);
        assert!(t.contains("pub"), "re-export should have pub");
        assert!(t.contains("use"), "re-export should have use");
    }

    #[test]
    fn generics_render_angle_brackets() {
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(
                    sym("Vec"),
                    Kind::Record(
                        Record::builder()
                            .generics([GenericParam::Type {
                                name: "T".to_owned(),
                                bounds: [].into(),
                                default: None,
                                variance: None,
                            }])
                            .build(),
                    ),
                ),
                Some(id(1)),
            ),
        ]);
        let e = pkg.view().entry(id(2)).unwrap();
        let toks = tokens(e, &pkg);
        let t = text_of(&toks);
        assert!(t.contains('<'), "generics should produce <: {}", t);
        assert!(t.contains('T'), "generic param T should be present: {}", t);
    }
}
