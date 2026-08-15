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
    change::EcosystemId,
    entry::AttrTok,
    foreign::ForeignKey,
    index::{RawRef, Ref},
    kinds::{Record, ty::Variance},
    lower::Lowering,
};

use crate::csharp::schema::{self, Nullability, TypeSig};

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
        // Protected OR internal → Protected (the wider of the two).
        "protected" | "protectedInternal" => Visibility::Protected,
        "internal" => Visibility::Internal,
        // Protected AND internal → Package (closest narrower).
        "privateProtected" => Visibility::Package,
        // Private, unknown, or empty → Private (safe default: member-level
        // default in C#).
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
pub fn lower_type(
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
    // Our own guard, not Roslyn's: a real type exists below here and raising
    // MAX_DEPTH is all that is needed to reach it.
    if depth > MAX_DEPTH {
        return Type::TRUNCATED;
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

        // `dynamic` is the gradual-typing escape hatch, not the top type.
        // `object` and `dynamic` have the *same* CLR representation but
        // opposite static contracts: `object` forbids every member access
        // until you cast, `dynamic` permits every member access and defers to
        // runtime binding. Collapsing them onto one opcode — which is what
        // `Type::Any` did — erased the single fact a caller most needs.
        TypeSig::Dynamic {} => Type::DYNAMIC,

        // `Nullable<T>` (value type): represent as `Union([T, Never])` to
        // match the annotated-nullable reference treatment.
        TypeSig::NullableValue { inner } => {
            let t = lower_type_depth(inner, name_to_doc_id, out, depth + 1);
            Type::Union(Box::new([t, Type::Never]))
        }

        // Roslyn produced an `IErrorTypeSymbol`: it has the name and could not
        // bind it, which in practice means an assembly reference the
        // extraction never loaded. Cross-package input is what closes this, so
        // it is `UnresolvedExternal` — and the name is kept, because two
        // unresolvable types in one overload set must not share an encoding.
        TypeSig::Error { name } => Type::unresolved_external(name.as_str()),
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

    // `Outer<T>.Inner` — nested generic type member projection.
    //
    // C# emits this for inner types of generic outer types, e.g.
    // `System.Collections.Generic.Dictionary<K,V>.KeyCollection`.
    // The IR now has `Type::QualifiedPath { self_ty, trait_ref: None, assoc }`:
    // - `self_ty`: the outer type (lowered from `owner`)
    // - `trait_ref`: `None` — C# dot-qualified paths have no `as Trait` disambiguation
    // - `assoc`: the simple name of the inner type (arity stripped)
    if let Some(owner_sig) = owner {
        let self_ty = lower_type_depth(owner_sig, name_to_doc_id, out, depth + 1);
        let assoc = simple_name(name); // strips arity backticks, takes last segment
        return Type::QualifiedPath {
            self_ty: Box::new(self_ty),
            trait_ref: None,
            assoc,
        };
    }

    // Resolve the FQN to a doc-id from within the current extraction; anything
    // else is a cross-package target and is *named* rather than erased.
    let raw_ref: RawRef = match name_to_doc_id.get(name) {
        Some(doc_id) => {
            let r: Ref<Record> = out.refer(doc_id.clone());
            r.into_raw()
        }
        // Cross-package reference: no doc-id in this extraction.
        //
        // This used to be `Type::Any`, under a comment saying resolving it
        // "would need `Lowering::refer_import` with a `UniqueId` from a
        // registry". That is no longer true: `refer_import` takes a
        // `ForeignKey`, which a producer builds from what it already holds, and
        // needs no registry handle. Erasing here was also actively harmful —
        // `Skeleton` encodes `Type::Any` as one byte, so two overloads whose
        // parameters differ only in cross-package types produced byte-identical
        // signature skeletons and collapsed onto one `IntroId`.
        None => out
            .refer_import::<Record>(csharp_foreign_key(name))
            .into_raw(),
    };

    if args.is_empty() {
        // Bare named type reference.
        Type::Nominal(raw_ref)
    } else {
        // Generic application: `List<T>`, `IEnumerable<string>`, etc.
        let base = Type::Nominal(raw_ref);
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

/// The cross-package key for a C# type outside the current extraction.
///
/// # What C# can and cannot state
///
/// The metadata fully-qualified name — arity backticks and all, e.g.
/// `System.Collections.Generic.List\`1` — is globally stable and identifies the
/// type exactly, so it is the join key. What is **not** derivable is the NuGet
/// package id: it is not in general the assembly name (the
/// `Microsoft.Extensions.*` assemblies split across several packages), and
/// `Extraction.assembly` describes only the assembly being extracted, never the
/// one a referenced type came from.
///
/// So this emits [`ForeignOrigin::Namespace`] over the type's namespace rather
/// than fabricating a `nuget:` lineage. A guessed lineage would render as a
/// working hyperlink to a symbol that does not exist, which is strictly worse
/// than an honest un-linked name.
///
/// [`ForeignOrigin::Namespace`]: nudox_ir::foreign::ForeignOrigin::Namespace
fn csharp_foreign_key(fqn: &str) -> ForeignKey {
    let bare = strip_arity(fqn);
    let namespace = bare.rfind('.').map_or("", |i| &bare[..i]);
    ForeignKey::in_namespace(
        EcosystemId::new("nuget"),
        namespace,
        fqn,
        // The display name drops the namespace and the arity marker: a reader
        // wants `List`, not ``System.Collections.Generic.List`1``.
        simple_name(fqn),
    )
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
        // `object` — C#'s genuine top type. Every value converts to it and
        // nothing narrows out of it without a cast. Distinct from `dynamic`
        // (see `TypeSig::Dynamic` above), which is now `Unknown`.
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
            while chars.peek().is_some_and(char::is_ascii_digit) {
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use nudox_ir::{entry::Symbol, entry::Visibility, package::PackageId};
    use std::path::PathBuf;

    fn make_sink() -> Lowering<String> {
        let sym = Symbol {
            name: "(root)".to_owned(),
            visibility: Visibility::Public,
            documentation: String::new(),
            source: PathBuf::new(),
            span: 0..0,
            aliases: Box::new([]),
            deprecation: None,
            doc_links: Box::new([]),
            attrs: Box::new([]),
            cfg: None,
        };
        Lowering::new(PackageId::path("test"), sym)
    }

    // ── CC-2: `object` is the top type; `dynamic` and `Error` are not ────────

    /// `dynamic` and `object` share a CLR representation and have opposite
    /// static contracts. They must not share an IR encoding.
    ///
    /// `object` forbids every member access until you cast; `dynamic` permits
    /// every member access and defers to runtime binding. Under `Type::Any`
    /// the two were byte-identical.
    #[test]
    fn dynamic_and_object_are_different_types() {
        use nudox_ir::kinds::UnknownType;
        let mut out = make_sink();
        let names = HashMap::new();

        let dynamic = lower_type(&TypeSig::Dynamic {}, &names, &mut out);
        assert_eq!(dynamic, Type::Unknown(UnknownType::DynamicallyTyped));

        let object = lower_type(
            &TypeSig::Named {
                name: "System.Object".to_owned(),
                args: vec![],
                owner: None,
                nullable: String::new(),
                type_kind: String::new(),
            },
            &names,
            &mut out,
        );
        assert_eq!(object, Type::Any, "`object` is C#'s genuine top type");
        assert_ne!(dynamic, object, "`dynamic` is not `object`");
    }

    /// A Roslyn `IErrorTypeSymbol` becomes `UnresolvedExternal` and keeps its
    /// name, so two unbindable types in one overload set stay distinct.
    #[test]
    fn error_type_keeps_its_name() {
        use nudox_ir::kinds::UnknownType;
        let mut out = make_sink();
        let names = HashMap::new();
        let a = lower_type(
            &TypeSig::Error {
                name: "Foo.Bar".to_owned(),
            },
            &names,
            &mut out,
        );
        assert_eq!(
            a,
            Type::Unknown(UnknownType::UnresolvedExternal {
                name: "Foo.Bar".to_owned()
            })
        );
        let b = lower_type(
            &TypeSig::Error {
                name: "Foo.Baz".to_owned(),
            },
            &names,
            &mut out,
        );
        assert_ne!(a, b, "two unbindable types must not collapse together");
        assert_ne!(a, Type::Any, "an unbindable type is not `object`");
    }

    /// `Outer<T>.Inner` must lower to `Type::QualifiedPath`, not `Type::Any`.
    ///
    /// C# nested types of generic outer types (e.g.
    /// `Dictionary<K,V>.KeyCollection`) carry an `owner` in the oracle. That
    /// A type outside the extraction names itself instead of erasing to `Any`.
    ///
    /// `Type::Any` here was the root cause of a whole class of identity loss:
    /// `Skeleton` encodes it as a single byte, so two overloads differing only
    /// in cross-package parameter types produced byte-identical signature
    /// skeletons and collapsed onto one `IntroId`. Asserting on the rendered
    /// display name rather than on "not Any" is deliberate — a `Ref::Foreign`
    /// carrying an empty or wrong label would satisfy the weaker check and
    /// still render a useless row.
    #[test]
    fn a_type_outside_the_extraction_names_itself_rather_than_erasing() {
        let mut sink = make_sink();
        // Deliberately empty: nothing is in this extraction, so every named
        // type below is cross-package.
        let names = HashMap::new();

        let sig = TypeSig::Named {
            name: "System.IO.Stream".to_owned(),
            args: vec![],
            owner: None,
            nullable: String::new(),
            type_kind: "Class".to_owned(),
        };

        let Type::Nominal(raw) = lower_type(&sig, &names, &mut sink) else {
            panic!("a cross-package named type must lower to a nominal reference");
        };
        let (key, target) = raw
            .as_foreign()
            .expect("a type outside the extraction must be a cross-package reference");
        assert_eq!(
            key.display.as_ref(),
            "Stream",
            "the display name is what renders when the ref is not linked"
        );
        assert_eq!(
            key.path.as_ref(),
            "System.IO.Stream",
            "the metadata FQN is the join key a resolver matches on"
        );
        assert!(
            target.is_none(),
            "nothing was sealed alongside this package, so it must be named but unlinked"
        );

        // Arity backticks belong to the join key, never to the label.
        let generic = TypeSig::Named {
            name: "System.Collections.Generic.List`1".to_owned(),
            args: vec![TypeSig::Named {
                name: "System.String".to_owned(),
                args: vec![],
                owner: None,
                nullable: String::new(),
                type_kind: "Class".to_owned(),
            }],
            owner: None,
            nullable: String::new(),
            type_kind: "Class".to_owned(),
        };
        let Type::Apply { base, args } = lower_type(&generic, &names, &mut sink) else {
            panic!("a generic application must stay an application");
        };
        let Type::Nominal(base_raw) = base.as_ref() else {
            panic!("the base of `List<string>` must be nominal, not erased");
        };
        let (base_key, _) = base_raw
            .as_foreign()
            .expect("the generic base is cross-package");
        assert_eq!(
            base_key.display.as_ref(),
            "List",
            "`List`1` must render as `List`, not with its arity marker"
        );
        assert_eq!(args.len(), 1, "the type argument must survive");

        // Two distinct cross-package types must stay distinguishable — that is
        // exactly what `Type::Any` destroyed.
        assert_ne!(
            key.path, base_key.path,
            "distinct cross-package types must carry distinct keys"
        );
    }

    /// owner used to be discarded and the whole type degraded to `Type::Any`.
    /// `Type::QualifiedPath { self_ty, trait_ref: None, assoc }` now preserves
    /// both the outer type and the inner member name. `trait_ref` is `None`
    /// because C# dot-qualified paths have no `as Trait` disambiguation —
    /// that slot exists for Rust's `<T as Trait>::Assoc`.
    #[test]
    fn nested_generic_inner_lowers_to_qualified_path_not_any() {
        let mut sink = make_sink();
        let names = HashMap::new();

        // `System.Collections.Generic.Dictionary`2.KeyCollection`
        let sig = TypeSig::Named {
            name: "System.Collections.Generic.Dictionary`2.KeyCollection".to_owned(),
            args: vec![],
            owner: Some(Box::new(TypeSig::Named {
                name: "System.Collections.Generic.Dictionary`2".to_owned(),
                args: vec![],
                owner: None,
                nullable: String::new(),
                type_kind: "Class".to_owned(),
            })),
            nullable: String::new(),
            type_kind: "Class".to_owned(),
        };

        let lowered = lower_type(&sig, &names, &mut sink);
        match &lowered {
            Type::QualifiedPath {
                trait_ref, assoc, ..
            } => {
                assert!(
                    trait_ref.is_none(),
                    "C# dot-qualified paths must carry trait_ref: None"
                );
                assert_eq!(
                    assoc, "KeyCollection",
                    "assoc must be the inner type's simple name (arity stripped)"
                );
            }
            other => panic!("Outer<T>.Inner must lower to QualifiedPath, got {other:?}"),
        }
        assert!(
            !matches!(lowered, Type::Any),
            "Outer<T>.Inner must NOT degrade to Type::Any"
        );
    }

    /// `simple_name` strips metadata arity backticks and takes the last segment.
    #[test]
    fn simple_name_strips_arity_and_namespace() {
        assert_eq!(
            simple_name("System.Collections.Generic.Dictionary`2"),
            "Dictionary"
        );
        assert_eq!(simple_name("Foo.Bar"), "Bar");
    }
}
