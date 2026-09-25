//! Signature rendering: `nudox_ir::entry::Entry` → `Vec<SigToken>`.
//!
//! # LR-4: one renderer per job, and exactly two jobs
//!
//! This module is the **only** place that converts an `Entry`'s kind data into
//! typed signature tokens. Every surface that shows a signature — the symbol
//! page header, search hit rows, quick-peek, MCP output, and the diff view —
//! uses the `tokens` function below. Rendering a signature by `format!` or any
//! other means outside this module is a review-blocking violation of LR-4.
//!
//! ## The claim this paragraph used to make, and why it was wrong
//!
//! It read: "Rendering a signature by `format!` or any other means outside
//! this module is a review-blocking violation of LR-4" — full stop, with no
//! qualification. Read as "no other code may render a `Type`", that rule was
//! both unenforced and unenforceable, and the codebase had already broken it
//! in the worst possible way: `nudox-graph`'s `Field.typeStr` was
//! `format!("{t:?}")`, shipping `Nominal(Intro(intro:3f1a9c2b…))` to MCP
//! clients. Five further type-valued schema fields were left *unexposed*
//! rather than copy that dump. So the unqualified rule bought no safety and
//! cost five answers.
//!
//! There are now two renderers, deliberately, with disjoint jobs:
//!
//! | | this module (`tokens`) | [`nudox_ir::render`] |
//! |---|---|---|
//! | input | an `Entry` + its `PackageView` | a bare `Type` |
//! | output | `Vec<SigToken>`, each nominal carrying a jump target | one `String` |
//! | reader | a human looking at a signature | a program holding a scalar |
//! | `Unknown(UnresolvedLocalName{"Context"})` | `Context` | `?unresolved(Context)` |
//! | `Unknown(Unannotated)` | `?` | `?unannotated` |
//! | `Unknown(TruncatedAtDepthLimit)` | `…` | `?depth-limit` |
//!
//! **Neither can serve the other's caller**, and the unknown-lattice rows are
//! why. A page renders `Context` because the name is the useful half and the
//! page has a second channel — no link target on that token — to say it is
//! unresolved. A scalar string has no second channel, so its rendering must
//! carry the reason inline and must `?`-prefix it so a hole can never be
//! mistaken for a type the source wrote.
//!
//! What LR-4 forbids, stated so it is checkable: **a third path, and `Debug`
//! in either of the two.** If you need type text somewhere new, call one of
//! these two by name; if neither fits, the correct move is to widen one of
//! them and say here why.
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

use crate::store::package::PackageView;
use nudox_ir::{
    entry::{Entry, Visibility},
    index::Ref,
    kind::Kind,
    kinds::{
        FieldKey, FnModifier, Function, GenericParam, Param, Receiver, RecordForm, Type,
        UnknownType,
        ty::{Primitive, TemplatePart, TupleElement, Variance, Width},
    },
};

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

    let Some(inner) = entry.kind().as_owned_kind() else {
        // A re-export's target is the fact the entry holds. A foreign path
        // (`serde_core::ser::Serialize`) is that target; a local alias keeps
        // the name, which is the only path this entry has.
        let target = match entry.kind() {
            nudox_ir::entry::EntryInner::Reference(Ref::Foreign { key, .. }) => {
                SharedStr::from(source_path(key.path.as_ref()))
            }
            _ => SharedStr::from(name),
        };
        return vec![
            SigToken::Kw("pub"),
            SigToken::Ws,
            SigToken::Kw("use"),
            SigToken::Ws,
            SigToken::Ident(target),
        ];
    };

    // A declaration signature is the compact, type-bearing source context
    // shared by search, MCP records, graph rows, and diffs.  Visibility is
    // part of that declaration for the declaration kinds where Rust-like
    // syntax expresses it; fields already add their own prefix and impls,
    // variants, and parameters do not have a declaration-level visibility
    // prefix.
    let prefix_visibility = matches!(
        &inner,
        Kind::Module(_)
            | Kind::Record(_)
            | Kind::Enum(_)
            | Kind::Trait(_)
            | Kind::Alias(_)
            | Kind::Const(_)
            | Kind::Static(_)
            | Kind::Function(_)
    );

    let rendered = match inner {
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
                for sup in &t.supers {
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
    };

    if prefix_visibility {
        let mut prefixed = Vec::with_capacity(rendered.len() + 2);
        push_visibility(&mut prefixed, entry.sym().visibility);
        prefixed.extend(rendered);
        prefixed
    } else {
        rendered
    }
}

// ---------------------------------------------------------------------------
// Function rendering
// ---------------------------------------------------------------------------

fn render_function(f: &Function, name: &str, package: &PackageView) -> Vec<SigToken> {
    let mut toks = Vec::new();

    // Modifiers in standard order
    for m in &f.modifiers {
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
            format!("\"{abi}\"").as_str(),
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
    for param_ref in &f.input_params {
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
                    if let Some(Kind::Param(p)) = param_entry.kind().as_owned_kind()
                        && let Some(ty) = &p.ty
                    {
                        toks.push(SigToken::Punct(":"));
                        toks.push(SigToken::Ws);
                        push_type(&mut toks, ty, package);
                    }
                } else {
                    toks.push(SigToken::Ident(SharedStr::from("_")));
                }
            }
            // Explicit, not a `_` wildcard. A `Ref<Param>` is same-package by
            // construction, so these are unreachable today — but the wildcard
            // they replace would have rendered a foreign param as a bare `_`,
            // indistinguishable from a missing entry, the day one appeared.
            Ref::Foreign { key, .. } => {
                toks.push(SigToken::Ident(SharedStr::from(key.display.as_ref())));
            }
            Ref::Local(_) => {
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
            for op_ref in &f.output_params {
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
    let rendered = if let Ref::Intro(id) = param_ref
        && let Some(param_entry) = view.entry(*id)
        && let Some(Kind::Param(p)) = param_entry.kind().as_owned_kind()
        && let Some(ty) = &p.ty
    {
        push_type(toks, ty, package);
        true
    } else {
        false
    };
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

        // Each reason renders differently, because each *is* different to a
        // reader. Collapsing them back to one glyph here would undo the whole
        // point of the lattice split: a reader must be able to tell "the
        // source wrote nothing" from "the source wrote `Any`" from "we know
        // the name and have not linked it yet".
        //
        // The two name-carrying reasons render the name itself — an
        // unresolved `Context` is far more useful on screen as `Context` than
        // as `any`, even with no jump target behind it.
        Type::Unknown(reason) => {
            let text = match reason {
                UnknownType::Unannotated | UnknownType::OracleGap => SharedStr::from("?"),
                UnknownType::DynamicallyTyped => SharedStr::from("dynamic"),
                UnknownType::UnresolvedLocalName { name }
                | UnknownType::UnresolvedExternal { name } => SharedStr::from(name.as_str()),
                UnknownType::TruncatedAtDepthLimit => SharedStr::from("…"),
                UnknownType::NoIrRepresentation { construct } => {
                    SharedStr::from(construct.as_str())
                }
            };
            toks.push(SigToken::Ty { text, target: None });
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
            for arg in args {
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
            for elem in elems {
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
                for ty in tys {
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
                for ty in tys {
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
                    format!("\"{abi}\"").as_str(),
                )));
                toks.push(SigToken::Ws);
            }
            toks.push(SigToken::Kw("fn"));
            toks.push(SigToken::Punct("("));
            let mut first = true;
            for p in params {
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
                    text: SharedStr::from(format!("{prefix} extends {bound_text}").as_str()),
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
            let ann_text = annotation.arg.as_ref().map_or_else(
                || format!("@{} ", annotation.token),
                |arg| format!("@{}({}) ", annotation.token, arg),
            );
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
            for part in parts {
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
            for m in members {
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
    for b in bounds {
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
            let text = package.indexes().path_of(*id).map_or_else(
                || SharedStr::from(id.to_hex().as_str()),
                |path| display_path(path.as_ref(), *id, package),
            );
            (text, Some(key))
        }
        // Cross-package reference. The KEY supplies the text, always — never
        // `StableRef`'s `Display`, which is `ecosystem:name#<64 hex>` and would
        // turn `impl ? for Memchr` into `impl cargo:core#8f3a…{64 chars} for
        // Memchr`: a regression in readability from the `?` it replaces.
        //
        // Rendering is therefore a pure function of one package: this function
        // holds a single `PackageView` and no corpus, so the same signature
        // renders the same text regardless of what else happens to be loaded.
        // Only *clickability* varies with the corpus — `target` is `Some` only
        // when a resolver actually placed the reference, so a token is never a
        // hyperlink to nothing.
        Ref::Foreign { key, target } => (SharedStr::from(key.display.as_ref()), target.clone()),
        Ref::Local(_) => {
            // This comment used to say "Should not appear in a sealed table"
            // while 35% of memchr's functions rendered `?`. It is true now:
            // `refer_import` no longer returns a `Ref::Local` into an import
            // arena that `seal` drops, and any residual local is reported in
            // `SealReport::unmapped_local` rather than rendered.
            (SharedStr::from("?"), None)
        }
    }
}

/// Turn a same-package moniker path into the text a reader should see.
///
/// # The defect (docs/LIMITATIONS.md L18, L39)
///
/// `path_of` (`crate::store::package::PackageIndexes::path_of`) is the
/// *physical* ancestor chain: every module `id` is nested under, root first,
/// dot-joined. For `memchr::Memchr` that chain is `memchr.memchr.memchr.Memchr`
/// — the package, its crate-root module (Rust names it after the crate), and
/// its `src/memchr.rs` module are all three named `memchr`, ahead of the
/// struct itself. The chain is correct; showing all three identical segments
/// is not. `workspace/gui`'s `collapse_repeated_crumbs` already fixes this for
/// the breadcrumb trail, which is a `Vec` of clickable crumbs a view can
/// post-process. A signature token's text is not — `resolve_nominal` returns
/// one flattened `SharedStr` that is already load-bearing by the time it
/// reaches `wire::ImplRow::label`, `SigToken::Ty::text`, and every other
/// surface `chunk::signature::tokens` feeds (LR-4). So the collapse has to
/// happen here, the one place a path is rendered into a label, rather than in
/// each of those call sites.
///
/// # What survives
///
/// 1. Adjacent duplicate segments collapse to one — the same rule
///    `collapse_repeated_crumbs` applies to breadcrumbs, applied one layer
///    lower, before the string is ever built. Only *adjacent* repeats
///    collapse; `a.b.a` is a real re-entry into an ancestor and stays, same
///    as the breadcrumb version.
/// 2. If the leaf name (the last surviving segment) is unique among this
///    package's declarations, the rest of the path drops entirely: a
///    signature should read `Memchr`, matching docs.rs, not `memchr.Memchr`,
///    when there is only one `Memchr` in the package to mean.
/// 3. If the leaf collides with another declaration — two distinct types
///    sharing a name in different modules, e.g. `io::Error` and `fmt::Error`
///    (`crate::store::index::name` module docs) — enough of the path survives
///    to tell them apart. The rule for "enough" is the one
///    `PreparedRow::prepare` already uses for colliding search rows
///    (`workspace/gui/src/stores/search_model.rs::common_head_len`): elide
///    only the head every namesake shares, keep the tail that actually
///    differs, and never elide the *last* surviving segment of whichever
///    namesake has the shortest path — that segment is the only thing
///    standing between it and looking unqualified.
///
/// Because the comparison set is symmetric (every namesake's own render goes
/// through this same function against the same set), two colliding leaves
/// never come out textually identical unless their full moniker paths were
/// identical to begin with — at which point no path-based label could have
/// told them apart, docs.rs included.
fn display_path(
    path: &str,
    self_id: nudox_ir::change::IntroId,
    package: &PackageView,
) -> SharedStr {
    let collapsed = collapse_repeated_segments(path);
    let Some((leaf, ancestors)) = collapsed.split_last() else {
        // `str::split('.')` on a non-empty `path` always yields at least one
        // element, so `collapsed` is never empty in practice. Fall back to
        // the raw path rather than panic if a future producer ever manages
        // an empty moniker.
        return SharedStr::from(path);
    };
    let leaf = *leaf;

    let indexes = package.indexes();

    // Every OTHER declaration in this package whose name is exactly `leaf`
    // (case-sensitive). `get_exact` folds case for its bucket lookup — that
    // is right for search, where "vec" and "Vec" should compete for the same
    // slot — but folding case here would make the `memchr` module and the
    // `Memchr` struct "collide" when nothing about the rendered signature
    // actually confuses them.
    let namesake_ancestors: Vec<Vec<&str>> = indexes
        .by_name
        .get_exact(&leaf.to_lowercase())
        .iter()
        .filter(|entry| entry.intro != self_id && entry.display == leaf)
        // A `pub use` re-export is a second *name* for the same item, not a
        // second *type* that happens to share a name. Nearly every real
        // crate re-exports its public surface at the crate root from an
        // internal module — `memchr` does exactly this for `Memchr` itself
        // (`src/lib.rs`: `pub use crate::memchr::{Memchr, …};`). Counting
        // that root-level alias as a namesake would mean bullet 2 above
        // almost never fires on a real package: the struct would never
        // collapse to its bare name, because its own re-export is always
        // sitting right there "colliding" with it.
        .filter(|entry| !is_reexport(package.view(), entry.intro))
        .filter_map(|entry| indexes.path_of(entry.intro))
        .map(|namesake_path| {
            let mut segs = collapse_repeated_segments(namesake_path.as_ref());
            segs.pop(); // drop the namesake's own leaf; keep only its ancestors
            segs
        })
        .collect();

    if namesake_ancestors.is_empty() {
        // Nothing else in the package shares this leaf name: it is already
        // unambiguous on its own, so the ancestor chain adds nothing a
        // reader needs (bullet 2 above).
        return SharedStr::from(leaf);
    }

    // Elide only the ancestor prefix every namesake — and this entry — share,
    // capped so the shallowest member of the group keeps at least one
    // segment of its own. Mirrors `common_head_len` in
    // `workspace/gui/src/stores/search_model.rs` exactly; see that function's
    // doc comment for why the cap exists (a fully-elided member would read as
    // "no path" and become indistinguishable from the unique case above).
    let shortest = namesake_ancestors
        .iter()
        .map(Vec::len)
        .chain(std::iter::once(ancestors.len()))
        .min()
        .unwrap_or(0);
    let ceiling = shortest.saturating_sub(1);

    let mut common = 0;
    while common < ceiling
        && namesake_ancestors
            .iter()
            .all(|other| other[common] == ancestors[common])
    {
        common += 1;
    }

    let kept = &ancestors[common..];
    if kept.is_empty() {
        SharedStr::from(leaf)
    } else {
        SharedStr::from(format!("{}.{leaf}", kept.join(".")))
    }
}

/// `!m` and `!v` are lowering namespace tags, not Rust syntax.
fn source_path(path: &str) -> &str {
    path.strip_suffix("!m")
        .or_else(|| path.strip_suffix("!v"))
        .unwrap_or(path)
}

/// True if `intro` denotes a re-export (`pub use …`) rather than a
/// declaration with its own identity.
///
/// Mirrors the two shapes `chunk::signature::tokens` itself already treats as
/// `pub use <name>` (top of this file): an [`nudox_ir::entry::EntryInner::Reference`]
/// (`entry.kind().as_owned_kind()` is `None`), or an owned
/// [`nudox_ir::kind::Kind::Reexport`]. Either way the entry names an existing
/// item under a new path rather than introducing a new one, so it must not
/// count as a namesake collision in [`display_path`]. An absent entry (should
/// not happen — `intro` came from this same package's `by_name` index) is
/// conservatively treated as "not a re-export", which only means it can still
/// contribute to disambiguation, never that a real collision gets hidden.
fn is_reexport(view: &nudox_ir::view::IrView, intro: nudox_ir::change::IntroId) -> bool {
    view.entry(intro)
        .is_some_and(|entry| matches!(entry.kind().as_owned_kind(), None | Some(Kind::Reexport(_))))
}

/// Collapse adjacent identical segments in a `.`-joined moniker path.
///
/// The one-level-lower sibling of `workspace/gui`'s `collapse_repeated_crumbs`
/// (`workspace/gui/src/views/symbol_page/header.rs`): that function drops a
/// breadcrumb whose label repeats the crumb before it, once the ancestor
/// chain has already been split into clickable pieces. This drops the same
/// kind of repeat one step earlier, on the raw dotted string, before it is
/// ever turned into crumbs or signature text — which is what lets
/// `display_path` fix the *label* rather than relying on every renderer to
/// post-process it.
pub(crate) fn collapse_repeated_segments(path: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('.') {
        if out.last() != Some(&seg) {
            out.push(seg);
        }
    }
    out
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
            SharedStr::from(format!("{prefix}{w}").as_str())
        }
        Primitive::Float(width) => {
            let w = width_str(width);
            SharedStr::from(format!("f{w}").as_str())
        }
        Primitive::MutPointer(inner) => {
            let mut inner_toks = Vec::new();
            push_type(&mut inner_toks, inner, package);
            let inner_text = tokens_to_text(&inner_toks);
            SharedStr::from(format!("*mut {inner_text}").as_str())
        }
        Primitive::ConstPointer(inner) => {
            let mut inner_toks = Vec::new();
            push_type(&mut inner_toks, inner, package);
            let inner_text = tokens_to_text(&inner_toks);
            SharedStr::from(format!("*const {inner_text}").as_str())
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
                .map(|l| format!("{} ", nudox_ir::kinds::lifetime_label(l)))
                .unwrap_or_default();
            let mut_kw = if *mutable { "mut " } else { "" };
            SharedStr::from(format!("&{lt}{mut_kw}{inner_text}").as_str())
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
    for gp in generics {
        if !first {
            toks.push(SigToken::Punct(","));
            toks.push(SigToken::Ws);
        }
        first = false;
        match gp {
            GenericParam::Lifetime { name } => {
                toks.push(SigToken::Lifetime(SharedStr::from(
                    nudox_ir::kinds::lifetime_label(name).as_ref(),
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
    use crate::store::package::{PackageView, Provenance};
    use nudox_ir::{
        apply::PristineIntroTable,
        change::{EcosystemId, IntroId, PackageLineageId, PackageName},
        entry::{Entry, Node, Symbol, Visibility as IrVis},
        index::Ref,
        kind::Kind,
        kinds::{
            Alias, Const, ConstExpr, Field, FieldKey, Function, GenericParam, Module, Record, Type,
            ty::Primitive,
        },
        view::IrView,
    };

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
            assert!(!text.is_empty(), "empty token in {ctx}: {tok:?}");
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
        assert!(t.contains("mod"), "expected mod kw: {t}");
        assert!(t.contains("root"), "expected name: {t}");
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
        assert!(
            t.starts_with("pub struct Point"),
            "visibility must be in signature: {t}"
        );
        assert!(t.contains("struct"), "expected struct: {t}");
        assert!(t.contains("Point"), "expected name: {t}");
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
        assert!(t.contains("union"), "expected union kw: {t}");
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
                            .value(
                                ConstExpr::builder()
                                    .ty(Type::U64)
                                    .source("1024".to_owned())
                                    .build(),
                            )
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
        assert!(t.contains("const"), "expected const kw: {t}");
        assert!(t.contains("1024"), "expected value: {t}");
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
        assert!(t.contains("mut"), "expected mut: {t}");
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
        assert!(
            t.starts_with("pub fn noop"),
            "visibility must be in signature: {t}"
        );
        assert!(t.contains("fn"), "expected fn: {t}");
        assert!(t.contains("noop"), "expected name: {t}");
        assert!(t.contains("()"), "expected parens: {t}");
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
        assert!(t.contains("&self"), "expected &self: {t}");
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
        assert!(t.contains('='), "expected = in alias: {t}");
        assert!(t.contains("str"), "expected target str: {t}");
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
        use crate::store::source::fixtures::build_rich_view;
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
        assert!(
            t.contains("ReExported"),
            "a local re-export keeps its name: {t}"
        );
    }

    #[test]
    fn foreign_reexport_renders_the_target_path() {
        use triomphe::Arc;

        use nudox_ir::change::{EcosystemId, PackageLineageId, PackageName};
        use nudox_ir::foreign::ForeignKey;

        let key = Arc::new(ForeignKey::in_package(
            PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("serde_core")),
            "serde_core::ser::Serialize!m",
            "Serialize",
        ));
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                Entry::reference(
                    sym("Serialize"),
                    Node::build(None::<nudox_ir::index::RawRef>, []),
                    Ref::Foreign { key, target: None },
                ),
                Some(id(1)),
            ),
        ]);
        let rendered = text_of(&tokens(pkg.view().entry(id(2)).unwrap(), &pkg));
        assert!(
            rendered.contains("serde_core::ser::Serialize"),
            "the foreign path is the re-export target: {rendered}"
        );
        assert!(
            !rendered.contains("!m"),
            "a namespace tag is not source syntax: {rendered}"
        );
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
        assert!(t.contains('<'), "generics should produce <: {t}");
        assert!(t.contains('T'), "generic param T should be present: {t}");
    }

    // -----------------------------------------------------------------------
    // display_path (docs/LIMITATIONS.md L18/L39: repeated-ancestor collapse)
    // -----------------------------------------------------------------------

    /// Pull the `text` of the one `SigToken::Ty` in `toks` whose `target` is
    /// `Some` — i.e. the rendering of a same-package `Type::Nominal` ref.
    fn resolved_ty_text(toks: &[SigToken]) -> String {
        toks.iter()
            .find_map(|t| match t {
                SigToken::Ty {
                    text,
                    target: Some(_),
                } => Some(text.to_string()),
                _ => None,
            })
            .expect("field type must resolve to a same-package target")
    }

    #[test]
    fn collapse_repeated_segments_drops_only_adjacent_runs() {
        // The exact shape reported for real memchr (L18/L39): three adjacent
        // `memchr` segments ahead of the leaf collapse to one.
        assert_eq!(
            collapse_repeated_segments("memchr.memchr.memchr.Memchr"),
            vec!["memchr", "Memchr"],
        );
        // A non-adjacent repeat is a real re-entry into an ancestor and must
        // survive — same rule `collapse_repeated_crumbs` applies to the
        // breadcrumb trail (`workspace/gui/src/views/symbol_page/header.rs`).
        assert_eq!(collapse_repeated_segments("a.b.a"), vec!["a", "b", "a"]);
    }

    #[test]
    fn same_package_nominal_ref_collapses_repeated_ancestor_and_drops_unique_leaf() {
        // Reproduces the real memchr ancestor chain: the package's root
        // module, the crate's own root module, and the `mod memchr;`
        // declared inside it are all named `memchr`, three deep, ahead of
        // the `Memchr` struct — `path_of` reports exactly
        // `memchr.memchr.memchr.Memchr` (asserted below as ground truth).
        // Nothing else in the package is named `Memchr`, so the rendered
        // text must collapse all the way to the bare leaf, matching what
        // docs.rs shows.
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("memchr"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(sym("memchr"), Kind::Module(Module)),
                Some(id(1)),
            ),
            (
                id(3),
                raw_entry(sym("memchr"), Kind::Module(Module)),
                Some(id(2)),
            ),
            (
                id(4),
                raw_entry(sym("Memchr"), Kind::Record(Record::builder().build())),
                Some(id(3)),
            ),
            (
                id(5),
                raw_entry(
                    sym("haystack"),
                    Kind::Field(
                        Field::builder()
                            .key(FieldKey::Named)
                            .ty(Type::Nominal(Ref::Intro(id(4))))
                            .build(),
                    ),
                ),
                Some(id(4)),
            ),
        ]);

        assert_eq!(
            pkg.indexes().path_of(id(4)).map(AsRef::as_ref),
            Some("memchr.memchr.memchr.Memchr"),
            "test setup must reproduce the real memchr ancestor chain"
        );

        let e = pkg.view().entry(id(5)).unwrap();
        let ty_text = resolved_ty_text(&tokens(e, &pkg));

        assert_eq!(
            ty_text, "Memchr",
            "a unique leaf behind a repeated ancestor chain must collapse to \
             the bare name — got {ty_text:?}"
        );
    }

    #[test]
    fn same_package_nominal_ref_keeps_disambiguating_tail_when_leaf_collides() {
        // Two distinct `Error` types declared in different modules
        // (`crate::store::index::name` module docs use this exact example)
        // must never render the same label. The precedent this mirrors is
        // `PreparedRow::prepare`'s search-row disambiguation
        // (`workspace/gui/src/stores/search_model.rs`): elide only the
        // ancestor prefix every namesake shares (`root`), and keep the part
        // that actually differs (`io` vs `fmt`).
        let pkg = make_package(vec![
            (id(1), raw_entry(sym("root"), Kind::Module(Module)), None),
            (
                id(2),
                raw_entry(sym("io"), Kind::Module(Module)),
                Some(id(1)),
            ),
            (
                id(3),
                raw_entry(sym("Error"), Kind::Record(Record::builder().build())),
                Some(id(2)),
            ),
            (
                id(4),
                raw_entry(sym("fmt"), Kind::Module(Module)),
                Some(id(1)),
            ),
            (
                id(5),
                raw_entry(sym("Error"), Kind::Record(Record::builder().build())),
                Some(id(4)),
            ),
            (
                id(6),
                raw_entry(
                    sym("io_err"),
                    Kind::Field(
                        Field::builder()
                            .key(FieldKey::Named)
                            .ty(Type::Nominal(Ref::Intro(id(3))))
                            .build(),
                    ),
                ),
                Some(id(3)),
            ),
            (
                id(7),
                raw_entry(
                    sym("fmt_err"),
                    Kind::Field(
                        Field::builder()
                            .key(FieldKey::Named)
                            .ty(Type::Nominal(Ref::Intro(id(5))))
                            .build(),
                    ),
                ),
                Some(id(5)),
            ),
        ]);

        let io_error = resolved_ty_text(&tokens(pkg.view().entry(id(6)).unwrap(), &pkg));
        let fmt_error = resolved_ty_text(&tokens(pkg.view().entry(id(7)).unwrap(), &pkg));

        assert_ne!(
            io_error, fmt_error,
            "two distinct types both named `Error` must never render the \
             same label — got {io_error:?} for both"
        );
        assert_eq!(io_error, "io.Error");
        assert_eq!(fmt_error, "fmt.Error");
    }
}
