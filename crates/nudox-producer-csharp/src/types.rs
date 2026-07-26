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
//! * Unresolved `Error` nodes → `Type::Any` (the oracle already logged the gap).
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

use nudox_ir::build::{GenericParam, Primitive, Type, Width, WherePred};

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
pub fn lower_type(t: &TypeSig) -> Type {
    lower_type_depth(t, 0)
}

fn lower_type_depth(t: &TypeSig, depth: usize) -> Type {
    if depth > MAX_DEPTH {
        return Type::Any;
    }

    match t {
        TypeSig::Named { name, args, owner, nullable, .. } => {
            let base = lower_named(name, args, owner.as_deref(), depth);
            apply_nullable(base, nullable)
        }

        TypeSig::TypeParam { name, nullable, .. } => {
            // A type-parameter reference becomes a `Nominal` pointing at the
            // named GenericParam entry.  Because Lowering requires a `Ref<T>`,
            // and we do not have a `Ref<GenericParam>` at type-lowering time
            // (GenericParams are embedded in the parent kind body, not
            // separate entries), we use `Type::Any` and note this as a known
            // gap: **the IR has no `Type::TypeVar(name)` primitive**.
            //
            // UNCERTAINTY: there is no `Type::GenericParam(name)` variant in
            // the new IR's `ty.rs`.  The old IR had `IrType::GenericParam`.
            // For now, fallback to `Any`; a proper fix requires either adding
            // `Type::TypeVar` or threading a mapping from type-param name →
            // Ref<GenericParam>.
            let _ = nullable;
            let _ = name;
            Type::Any
        }

        // SZ array → `Slice`; multidimensional → outer `Slice` of inner
        // (rank-N is not directly representable; we nest Slice N times which
        // loses the rectangular shape).  UNCERTAINTY: no `Type::Array { rank }`
        // exists in the new IR; `Type::Array` has a `length: usize` for
        // fixed-size, not rank. We use `Slice` for all ranks.
        TypeSig::Array { element, rank: _, nullable } => {
            let elem = lower_type_depth(element, depth + 1);
            let base = Type::Slice(Box::new(elem));
            apply_nullable(base, nullable)
        }

        // Unmanaged pointer.
        TypeSig::Pointer { pointee } => {
            let inner = lower_type_depth(pointee, depth + 1);
            Type::Primitive(Primitive::MutPointer(Box::new(inner)))
        }

        // Function pointer (`delegate*<...>`): calling convention is dropped
        // (no slot in the new IR).
        TypeSig::FuncPtr { params, return_type, .. } => {
            // UNCERTAINTY: the new IR has no `FunctionPointer` type variant.
            // The old IR had `IrType::FunctionPointer`. We cannot faithfully
            // represent this; return Any and note the gap.
            let _ = params;
            let _ = return_type;
            Type::Any
        }

        // Value tuples: labelled or unlabelled — both become `Type::Tuple`.
        // Named elements lose their labels (no `NamedTuple` in the new IR).
        TypeSig::Tuple { elements, nullable } => {
            let inner: Box<[Type]> = elements
                .iter()
                .map(|e| lower_type_depth(&e.ty, depth + 1))
                .collect();
            let base = Type::Tuple(inner);
            apply_nullable(base, nullable)
        }

        TypeSig::Dynamic {} => Type::Any,

        // `Nullable<T>` (value type): represent as `Union([T, Never])` to
        // match the annotated-nullable reference treatment.
        TypeSig::NullableValue { inner } => {
            let t = lower_type_depth(inner, depth + 1);
            Type::Union(Box::new([t, Type::Never]))
        }

        // Unresolvable type (missing dependency).
        TypeSig::Error { .. } => Type::Any,
    }
}

/// Lower a `Named` node: primitive special-cases first, then generic
/// application, then a plain `Type::Any` for unresolvable names.
fn lower_named(name: &str, args: &[TypeSig], owner: Option<&TypeSig>, depth: usize) -> Type {
    if let Some(prim) = lower_primitive(name) {
        return prim;
    }

    // `Outer<T>.Inner`: we cannot represent qualified-path member projection
    // without `Type::QualifiedPath`; fall back to Any for the owner, then
    // return Any for the whole thing.
    // UNCERTAINTY: the new IR has no `QualifiedPath` in ty.rs.
    if owner.is_some() {
        return Type::Any;
    }

    if args.is_empty() {
        // Bare named type reference.  We cannot produce a `Ref<Record>` here
        // because we may not have declared the target yet (the oracle flat list
        // means we process types in declaration order, not dependency order).
        // UNCERTAINTY: `Type::Nominal(RawRef)` requires a `RawRef`; producing
        // one requires calling `Lowering::refer`, which is only available from
        // the lowering pass, not from a standalone type-lowering helper.
        // We return `Type::Any` for all named references as a conservative gap.
        // The correct fix is to pass `&mut Lowering<DocId>` into every type
        // helper and call `lowering.refer(doc_id_from_name(name))` there;
        // that is a pervasive refactor but not architecturally blocked.
        Type::Any
    } else {
        // Generic application: `List<T>` etc. Base is also Any for now.
        let type_args: Box<[Type]> =
            args.iter().map(|a| lower_type_depth(a, depth + 1)).collect();
        Type::Apply { base: Box::new(Type::Any), args: type_args }
    }
}

/// The primitive mapping from C# metadata name to nudox-ir `Type::Primitive`.
fn lower_primitive(name: &str) -> Option<Type> {
    let prim = match name {
        "System.Boolean" => Type::Primitive(Primitive::Bool),
        "System.SByte" => Type::Primitive(Primitive::Integer { signed: true, width: Width::W8 }),
        "System.Int16" => Type::Primitive(Primitive::Integer { signed: true, width: Width::W16 }),
        "System.Int32" => Type::Primitive(Primitive::Integer { signed: true, width: Width::W32 }),
        "System.Int64" => Type::Primitive(Primitive::Integer { signed: true, width: Width::W64 }),
        "System.Int128" => Type::Primitive(Primitive::Integer { signed: true, width: Width::W128 }),
        "System.Byte" => Type::Primitive(Primitive::Integer { signed: false, width: Width::W8 }),
        "System.UInt16" => Type::Primitive(Primitive::Integer { signed: false, width: Width::W16 }),
        "System.UInt32" => Type::Primitive(Primitive::Integer { signed: false, width: Width::W32 }),
        "System.UInt64" => Type::Primitive(Primitive::Integer { signed: false, width: Width::W64 }),
        "System.UInt128" => {
            Type::Primitive(Primitive::Integer { signed: false, width: Width::W128 })
        }
        // Native ints.
        "nint" | "System.IntPtr" => {
            Type::Primitive(Primitive::Integer { signed: true, width: Width::Arch })
        }
        "nuint" | "System.UIntPtr" => {
            Type::Primitive(Primitive::Integer { signed: false, width: Width::Arch })
        }
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
        // Falls through to return None, lowered as a named type (→ Any).
        _ => return None,
    };
    Some(prim)
}

/// Apply 3-state nullability.
///
/// | oracle token  | IR form                        |
/// |---------------|--------------------------------|
/// | `annotated`   | `Union([T, Never])`  (`T?`)    |
/// | `notAnnotated`| bare `T`                       |
/// | oblivious     | bare `T`                       |
///
/// We cannot encode `notAnnotated` vs oblivious distinctly in the current IR
/// (`Type` has no `Annotated` / `Oblivious` wrapper). Both become bare `T`.
/// A future `Type::Annotated(Box<Type>)` variant would let us round-trip.
fn apply_nullable(base: Type, nullable: &str) -> Type {
    match Nullability::parse(nullable) {
        Nullability::Annotated => Type::Union(Box::new([base, Type::Never])),
        Nullability::NotAnnotated | Nullability::Oblivious => base,
    }
}

// ---------------------------------------------------------------------------
// Declaration-site generic parameters and where-predicates
// ---------------------------------------------------------------------------

/// Lower a declaration-site type-parameter list into IR [`GenericParam`]s and
/// [`WherePred`]s.
///
/// C# variance is declaration-site (`in`/`out` on interfaces & delegates), but
/// the new IR's `GenericParam::Type` has no variance field — variance is
/// dropped here.  Special constraints (`class`, `struct`, `new()`, `notnull`,
/// `unmanaged`, `allows ref struct`) have no structural slot; they ride as
/// synthetic `WherePred` bounds with `target = Type::Any` and a `Builtin` name
/// prefixed `csharp:` so they are never confused with real interface FQNs.
pub fn lower_type_params(
    type_params: &[schema::TypeParam],
) -> (Vec<GenericParam>, Vec<WherePred>) {
    let mut params = Vec::with_capacity(type_params.len());
    let mut wheres = Vec::new();

    for tp in type_params {
        params.push(GenericParam::Type {
            name: tp.name.clone(),
            bounds: Box::new([]),   // explicit-type bounds go into `wheres`
            default: None,
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
        // bounds.  Since named references currently lower to `Any`, these are
        // low-fidelity but not lost.
        for bound_sig in &c.types {
            let bound_ty = lower_type(bound_sig);
            let target = Type::Primitive(Primitive::Builtin(tp.name.clone()));
            wheres.push(WherePred { target, bounds: Box::new([bound_ty]) });
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
        TypeSig::FuncPtr { params, return_type, .. } => {
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
