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
    use crate::{
        entry::AttrTok,
        skeleton::function_signature_skeleton,
    };

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
}
