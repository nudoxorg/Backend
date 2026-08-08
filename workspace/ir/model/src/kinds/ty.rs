use std::num::{NonZero, NonZeroU16};

use crate::{List, entry::AttrTok, index::RawRef, visitor::Visitor};

#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Type {
    /// A receiver/self type such as Rust `Self` or TypeScript `this`.
    SelfType,

    /// A fundamental, language-level built-in type.
    /// Ex: `i32`, `f64`, `bool`.
    Primitive(Primitive),

    /// A fixed-length, heterogeneous collection of types, with optional
    /// per-element labels.
    ///
    /// Ex: `(i32, String)` (positional), `(int start, int end)` in C#,
    /// `[start: number, end: number]` in TypeScript.
    ///
    /// An empty list `()` represents the unit type. C# value tuples and
    /// TypeScript tuple labels are carried losslessly as [`TupleElement::Named`]
    /// entries; unnamed positions use [`TupleElement::Positional`].
    Tuple(List<TupleElement>),

    /// A dynamically-sized view into a contiguous sequence.
    /// Ex: `[u8]` or `[]T`.
    Slice(Box<Type>),

    /// A fixed-size contiguous sequence.
    /// Ex: `[i32; 4]` or `std::array<int, 4>`.
    Array { ty: Box<Type>, length: usize },

    /// An untagged union or sum of types.
    /// Ex: `string | number`.
    Union(List<Type>),

    /// An intersection or combination of types.
    /// Ex: `Serializable & Cloneable`.
    Intersection(List<Type>),

    /// Represents a type that cannot exist (Bottom Type).
    /// Ex: `!` in Rust, `never` in TypeScript, `NoReturn` in Python.
    Never,

    /// Represents the "All" type (Top Type).
    /// Ex: `any` or `unknown` in TypeScript, `Object` in Java.
    Any,

    /// A **nominal** reference to a declared type (record/enum/trait/alias).
    /// Ex: `Bar`, `std::string::String`, `java.util.List`.
    ///
    /// Untyped ([`RawRef`]) because a nominal type may name any type-like kind.
    /// Like every other reference it is `Local` while building and lowered to
    /// `Intro`/`Foreign` by [`seal`](crate::package), so it is
    /// content-addressed in the sealed table.
    Nominal(RawRef),

    /// A **generic application** of a base type to arguments.
    /// Ex: `Bar<u32>`, `Vec<T>`, `HashMap<K, V>`, `List<String>`.
    ///
    /// `base` is normally a [`Type::Nominal`]; `args` are the applied types.
    /// This is what distinguishes `Foo for Bar<u32>` from `Foo for
    /// Bar<String>`.
    Apply { base: Box<Type>, args: List<Type> },

    /// A reference to a **generic parameter**, by name.
    /// Ex: the `T` in `fn id<T>(x: T) -> T`.
    ///
    /// This is a *use* of a type parameter, distinct from its *declaration* in
    /// [`GenericParam`](crate::kinds::GenericParam). Without it, every producer
    /// has to lower `T` to [`Type::Any`], which loses the link between a
    /// signature and the parameter list that binds it — Go, C# and Java all
    /// independently hit this.
    ///
    /// The name is carried verbatim. Note this makes a bare `TypeVar`
    /// *not* alpha-equivalent; identity skeletons deliberately exclude generic
    /// parameter names, so [`skeleton`](crate::skeleton) encodes only the
    /// opcode and not the name.
    TypeVar(String),

    /// A use-site wildcard with optional bound.
    /// Ex: Java `?`, `? extends T`, `? super T`; TypeScript's `unknown` in
    /// variance position; Kotlin's `out`/`in` projections.
    ///
    /// `bound` is `None` for an unbounded wildcard (`?`).
    Wildcard {
        variance: Variance,
        bound: Option<Box<Type>>,
    },

    /// A **callable type** expressed as a structural signature: parameter types,
    /// return type, and an optional calling-convention/ABI string.
    ///
    /// Requested by Go, Rust, C# and C/C++ (4 of 7 producers). Each had
    /// independently lowered anonymous callable types to `Type::Any`:
    ///
    /// - C: `void (*)(int, bool)` — C function pointers.
    /// - Rust: `fn(i32) -> bool` — bare fn types.
    /// - C#: `delegate*<int, bool>` managed delegates and
    ///   `delegate* unmanaged[Cdecl]<int, bool>` unmanaged delegates.
    /// - Go: `func(int) bool` — anonymous function types.
    ///
    /// **ABI slot rationale:** C# `delegate* unmanaged[Cdecl]<>` is real API
    /// surface — two delegates that differ only in calling convention have
    /// distinct ABI contracts and must not unify. The slot is `Option<String>`
    /// and is `None` for managed/default-ABI callable types, matching the
    /// parallel treatment of `Function::abi`. Carrying it here (rather than
    /// hiding it in an attribute) keeps the type structurally comparable and
    /// makes the ABI visible to Trustfall queries without payload inspection.
    FunctionPointer {
        // Types of the positional parameters, in order.
        params: List<Type>,
        // Return type. None for void/unit or absent return annotation.
        ret: Option<Box<Type>>,
        // Calling convention / ABI string, if explicitly specified.
        // Examples: "Cdecl", "StdCall", "C". None = language default.
        abi: Option<String>,
    },

    /// A type with attached metadata annotation.
    ///
    /// Requested by Java, C# and Python (3 of 7 producers). Each language
    /// independently needed to attach source-level annotation metadata to a type
    /// without losing the underlying type identity:
    ///
    /// - Java: `@NonNull String` — nullability and other annotations on types.
    /// - C#: nullable-reference types — `string?` vs `string` (oblivious) vs
    ///   `string` (not-annotated). Without this variant, `notAnnotated` and
    ///   `oblivious` are indistinguishable, a real information loss.
    /// - Python: `Annotated[T, meta]` — PEP 593 metadata attached to a type.
    ///
    /// **Payload choice:** `AttrTok` (the same type used by `Symbol::attrs`) is
    /// reused here rather than a bare `String`. This keeps the schema consistent
    /// and lets downstream consumers handle type annotations with the same
    /// tooling they already use for symbol attributes. `token` carries the
    /// annotation name; `arg` carries its argument if any.
    Annotated {
        // The underlying type being annotated.
        inner: Box<Type>,
        // The annotation, using the same AttrTok type as Symbol::attrs.
        annotation: AttrTok,
    },

    // ─── TypeScript-specific structural types ───────────────────────────────
    /// A TypeScript **conditional type**: `Check extends Extends ? Then : Else`.
    ///
    /// TypeScript conditional types let types branch on assignability:
    /// `T extends string ? A : B` resolves to `A` when `T` is assignable to
    /// `string`, and `B` otherwise. The four arms are all load-bearing:
    /// `check` is the type under test, `extends_ty` is the bound, `then_ty`
    /// is taken when the condition holds, `else_ty` when it fails.
    ///
    /// Previously emitted as `Type::Unsupported` / `Type::Any` by the
    /// TypeScript producer, erasing the entire conditional structure.
    Conditional {
        check: Box<Type>,
        extends_ty: Box<Type>,
        then_ty: Box<Type>,
        else_ty: Box<Type>,
    },

    /// A TypeScript **mapped type**: `{ readonly [P in keyof T]?: T[P] }`.
    ///
    /// Mapped types iterate over the keys of a source type and produce a new
    /// object type. `key_var` is the bound variable name (alpha-equivalent —
    /// excluded from the skeleton); `source` is the type being iterated;
    /// `value` is the body type per property; `readonly` and `optional` are
    /// tri-state modifiers. A bare `bool` cannot represent the three TS states
    /// (`+`/`-`/absent) — see [`MappedModifier`].
    ///
    /// **Identity decision:** `readonly` and `optional` are identity-relevant.
    /// `{ readonly [P in K]: V }` and `{ [P in K]: V }` have different
    /// assignability rules. `key_var` is alpha-equivalent and excluded from the
    /// skeleton, like `TypeVar` names and `GenericParam` names.
    ///
    /// Previously emitted as `Type::Unsupported` / `Type::Any` by the
    /// TypeScript producer.
    Mapped {
        key_var: String,
        source: Box<Type>,
        value: Box<Type>,
        readonly: MappedModifier,
        optional: MappedModifier,
    },

    /// A TypeScript **template literal type**: `` `prefix-${T}` ``.
    ///
    /// Alternates fixed string spans with interpolated type positions via
    /// [`TemplatePart`]. The literal text IS identity-relevant: `` `a-${T}` ``
    /// and `` `b-${T}` `` are distinct types.
    ///
    /// Previously emitted as `Type::Unsupported` / `Type::Any` by the
    /// TypeScript producer.
    TemplateLiteral(List<TemplatePart>),

    // ─── Anonymous structural types (Go + TypeScript) ───────────────────────
    /// An **anonymous structural record** type: Go's `struct { X int }` or a
    /// TypeScript object-literal type `{ x: number; y?: string }`.
    ///
    /// Named records (declared with a name) are `Type::Nominal`. This variant
    /// covers inline structural types with no nominal name. Fields carry name,
    /// type, `optional`, and `readonly` — both Go and TypeScript expose these
    /// at the type level. For Go anonymous interfaces (method sets), `form` is
    /// `AnonRecordForm::Interface` and each method's `ty` is a
    /// `Type::FunctionPointer`. This unifies struct and interface anonymous
    /// types under one variant rather than duplicating encoding infrastructure.
    ///
    /// Previously emitted as `Type::Any` by Go and TypeScript producers.
    AnonymousRecord {
        form: AnonRecordForm,
        members: List<AnonField>,
    },

    // ─── Rust-specific types ────────────────────────────────────────────────
    /// Rust **`impl Trait`** in argument or return position.
    ///
    /// An opaque existential type: the compiler selects a concrete type
    /// statically; the call site only sees the bound set. Distinct from
    /// `dyn Trait` (see [`Type::DynTrait`]): `impl Trait` is zero-cost static
    /// dispatch; `dyn Trait` is a fat-pointer with dynamic dispatch. Collapsing
    /// them into one variant with a flag would obscure this fundamental
    /// semantic difference.
    ///
    /// Previously emitted as `Type::Any` by the Rust producer.
    ImplTrait(List<Type>),

    /// Rust **`dyn Trait`** (dynamic dispatch, fat-pointer object).
    ///
    /// A fat pointer with a vtable, requiring object safety. Previously
    /// squeezed into `Type::Intersection`, which loses the distinction between
    /// a structural intersection (`A & B`) and a trait-object type
    /// (`dyn A + B`). Separate from [`Type::ImplTrait`] — see that variant for
    /// the full rationale.
    DynTrait(List<Type>),

    /// A **placeholder type** the producer could not resolve.
    ///
    /// Rust's `_` wildcard, clang's `__auto_type`, or any type the producer
    /// encountered but could not lower. Distinct from [`Type::Any`]:
    ///
    /// - `Any` — the type is genuinely dynamic/unconstrained at the language level.
    /// - `Inferred` — a specific type exists but the producer could not determine it.
    ///
    /// Conflating them makes it impossible to distinguish a real dynamic type
    /// from a producer resolution gap.
    ///
    /// **Note on foreign-ref fallbacks:** cross-package references falling back
    /// to `Type::Any` are NOT a type-system gap and do not belong here.
    /// `Ref::Foreign(StableRef)` already exists for that case; the fallback is
    /// an acquisition-boundary problem (producers have no registry handle),
    /// not a missing type variant.
    Inferred,

    /// A **qualified path** expression: `<T as Trait>::Assoc` (Rust) or
    /// `Outer<T>.Inner` (C#).
    ///
    /// `self_ty` is the base type; `trait_ref` is the disambiguation trait
    /// (`None` for C#-style dot-qualified paths); `assoc` is the associated
    /// item name.
    ///
    /// Previously emitted as `Type::Any` by both the Rust and C# producers.
    QualifiedPath {
        self_ty: Box<Type>,
        trait_ref: Option<Box<Type>>,
        assoc: String,
    },
}

// ─── Supporting types for the new variants ──────────────────────────────────

/// A tri-state modifier on a mapped type's `readonly` or `optional` position.
///
/// TypeScript distinguishes three states:
/// - `Add` (`readonly` / `+readonly` / `?` / `+?`) — explicitly added.
/// - `Remove` (`-readonly` / `-?`) — explicitly removed from the source type.
/// - `Absent` — not mentioned; inherited from the source type.
///
/// A bare `bool` cannot represent this: `Add` and `Absent` would collapse
/// into the same value, losing either the removal or the inheritance case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum MappedModifier {
    /// Modifier explicitly added (`readonly` / `+readonly` / `?` / `+?`).
    Add,
    /// Modifier explicitly removed (`-readonly` / `-?`).
    Remove,
    /// Modifier absent — inherited from the source type.
    Absent,
}

/// A single part of a [`Type::TemplateLiteral`].
///
/// Template literal types alternate fixed string spans with interpolated
/// type positions: `` `error-${Code}: ${Message}` `` is
/// `Literal("error-") | Interpolated(Code) | Literal(": ") | Interpolated(Message)`.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum TemplatePart {
    /// A fixed string literal span (e.g. `"error-"`).
    Literal(String),
    /// An interpolated type position (e.g. `Code` or `number`).
    Interpolated(Box<Type>),
}

/// The form of an [`Type::AnonymousRecord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum AnonRecordForm {
    /// An anonymous struct (`struct { X int }` in Go; `{ x: number }` in
    /// TypeScript).
    Struct,
    /// An anonymous interface / method-set type (`interface { Foo() bool }` in
    /// Go).
    Interface,
}

/// A member of an [`Type::AnonymousRecord`].
///
/// Used for both struct fields (Go / TypeScript) and interface methods (Go).
/// For methods the `ty` field carries a [`Type::FunctionPointer`].
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct AnonField {
    /// The member name.
    pub name: String,
    /// The member type.
    pub ty: Type,
    /// Whether the member is optional (`foo?: T` in TypeScript). Always
    /// `false` for Go struct fields and interface methods.
    pub optional: bool,
    /// Whether the member is read-only (`readonly foo: T` in TypeScript).
    /// Always `false` for Go struct fields and interface methods.
    pub readonly: bool,
}

/// A single element inside a [`Type::Tuple`], optionally labelled.
///
/// TypeScript and C# both carry per-element labels at the source level:
///
/// - TypeScript: `[start: number, end: number]` — named tuple elements.
/// - C#: `(int start, int end)` — value tuple element names.
///
/// Both languages had previously dropped the label at the IR boundary because
/// `Type::Tuple` was `List<Type>`. TypeScript already preserved the labels
/// internally and discarded them only during lowering; this variant lets it
/// emit them faithfully.
///
/// **Shape choice:** An enum over `Positional(Type)` and `Named { label, ty }`
/// is preferred over `(Option<String>, Type)` because it is self-documenting,
/// makes the presence/absence of a label explicit at the type level rather than
/// as an `Option` that might be interpreted multiple ways, and gives pattern
/// matching clear arms. The tuple form would be slightly more compact but less
/// readable.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum TupleElement {
    /// An unlabelled element, as in Rust `(i32, String)`.
    Positional(Type),

    /// A labelled element, as in C# `(int start, int end)` or TypeScript
    /// `[start: number, end: number]`.
    Named {
        // The element label as it appears in source.
        label: String,
        // The element type.
        ty: Type,
    },
}

/// Use-site variance for a [`Type::Wildcard`] or declaration-site variance for
/// a [`crate::kinds::GenericParam`] type parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Variance {
    /// Unbounded, or bound in neither direction (`?`); or invariant as in most
    /// Rust and Java type parameters.
    Invariant,
    /// Bounded above (`? extends T`, `out T`); or declaration-site covariance
    /// as in C# `out T` on a generic interface or delegate parameter.
    Covariant,
    /// Bounded below (`? super T`, `in T`); or declaration-site contravariance
    /// as in C# `in T` on a generic interface or delegate parameter.
    Contravariant,
}

impl Type {
    pub const U8: Self = Type::Primitive(Primitive::Integer {
        signed: false,
        width: Width::W8,
    });

    pub const U16: Self = Type::Primitive(Primitive::Integer {
        signed: false,
        width: Width::W16,
    });

    pub const U32: Self = Type::Primitive(Primitive::Integer {
        signed: false,
        width: Width::W32,
    });

    pub const U64: Self = Type::Primitive(Primitive::Integer {
        signed: false,
        width: Width::W64,
    });

    pub const U128: Self = Type::Primitive(Primitive::Integer {
        signed: false,
        width: Width::W128,
    });

    pub const I8: Self = Type::Primitive(Primitive::Integer {
        signed: true,
        width: Width::W8,
    });

    pub const I16: Self = Type::Primitive(Primitive::Integer {
        signed: true,
        width: Width::W16,
    });

    pub const I32: Self = Type::Primitive(Primitive::Integer {
        signed: true,
        width: Width::W32,
    });

    pub const I64: Self = Type::Primitive(Primitive::Integer {
        signed: true,
        width: Width::W64,
    });

    pub const I128: Self = Type::Primitive(Primitive::Integer {
        signed: true,
        width: Width::W128,
    });
}

/// A language-level primitive type, independent of any target architecture.
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Primitive {
    Integer {
        signed: bool,
        width: Width,
    },

    Float(Width),

    Bool,

    Char,

    /// Type of a string literal (if it exists)
    ///
    /// Note that this is specifically for primitive types, so this should be
    /// equivalent to `str` in Rust or `string` in C#, not Rust's `String` or
    /// C++'s `std::string`.
    // TODO: should rust string literals resolve to BorrowedRef then?
    // TODO: should C/C++ string literals resolve to `char*` or `char[]` instead?
    Str,

    /// A raw, mutable, unmanaged pointer.
    /// Ex: `*mut T`, `int*`.
    MutPointer(Box<Type>),

    /// A raw, const, unmanaged pointer.
    /// Ex: `*const T`, `int *const`.
    ConstPointer(Box<Type>),

    /// A managed reference with optional lifetime/mutability tracking.
    /// Ex: `&'a mut T`.
    ///
    /// `lifetime` carries the producer's spelling and may or may not include
    /// the `'` sigil; render it through
    /// [`lifetime_label`](crate::kinds::lifetime_label) rather than prepending
    /// one, or a producer that stores the source spelling renders `&''a T`.
    Reference {
        lifetime: Option<String>,
        mutable: bool,
        ty: Box<Type>,
    },

    /// An arbitrary primtive type, e.g. Date in JavaScript/TypeScript
    Builtin(String),
}

/// A language-level primitive type, independent of any target architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Width {
    Fixed(NonZeroU16),

    /// Machine-dependent / pointer-sized (e.g., `usize`, `isize`).
    /// Generally not applicable to Floats
    Arch,
}

impl Width {
    /// 8-bit width
    pub const W8: Self = Width::Fixed(NonZero::new(8).unwrap());

    /// 16-bit width
    pub const W16: Self = Width::Fixed(NonZero::new(16).unwrap());

    /// 32-bit width
    pub const W32: Self = Width::Fixed(NonZero::new(32).unwrap());

    /// 64-bit width
    pub const W64: Self = Width::Fixed(NonZero::new(64).unwrap());

    /// 80-bit width
    pub const W80: Self = Width::Fixed(NonZero::new(80).unwrap());

    /// 128-bit width
    pub const W128: Self = Width::Fixed(NonZero::new(128).unwrap());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{entry::AttrTok, skeleton::function_signature_skeleton};

    // Helper: get a skeleton bytes vec for a single type, by wrapping it as
    // the sole input parameter type of a function signature.  This exercises
    // the same `Skeleton::ty` path without calling the private method directly.
    fn ty_skeleton(t: &Type) -> Vec<u8> {
        function_signature_skeleton(&[Some(t.clone())], &[], &[], &[])
    }

    // ── Feature 1: Type::FunctionPointer ─────────────────────────────────────

    /// Construction and serde round-trip for FunctionPointer.
    #[test]
    fn function_pointer_roundtrip() {
        let fp = Type::FunctionPointer {
            params: [Type::I32, Type::Any].into(),
            ret: Some(Box::new(Type::Primitive(Primitive::Bool))),
            abi: Some("Cdecl".to_owned()),
        };
        let json = serde_json::to_string(&fp).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(fp, back);
    }

    /// FunctionPointer without ABI and with None return round-trips.
    #[test]
    fn function_pointer_no_abi_no_ret_roundtrip() {
        let fp = Type::FunctionPointer {
            params: [].into(),
            ret: None,
            abi: None,
        };
        let json = serde_json::to_string(&fp).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(fp, back);
    }

    /// Two FunctionPointers differing only in ABI must have distinct skeletons.
    #[test]
    fn function_pointer_abi_changes_skeleton() {
        let managed = Type::FunctionPointer {
            params: [Type::I32].into(),
            ret: None,
            abi: None,
        };
        let cdecl = Type::FunctionPointer {
            params: [Type::I32].into(),
            ret: None,
            abi: Some("Cdecl".to_owned()),
        };
        assert_ne!(
            ty_skeleton(&managed),
            ty_skeleton(&cdecl),
            "delegate*<> and delegate* unmanaged[Cdecl]<> must not have the same skeleton"
        );
    }

    /// Two FunctionPointers differing only in param type have distinct skeletons.
    #[test]
    fn function_pointer_param_changes_skeleton() {
        let f_i32 = Type::FunctionPointer {
            params: [Type::I32].into(),
            ret: None,
            abi: None,
        };
        let f_i64 = Type::FunctionPointer {
            params: [Type::I64].into(),
            ret: None,
            abi: None,
        };
        assert_ne!(ty_skeleton(&f_i32), ty_skeleton(&f_i64));
    }

    // ── Feature 3: Variance on GenericParam::Type ─────────────────────────────

    // (Variance itself lives in generics.rs tests; the Type::Wildcard path is
    // already exercised. Here we just confirm the field exists and round-trips.)

    // ── Feature 4: Type::Annotated ───────────────────────────────────────────

    /// Construction and serde round-trip for Type::Annotated.
    #[test]
    fn annotated_roundtrip() {
        let ann = Type::Annotated {
            inner: Box::new(Type::Primitive(Primitive::Str)),
            annotation: AttrTok {
                token: "NonNull".to_owned(),
                arg: None,
            },
        };
        let json = serde_json::to_string(&ann).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ann, back);
    }

    /// Annotated with an arg round-trips.
    #[test]
    fn annotated_with_arg_roundtrip() {
        let ann = Type::Annotated {
            inner: Box::new(Type::Any),
            annotation: AttrTok {
                token: "Nullable".to_owned(),
                arg: Some("ALWAYS".to_owned()),
            },
        };
        let json = serde_json::to_string(&ann).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ann, back);
    }

    /// @NonNull String and String (unannotated) must have different skeletons.
    #[test]
    fn annotated_vs_unannotated_differ_in_skeleton() {
        let raw = Type::Primitive(Primitive::Str);
        let annotated = Type::Annotated {
            inner: Box::new(Type::Primitive(Primitive::Str)),
            annotation: AttrTok {
                token: "NonNull".to_owned(),
                arg: None,
            },
        };
        assert_ne!(
            ty_skeleton(&raw),
            ty_skeleton(&annotated),
            "@NonNull String and String must have different identity skeletons"
        );
    }

    /// Two annotations differing only in token name must have different skeletons.
    #[test]
    fn different_annotation_tokens_differ_in_skeleton() {
        let non_null = Type::Annotated {
            inner: Box::new(Type::Any),
            annotation: AttrTok {
                token: "NonNull".to_owned(),
                arg: None,
            },
        };
        let nullable = Type::Annotated {
            inner: Box::new(Type::Any),
            annotation: AttrTok {
                token: "Nullable".to_owned(),
                arg: None,
            },
        };
        assert_ne!(ty_skeleton(&non_null), ty_skeleton(&nullable));
    }

    // ── Feature 5: TupleElement / Type::Tuple ────────────────────────────────

    /// A positional tuple round-trips.
    #[test]
    fn tuple_positional_roundtrip() {
        let t = Type::Tuple(
            [
                TupleElement::Positional(Type::I32),
                TupleElement::Positional(Type::Any),
            ]
            .into(),
        );
        let json = serde_json::to_string(&t).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }

    /// A named-element tuple round-trips.
    #[test]
    fn tuple_named_roundtrip() {
        let t = Type::Tuple(
            [
                TupleElement::Named {
                    label: "start".to_owned(),
                    ty: Type::I32,
                },
                TupleElement::Named {
                    label: "end".to_owned(),
                    ty: Type::I32,
                },
            ]
            .into(),
        );
        let json = serde_json::to_string(&t).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(t, back);
    }

    /// A tuple with labels and one without must have DIFFERENT skeletons.
    ///
    /// `(int start, int end)` and `(int, int)` are distinct C# types — the
    /// labelled form supports `.start` / `.end` member access.
    #[test]
    fn named_vs_positional_tuple_differ_in_skeleton() {
        let positional = Type::Tuple(
            [
                TupleElement::Positional(Type::I32),
                TupleElement::Positional(Type::I32),
            ]
            .into(),
        );
        let named = Type::Tuple(
            [
                TupleElement::Named {
                    label: "start".to_owned(),
                    ty: Type::I32,
                },
                TupleElement::Named {
                    label: "end".to_owned(),
                    ty: Type::I32,
                },
            ]
            .into(),
        );
        assert_ne!(
            ty_skeleton(&positional),
            ty_skeleton(&named),
            "(int, int) and (int start, int end) must have different identity skeletons"
        );
    }

    /// Two tuples with different labels must have different skeletons.
    #[test]
    fn different_labels_differ_in_skeleton() {
        let ab = Type::Tuple(
            [
                TupleElement::Named {
                    label: "a".to_owned(),
                    ty: Type::I32,
                },
                TupleElement::Named {
                    label: "b".to_owned(),
                    ty: Type::I32,
                },
            ]
            .into(),
        );
        let xy = Type::Tuple(
            [
                TupleElement::Named {
                    label: "x".to_owned(),
                    ty: Type::I32,
                },
                TupleElement::Named {
                    label: "y".to_owned(),
                    ty: Type::I32,
                },
            ]
            .into(),
        );
        assert_ne!(ty_skeleton(&ab), ty_skeleton(&xy));
    }

    // ── Type::Conditional ─────────────────────────────────────────────────────

    #[test]
    fn conditional_roundtrip() {
        let c = Type::Conditional {
            check: Box::new(Type::TypeVar("T".to_owned())),
            extends_ty: Box::new(Type::Primitive(Primitive::Str)),
            then_ty: Box::new(Type::Primitive(Primitive::Bool)),
            else_ty: Box::new(Type::Never),
        };
        let json = serde_json::to_string(&c).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(c, back);
    }

    /// Two Conditionals differing only in then_ty must have distinct skeletons.
    #[test]
    fn conditional_then_changes_skeleton() {
        let c1 = Type::Conditional {
            check: Box::new(Type::TypeVar("T".to_owned())),
            extends_ty: Box::new(Type::Primitive(Primitive::Str)),
            then_ty: Box::new(Type::Primitive(Primitive::Bool)),
            else_ty: Box::new(Type::Never),
        };
        let c2 = Type::Conditional {
            check: Box::new(Type::TypeVar("T".to_owned())),
            extends_ty: Box::new(Type::Primitive(Primitive::Str)),
            then_ty: Box::new(Type::I32),
            else_ty: Box::new(Type::Never),
        };
        assert_ne!(ty_skeleton(&c1), ty_skeleton(&c2));
    }

    // ── Type::Mapped ──────────────────────────────────────────────────────────

    #[test]
    fn mapped_roundtrip() {
        let m = Type::Mapped {
            key_var: "P".to_owned(),
            source: Box::new(Type::TypeVar("T".to_owned())),
            value: Box::new(Type::Any),
            readonly: MappedModifier::Add,
            optional: MappedModifier::Remove,
        };
        let json = serde_json::to_string(&m).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(m, back);
    }

    /// `{ readonly [P in K]: V }` and `{ [P in K]: V }` must have distinct
    /// skeletons — the readonly modifier is identity-relevant.
    #[test]
    fn mapped_readonly_changes_skeleton() {
        let with_readonly = Type::Mapped {
            key_var: "P".to_owned(),
            source: Box::new(Type::Any),
            value: Box::new(Type::Any),
            readonly: MappedModifier::Add,
            optional: MappedModifier::Absent,
        };
        let without_readonly = Type::Mapped {
            key_var: "P".to_owned(),
            source: Box::new(Type::Any),
            value: Box::new(Type::Any),
            readonly: MappedModifier::Absent,
            optional: MappedModifier::Absent,
        };
        assert_ne!(ty_skeleton(&with_readonly), ty_skeleton(&without_readonly));
    }

    /// The key_var name is alpha-equivalent — two Mapped types differing only
    /// in key_var name must have the SAME skeleton.
    #[test]
    fn mapped_key_var_is_alpha_equivalent_in_skeleton() {
        let with_p = Type::Mapped {
            key_var: "P".to_owned(),
            source: Box::new(Type::Any),
            value: Box::new(Type::Any),
            readonly: MappedModifier::Absent,
            optional: MappedModifier::Absent,
        };
        let with_k = Type::Mapped {
            key_var: "K".to_owned(),
            source: Box::new(Type::Any),
            value: Box::new(Type::Any),
            readonly: MappedModifier::Absent,
            optional: MappedModifier::Absent,
        };
        assert_eq!(
            ty_skeleton(&with_p),
            ty_skeleton(&with_k),
            "key_var name is alpha-equivalent — must not change the skeleton"
        );
    }

    // ── Type::TemplateLiteral ─────────────────────────────────────────────────

    #[test]
    fn template_literal_roundtrip() {
        let tl = Type::TemplateLiteral(
            [
                TemplatePart::Literal("error-".to_owned()),
                TemplatePart::Interpolated(Box::new(Type::TypeVar("Code".to_owned()))),
            ]
            .into(),
        );
        let json = serde_json::to_string(&tl).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(tl, back);
    }

    /// `error-${T}` and `warning-${T}` must have distinct skeletons.
    #[test]
    fn template_literal_text_changes_skeleton() {
        let error_t = Type::TemplateLiteral(
            [
                TemplatePart::Literal("error-".to_owned()),
                TemplatePart::Interpolated(Box::new(Type::TypeVar("T".to_owned()))),
            ]
            .into(),
        );
        let warning_t = Type::TemplateLiteral(
            [
                TemplatePart::Literal("warning-".to_owned()),
                TemplatePart::Interpolated(Box::new(Type::TypeVar("T".to_owned()))),
            ]
            .into(),
        );
        assert_ne!(ty_skeleton(&error_t), ty_skeleton(&warning_t));
    }

    // ── Type::AnonymousRecord ─────────────────────────────────────────────────

    #[test]
    fn anonymous_record_struct_roundtrip() {
        let ar = Type::AnonymousRecord {
            form: AnonRecordForm::Struct,
            members: [
                AnonField {
                    name: "x".to_owned(),
                    ty: Type::I32,
                    optional: false,
                    readonly: false,
                },
                AnonField {
                    name: "y".to_owned(),
                    ty: Type::I32,
                    optional: true,
                    readonly: true,
                },
            ]
            .into(),
        };
        let json = serde_json::to_string(&ar).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ar, back);
    }

    #[test]
    fn anonymous_record_interface_roundtrip() {
        let ar = Type::AnonymousRecord {
            form: AnonRecordForm::Interface,
            members: [AnonField {
                name: "Foo".to_owned(),
                ty: Type::FunctionPointer {
                    params: [].into(),
                    ret: None,
                    abi: None,
                },
                optional: false,
                readonly: false,
            }]
            .into(),
        };
        let json = serde_json::to_string(&ar).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(ar, back);
    }

    /// `{ x: number }` and `{ x?: number }` must have different skeletons.
    #[test]
    fn anonymous_record_optional_changes_skeleton() {
        let required = Type::AnonymousRecord {
            form: AnonRecordForm::Struct,
            members: [AnonField {
                name: "x".to_owned(),
                ty: Type::I32,
                optional: false,
                readonly: false,
            }]
            .into(),
        };
        let optional = Type::AnonymousRecord {
            form: AnonRecordForm::Struct,
            members: [AnonField {
                name: "x".to_owned(),
                ty: Type::I32,
                optional: true,
                readonly: false,
            }]
            .into(),
        };
        assert_ne!(ty_skeleton(&required), ty_skeleton(&optional));
    }

    /// Struct form and interface form must have different skeletons.
    #[test]
    fn anonymous_record_form_changes_skeleton() {
        let s = Type::AnonymousRecord {
            form: AnonRecordForm::Struct,
            members: [].into(),
        };
        let i = Type::AnonymousRecord {
            form: AnonRecordForm::Interface,
            members: [].into(),
        };
        assert_ne!(ty_skeleton(&s), ty_skeleton(&i));
    }

    // ── Type::ImplTrait + Type::DynTrait ──────────────────────────────────────

    #[test]
    fn impl_trait_roundtrip() {
        let it = Type::ImplTrait([Type::Any, Type::SelfType].into());
        let json = serde_json::to_string(&it).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(it, back);
    }

    #[test]
    fn dyn_trait_roundtrip() {
        let dt = Type::DynTrait([Type::Any].into());
        let json = serde_json::to_string(&dt).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(dt, back);
    }

    /// `impl Trait` and `dyn Trait` with the same bounds must have DIFFERENT
    /// skeletons — they are distinct types with distinct dispatch semantics.
    #[test]
    fn impl_trait_vs_dyn_trait_differ_in_skeleton() {
        let impl_t = Type::ImplTrait([Type::Any].into());
        let dyn_t = Type::DynTrait([Type::Any].into());
        assert_ne!(ty_skeleton(&impl_t), ty_skeleton(&dyn_t));
    }

    #[test]
    fn impl_trait_bounds_change_skeleton() {
        let impl_a = Type::ImplTrait([Type::Any].into());
        let impl_b = Type::ImplTrait([Type::Never].into());
        assert_ne!(ty_skeleton(&impl_a), ty_skeleton(&impl_b));
    }

    // ── Type::Inferred ────────────────────────────────────────────────────────

    #[test]
    fn inferred_roundtrip() {
        let json = serde_json::to_string(&Type::Inferred).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(Type::Inferred, back);
    }

    /// Inferred and Any must have distinct skeletons.
    #[test]
    fn inferred_vs_any_differ_in_skeleton() {
        assert_ne!(ty_skeleton(&Type::Inferred), ty_skeleton(&Type::Any));
    }

    // ── Type::QualifiedPath ───────────────────────────────────────────────────

    #[test]
    fn qualified_path_roundtrip() {
        let qp = Type::QualifiedPath {
            self_ty: Box::new(Type::TypeVar("T".to_owned())),
            trait_ref: Some(Box::new(Type::Any)),
            assoc: "Item".to_owned(),
        };
        let json = serde_json::to_string(&qp).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(qp, back);
    }

    #[test]
    fn qualified_path_no_trait_ref_roundtrip() {
        let qp = Type::QualifiedPath {
            self_ty: Box::new(Type::Any),
            trait_ref: None,
            assoc: "Inner".to_owned(),
        };
        let json = serde_json::to_string(&qp).expect("serialize");
        let back: Type = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(qp, back);
    }

    /// `<T as Iterator>::Item` and `<T as Iterator>::IntoIter` must have
    /// distinct skeletons.
    #[test]
    fn qualified_path_assoc_changes_skeleton() {
        let item = Type::QualifiedPath {
            self_ty: Box::new(Type::TypeVar("T".to_owned())),
            trait_ref: Some(Box::new(Type::Any)),
            assoc: "Item".to_owned(),
        };
        let into_iter = Type::QualifiedPath {
            self_ty: Box::new(Type::TypeVar("T".to_owned())),
            trait_ref: Some(Box::new(Type::Any)),
            assoc: "IntoIter".to_owned(),
        };
        assert_ne!(ty_skeleton(&item), ty_skeleton(&into_iter));
    }

    /// A QualifiedPath with a trait_ref and one without must have distinct
    /// skeletons: `<T as Foo>::Bar` and `T.Bar` are structurally different.
    #[test]
    fn qualified_path_trait_ref_presence_changes_skeleton() {
        let with_trait = Type::QualifiedPath {
            self_ty: Box::new(Type::Any),
            trait_ref: Some(Box::new(Type::Any)),
            assoc: "Bar".to_owned(),
        };
        let without_trait = Type::QualifiedPath {
            self_ty: Box::new(Type::Any),
            trait_ref: None,
            assoc: "Bar".to_owned(),
        };
        assert_ne!(ty_skeleton(&with_trait), ty_skeleton(&without_trait));
    }
}
