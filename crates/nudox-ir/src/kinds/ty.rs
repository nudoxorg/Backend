use std::num::{NonZero, NonZeroU16};

use crate::{List, index::RawRef, visitor::Visitor};

#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Type {
    /// A receiver/self type such as Rust `Self` or TypeScript `this`.
    SelfType,

    /// A fundamental, language-level built-in type.
    /// Ex: `i32`, `f64`, `bool`.
    Primitive(Primitive),

    /// A fixed-length, heterogeneous collection of types.
    /// Ex: `(i32, String)`. An empty vec `()` represents the Unit type.
    Tuple(List<Type>),

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
}

/// Use-site variance for a [`Type::Wildcard`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Variance {
    /// Unbounded, or bound in neither direction (`?`).
    Invariant,
    /// Bounded above (`? extends T`, `out T`).
    Covariant,
    /// Bounded below (`? super T`, `in T`).
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
