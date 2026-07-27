//! Lower the oracle's structural [`schema::TypeSig`] tree into nudox-ir's
//! [`Type`] algebra, and map C# accessibility + modifiers to IR vocabulary.
//!
//! # Track A decisions
//!
//! * `System.Decimal` has no `Primitive` variant — lowered to `Type::Nominal`
//!   via a forward-refer (Track B item 1 would add `Primitive::Decimal`).
//! * Nullability: 3-state NRT is preserved via `Type::Union([T, Never])` for
//!   annotated-nullable (`T?`) and left bare for `notAnnotated`.  Oblivious
//!   drops the annotation entirely (no noise for old assemblies).  See below.
//! * `dynamic` → `Type::Any`.
//! * `System.Void` → `Type::Tuple([])` (unit).
//! * Unresolved `Error` nodes → `Type::Any` (the oracle already logged the
//!   gap).
//!
//! # Nullability representation decision
//!
//! The new IR has no `TypeOperator`; it does not carry arbitrary unary
//! operators.  The available options for `T?` (annotated nullable reference):
//! - `Type::Union([lower(T), Type::Never])` — semantically accurate, mirrors
//!   what TypeScript `T | null` maps to.
//! - Drop nullability entirely (lose information).
//!
//! We chose `Union([T, Never])` for annotated-nullable, no wrapper for
//! `notAnnotated`, and no wrapper for oblivious.  This is the most information-
//! preserving choice available in the current IR.  Downstream consumers can
//! pattern-match `Union` to recover the T.

use std::collections::HashMap;

use nudox_ir::{
    build::{GenericParam, Primitive, TupleElement, Type, WherePred, Width},
    entry::AttrTok,
    index::{RawRef, Ref},
    kinds::{ty::Variance, Record},
    lower::Lowering,
};

use crate::schema::{self, Nullability, TypeSig};

/// Defensive recursion bound, mirroring the oracle's own depth guard.
const MAX_DEPTH: usize = 64;

/// Map a Roslyn accessibility token onto the IR visibility vocabulary.
///
/// # Protected internal / private protected decision
///
/// C# has two compound visibilities the IR has no exact match for:
///
/// | C# token            | IR mapping    | Rationale                                  |
/// |---------------------|---------------|--------------------------------------------|
/// | `protectedInternal` | `Protected`   | OR semantics — widens to the broader one   |
/// | `privateProtected`  | `Package`     | AND semantics — closest available narrower |
///
/// The oracle token is **always** stamped verbatim into `Symbol.documentation`
/// as a `Declared: \`…\`` note so the original pair stays recoverable.
pub fn map_visibility(s: &str) -> nudox_ir::entry::Visibility {
    use nudox_ir::entry::Visibility;
    match s {
        "public" => Visibility::Public,
        "protected" => Visibility::Protected,
        "internal" => Visibility::Internal,
        // Protected OR internal → Protected (the wider of the two).
        "protectedInternal" => Visibility::Protected,
        // Protected AND internal → Package (closest narrower).
        "privateProtected" => Visibility::Package,
        "private" => Visibility::Private,
        // Unknown / empty → Private (safe default: member-level default in C#).
        _ => Visibility::Private,
    }
}

/// Lower an oracle type signature into the IR type algebra.
///
/// `name_to_doc_id` maps a metadata FQN (with arity backticks, e.g.
/// `"System.Collections.Generic.List\`1"`) to the Roslyn doc-id
/// (`"T:System.Collections.Generic.List\`1"`).  Only types present in the
/// *current* extraction are in this map; cross-package references remain as
/// `Any` (the Lowering's cross-package import API is not yet wired here).
///
/// `out` is borrowed mutably so `refer` can be called for nominal types.
/// `refer` is idempotent and order-independent, so forward references are free.
pub fn lower_type<'a>(
    t: &TypeSig,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) -> Type {
    lower_type_depth(t, name_to_doc_id, out, 0)
}

fn lower_type_depth(
    t: &TypeSig,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
    depth: usize,
) -> Type {
    if depth > MAX_DEPTH {
        return Type::Any;
    }

    match t {
        TypeSig::Named {
            name,
            args,
            owner,
            nullable,
            ..
        } => {
            let base = lower_named(name, args, owner.as_deref(), name_to_doc_id, out, depth);
            apply_nullable(base, nullable)
        }

        TypeSig::TypeParam { name, nullable, .. } => {
            // `Type::TypeVar(String)` is the correct representation for a
            // type-parameter use — e.g. the `T` in `List<T>`. It carries the
            // name verbatim and is distinct from a declaration (`GenericParam`).
            let base = Type::TypeVar(name.clone());
            apply_nullable(base, nullable)
        }

        // SZ array → `Slice`; multidimensional → outer `Slice` of inner
        // (rank-N is not directly representable; we nest Slice N times which
        // loses the rectangular shape).
        // KNOWN GAP: no `Type::Array { rank }` exists in the new IR;
        // `Type::Array` has a `length: usize` for fixed-size, not rank.
        // We use `Slice` for all ranks. A future `Type::Array { rank }` variant
        // would let us represent `T[,]` as `Array { ty: T, rank: 2 }`.
        TypeSig::Array {
            element,
            rank: _,
            nullable,
        } => {
            let elem = lower_type_depth(element, name_to_doc_id, out, depth + 1);
            let base = Type::Slice(Box::new(elem));
            apply_nullable(base, nullable)
        }

        // Unmanaged pointer.
        TypeSig::Pointer { pointee } => {
            let inner = lower_type_depth(pointee, name_to_doc_id, out, depth + 1);
            Type::Primitive(Primitive::MutPointer(Box::new(inner)))
        }

        // Function pointer (`delegate*<int, string>` / `delegate* unmanaged[Cdecl]<...>`).
        //
        // C# `delegate*<P1, ..., Pn, Ret>` is a structural callable type, not a
        // delegate class — it has no implicit boxing and its calling convention is
        // part of its identity.  Previously lowered to `Type::Any`, which silently
        // dropped all parameter and return type information.
        //
        // Calling-convention mapping:
        //   - Managed (default):  `call_conv` is empty / "managed"  → `abi = None`.
        //   - Unmanaged default:  `call_conv = "unmanaged"` + no modifiers → `abi = Some("unmanaged")`.
        //   - Unmanaged specific: `unmanaged_call_convs = ["Cdecl"]` etc.
        //     → `abi = Some("Cdecl")` (first modifier; multiple convs are joined with ",").
        TypeSig::FuncPtr {
            params,
            return_type,
            call_conv,
            unmanaged_call_convs,
        } => {
            let ir_params: Box<[Type]> = params
                .iter()
                .map(|p| lower_type_depth(p, name_to_doc_id, out, depth + 1))
                .collect();

            let ir_ret: Option<Box<Type>> = return_type.as_deref().and_then(|r| {
                let t = lower_type_depth(r, name_to_doc_id, out, depth + 1);
                // `void` return → None (no return type).
                match &t {
                    Type::Tuple(elems) if elems.is_empty() => None,
                    _ => Some(Box::new(t)),
                }
            });

            // Derive ABI string from the oracle's calling-convention fields.
            let abi: Option<String> = if !unmanaged_call_convs.is_empty() {
                // Specific unmanaged convention(s): "Cdecl", "StdCall", etc.
                // Multiple modifiers are legal C# syntax; join them.
                Some(unmanaged_call_convs.join(","))
            } else if call_conv == "unmanaged" {
                // Unmanaged with no specific modifier: platform default unmanaged ABI.
                Some("unmanaged".to_string())
            } else {
                // Managed (default ABI) or empty/unknown — no annotation needed.
                None
            };

            Type::FunctionPointer {
                params: ir_params,
                ret: ir_ret,
                abi,
            }
        }

        // Value tuples: `(int, string)` (positional) or `(int start, int end)` (named).
        //
        // C# oracle emits `TupleElement { name: Option<String>, ty: TypeSig }`.
        // When `name` is `Some(label)` (non-empty), the element is a *named* tuple
        // element and its label is part of the API surface — e.g. the two overloads
        // `M((int x, int y))` and `M((int start, int end))` differ in their labels.
        // Previously all labels were dropped; now `TupleElement::Named` preserves them.
        TypeSig::Tuple { elements, nullable } => {
            let inner: Box<[TupleElement]> = elements
                .iter()
                .map(|e| {
                    let ty = lower_type_depth(&e.ty, name_to_doc_id, out, depth + 1);
                    match &e.name {
                        Some(label) if !label.is_empty() => TupleElement::Named {
                            label: label.clone(),
                            ty,
                        },
                        _ => TupleElement::Positional(ty),
                    }
                })
                .collect();
            let base = Type::Tuple(inner);
            apply_nullable(base, nullable)
        }

        TypeSig::Dynamic {} => Type::Any,

        // `Nullable<T>` (value type): represent as `Union([T, Never])` to
        // match the annotated-nullable reference treatment.
        TypeSig::NullableValue { inner } => {
            let t = lower_type_depth(inner, name_to_doc_id, out, depth + 1);
            Type::Union(Box::new([t, Type::Never]))
        }

        // Unresolvable type (missing dependency).
        TypeSig::Error { .. } => Type::Any,
    }
}

/// Lower a `Named` node: primitive special-cases first, then generic
/// application or bare nominal reference.
fn lower_named(
    name: &str,
    args: &[TypeSig],
    owner: Option<&TypeSig>,
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
    depth: usize,
) -> Type {
    if let Some(prim) = lower_primitive(name) {
        return prim;
    }

    // `Outer<T>.Inner`: we cannot represent qualified-path member projection
    // without `Type::QualifiedPath`.
    // KNOWN GAP: the new IR has no `QualifiedPath` variant in ty.rs.
    // A future `Type::QualifiedPath { base: Box<Type>, member: String }`
    // would allow `Outer<T>.Inner` to be faithfully represented.
    if owner.is_some() {
        return Type::Any;
    }

    // Attempt to resolve the FQN to a doc-id from within the current extraction.
    // Cross-package named types (e.g. `System.IO.Stream` when not in the
    // extraction) stay `Any`.
    let raw_ref: Option<RawRef> = name_to_doc_id
        .get(name)
        .map(|doc_id| -> RawRef {
            let r: Ref<Record> = out.refer(doc_id.clone());
            r.into_raw()
        });

    if args.is_empty() {
        // Bare named type reference.
        match raw_ref {
            Some(r) => Type::Nominal(r),
            // Cross-package reference: no doc-id in this extraction.
            // KNOWN GAP: to resolve cross-package nominal types we would need
            // `Lowering::refer_import` with a `UniqueId` from a registry;
            // that requires the caller to supply a package-registry handle.
            None => Type::Any,
        }
    } else {
        // Generic application: `List<T>`, `IEnumerable<string>`, etc.
        // The base is Nominal if known, else Any (cross-package).
        let base = match raw_ref {
            Some(r) => Type::Nominal(r),
            None => Type::Any,
        };
        let type_args: Box<[Type]> = args
            .iter()
            .map(|a| lower_type_depth(a, name_to_doc_id, out, depth + 1))
            .collect();
        Type::Apply {
            base: Box::new(base),
            args: type_args,
        }
    }
}

/// The primitive mapping from C# metadata name to nudox-ir `Type::Primitive`.
fn lower_primitive(name: &str) -> Option<Type> {
    let prim = match name {
        "System.Boolean" => Type::Primitive(Primitive::Bool),
        "System.SByte" => Type::Primitive(Primitive::Integer {
            signed: true,
            width: Width::W8,
        }),
        "System.Int16" => Type::Primitive(Primitive::Integer {
            signed: true,
            width: Width::W16,
        }),
        "System.Int32" => Type::Primitive(Primitive::Integer {
            signed: true,
            width: Width::W32,
        }),
        "System.Int64" => Type::Primitive(Primitive::Integer {
            signed: true,
            width: Width::W64,
        }),
        "System.Int128" => Type::Primitive(Primitive::Integer {
            signed: true,
            width: Width::W128,
        }),
        "System.Byte" => Type::Primitive(Primitive::Integer {
            signed: false,
            width: Width::W8,
        }),
        "System.UInt16" => Type::Primitive(Primitive::Integer {
            signed: false,
            width: Width::W16,
        }),
        "System.UInt32" => Type::Primitive(Primitive::Integer {
            signed: false,
            width: Width::W32,
        }),
        "System.UInt64" => Type::Primitive(Primitive::Integer {
            signed: false,
            width: Width::W64,
        }),
        "System.UInt128" => Type::Primitive(Primitive::Integer {
            signed: false,
            width: Width::W128,
        }),
        // Native ints.
        "nint" | "System.IntPtr" => Type::Primitive(Primitive::Integer {
            signed: true,
            width: Width::Arch,
        }),
        "nuint" | "System.UIntPtr" => Type::Primitive(Primitive::Integer {
            signed: false,
            width: Width::Arch,
        }),
        "System.Half" => Type::Primitive(Primitive::Float(Width::W16)),
        "System.Single" => Type::Primitive(Primitive::Float(Width::W32)),
        "System.Double" => Type::Primitive(Primitive::Float(Width::W64)),
        "System.Char" => Type::Primitive(Primitive::Char),
        // `string` is the primitive string type in C# (unlike heap-allocated String in Rust).
        "System.String" => Type::Primitive(Primitive::Str),
        // `object` — the top type.
        "System.Object" => Type::Any,
        // `void` — the unit type.
        "System.Void" => Type::Tuple(Box::new([])),
        // `System.Decimal` — Track B item 1: no dedicated primitive slot yet.
        // Falls through to return None, resolved as Nominal if in extraction.
        _ => return None,
    };
    Some(prim)
}

/// Apply 3-state nullability.
///
/// | oracle token  | IR form                                                  |
/// |---------------|----------------------------------------------------------|
/// | `annotated`   | `Union([T, Never])`  (`T?` — may be null)                |
/// | `notAnnotated`| `Annotated { inner: T, annotation: "notAnnotated" }`     |
/// | oblivious     | bare `T` (no NRT context; old assemblies)                |
///
/// **Why distinguish `notAnnotated` from oblivious?**  In a nullable-enabled
/// context `string` (notAnnotated) explicitly asserts non-null — the author
/// said "this parameter is never null".  Oblivious `string` (old assembly or
/// disabled NRT) makes no such assertion.  Collapsing both to bare `T` drops
/// that guarantee and makes it impossible to surface "this type was declared
/// non-null" in the index, which is the single most-requested nullability fact.
///
/// `Type::Annotated` with `annotation.token = "notAnnotated"` encodes this
/// without inventing a new type variant; downstream consumers can strip the
/// wrapper to recover `T` or inspect the annotation to determine nullability
/// status.
fn apply_nullable(base: Type, nullable: &str) -> Type {
    match Nullability::parse(nullable) {
        Nullability::Annotated => Type::Union(Box::new([base, Type::Never])),
        Nullability::NotAnnotated => Type::Annotated {
            inner: Box::new(base),
            annotation: AttrTok {
                token: "notAnnotated".to_string(),
                arg: None,
            },
        },
        Nullability::Oblivious => base,
    }
}

// ---------------------------------------------------------------------------
// Declaration-site generic parameters and where-predicates
// ---------------------------------------------------------------------------

/// Lower a declaration-site type-parameter list into IR [`GenericParam`]s and
/// [`WherePred`]s.
///
/// # Variance
///
/// C# has declaration-site variance (`in`/`out` on interfaces & delegates),
/// which is real API surface — `IEnumerable<out T>` is covariant and that
/// affects assignment compatibility.  The oracle emits `"in"` / `"out"` /
/// `"none"` on every type parameter.  These are now wired to
/// `GenericParam::Type::variance`:
///
/// | Oracle token | IR value                    |
/// |--------------|-----------------------------|
/// | `"out"`      | `Some(Variance::Covariant)` |
/// | `"in"`       | `Some(Variance::Contravariant)` |
/// | `"none"` / _ | `None` (unspecified)        |
///
/// `None` means "no annotation in source" — the case for most type params on
/// classes and structs (where variance is neither declared nor meaningful).
///
/// # Special constraints
///
/// `class`, `struct`, `new()`, `notnull`, `unmanaged`, `allows ref struct`
/// have no structural slot; they ride as synthetic `WherePred` bounds with
/// `target = Type::Any` and a `Builtin` name prefixed `csharp:` so they are
/// never confused with real interface FQNs.
pub fn lower_type_params(
    type_params: &[schema::TypeParam],
    name_to_doc_id: &HashMap<String, String>,
    out: &mut Lowering<String>,
) -> (Vec<GenericParam>, Vec<WherePred>) {
    let mut params = Vec::with_capacity(type_params.len());
    let mut wheres = Vec::new();

    for tp in type_params {
        // Wire declaration-site variance from the oracle token.
        // `"out"` = covariant (C# output position, widens safely).
        // `"in"`  = contravariant (C# input position, narrows safely).
        // `"none"` or anything else = no annotation (invariant by default).
        let variance: Option<Variance> = match tp.variance.as_str() {
            "out" => Some(Variance::Covariant),
            "in" => Some(Variance::Contravariant),
            _ => None,
        };

        params.push(GenericParam::Type {
            name: tp.name.clone(),
            bounds: Box::new([]), // explicit-type bounds go into `wheres`
            default: None,
            variance,
        });

        let c = &tp.constraints;
        if c.reference_type {
            wheres.push(synthetic_where(&tp.name, "csharp:class"));
        }
        if c.value_type {
            wheres.push(synthetic_where(&tp.name, "csharp:struct"));
        }
        if c.not_null {
            wheres.push(synthetic_where(&tp.name, "csharp:notnull"));
        }
        if c.unmanaged {
            wheres.push(synthetic_where(&tp.name, "csharp:unmanaged"));
        }
        if c.allows_ref_like {
            wheres.push(synthetic_where(&tp.name, "csharp:allows ref struct"));
        }
        if c.constructor {
            wheres.push(synthetic_where(&tp.name, "csharp:new()"));
        }
        // Explicit type constraints (`where T : Base`) — lowered as WherePred
        // bounds with nominal types resolved via the name map.
        for bound_sig in &c.types {
            let bound_ty = lower_type(bound_sig, name_to_doc_id, out);
            let target = Type::Primitive(Primitive::Builtin(tp.name.clone()));
            wheres.push(WherePred {
                target,
                bounds: Box::new([bound_ty]),
            });
        }
    }

    (params, wheres)
}

/// A synthetic `WherePred` for a special C# constraint keyword.
fn synthetic_where(param_name: &str, keyword: &str) -> WherePred {
    WherePred {
        target: Type::Primitive(Primitive::Builtin(param_name.to_string())),
        bounds: Box::new([Type::Primitive(Primitive::Builtin(keyword.to_string()))]),
    }
}

// ---------------------------------------------------------------------------
// Attribute rendering (for documentation notes — no structural slot)
// ---------------------------------------------------------------------------

/// Strip metadata arity backticks (`List\`1` → `List`).
pub fn strip_arity(name: &str) -> String {
    if !name.contains('`') {
        return name.to_string();
    }
    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '`' {
            while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The trailing segment of a dotted qualified name (arity stripped).
pub fn simple_name(qualified: &str) -> String {
    let bare = strip_arity(qualified);
    bare.rsplit('.').next().unwrap_or(&bare).to_string()
}

/// Render an attribute use to its source-like form: `[Foo("x")]`.
pub fn render_attribute(a: &schema::Attr) -> String {
    let short = simple_name(&a.ty);
    // Attribute-name convention: `FooAttribute` is written `[Foo]`.
    let name = short.strip_suffix("Attribute").unwrap_or(&short);
    let mut args: Vec<String> = a.args.clone();
    args.extend(a.named.iter().map(|(k, v)| format!("{k} = {v}")));
    if args.is_empty() {
        format!("[{name}]")
    } else {
        format!("[{name}({})]", args.join(", "))
    }
}

/// Render a type signature to compact C# display text (for doc notes).
pub fn type_display(t: &TypeSig) -> String {
    match t {
        TypeSig::Named { name, args, .. } => {
            let base = simple_name(name);
            if args.is_empty() {
                base
            } else {
                let a: Vec<String> = args.iter().map(type_display).collect();
                format!("{}<{}>", base, a.join(", "))
            }
        }
        TypeSig::TypeParam { name, .. } => name.clone(),
        TypeSig::Array { element, rank, .. } => {
            let commas = ",".repeat((*rank as usize).saturating_sub(1));
            format!("{}[{commas}]", type_display(element))
        }
        TypeSig::Pointer { pointee } => format!("{}*", type_display(pointee)),
        TypeSig::FuncPtr {
            params,
            return_type,
            ..
        } => {
            let mut parts: Vec<String> = params.iter().map(type_display).collect();
            if let Some(ret) = return_type {
                parts.push(type_display(ret));
            }
            format!("delegate*<{}>", parts.join(", "))
        }
        TypeSig::Tuple { elements, .. } => {
            let parts: Vec<String> = elements.iter().map(|e| type_display(&e.ty)).collect();
            format!("({})", parts.join(", "))
        }
        TypeSig::Dynamic {} => "dynamic".to_string(),
        TypeSig::NullableValue { inner } => format!("{}?", type_display(inner)),
        TypeSig::Error { name } => simple_name(name),
    }
}
