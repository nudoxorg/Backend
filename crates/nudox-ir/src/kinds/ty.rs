use std::num::{NonZero, NonZeroU16};

use crate::{List, index::EntryIndex, visitor::Visitor};

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Type {
    /// A receiver/self type such as Rust `Self` or TypeScript `this`.
    SelfType,

    /// A fundamental, language-level built-in type.
    /// Ex: `i32`, `f64`, `bool`.
    Primitive(Primitive),

    /// A fixed-length, heterogeneous collection of types.
    /// Ex: `(i32, String)`. An empty vec `()` represents the Unit type.
    Tuple(List<EntryIndex<Type>>),

    /// A dynamically-sized view into a contiguous sequence.
    /// Ex: `[u8]` or `[]T`.
    Slice(EntryIndex<Type>),

    /// A fixed-size contiguous sequence.
    /// Ex: `[i32; 4]` or `std::array<int, 4>`.
    Array { ty: EntryIndex<Type>, length: usize },

    /// An untagged union or sum of types.
    /// Ex: `string | number`.
    Union(List<EntryIndex<Type>>),

    /// An intersection or combination of types.
    /// Ex: `Serializable & Cloneable`.
    Intersection(List<EntryIndex<Type>>),

    /// Represents a type that cannot exist (Bottom Type).
    /// Ex: `!` in Rust, `never` in TypeScript, `NoReturn` in Python.
    Never,

    /// Represents the "All" type (Top Type).
    /// Ex: `any` or `unknown` in TypeScript, `Object` in Java.
    Any,
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
    MutPointer(EntryIndex<Type>),

    /// A raw, const, unmanaged pointer.
    /// Ex: `*const T`, `int *const`.
    ConstPointer(EntryIndex<Type>),

    /// A managed reference with optional lifetime/mutability tracking.
    /// Ex: `&'a mut T`.
    Reference {
        lifetime: Option<String>,
        mutable: bool,
        ty: EntryIndex<Type>,
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
