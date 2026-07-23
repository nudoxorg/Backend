use std::num::{NonZero, NonZeroU16};

use crate::{
    List,
    index::{EntryIndex, UntypedEntryIndex},
    kinds::{Bound, ConstExpr, FnModifier, Generic, GenericArg, Param, TraitRef},
    visitor::Visitor,
};

/// A type expression.
///
/// Structural forms nest other [`Type`] entries by [`EntryIndex`]; nominal
/// forms point at the declaration that introduces the type. The variant set
/// spans the Rust, C/C++, TypeScript, Java/C#, and ML-family type systems.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Type {
    /// A nominal reference to a declaration, with generic arguments.
    /// Ex: `MyStruct`, `Vec<T>`, `std::map<K, V>`, `List<String>`.
    Named {
        /// The entry that defines the type (record, enum, trait, alias, …).
        def: UntypedEntryIndex,
        /// Generic arguments applied at this use site.
        args: List<GenericArg>,
    },

    /// A use of an in-scope generic type parameter (the `T` in `Box<T>`).
    Param(EntryIndex<Generic>),

    /// A receiver/self type (`Self`, `this`).
    SelfType,

    /// A fundamental, language-level scalar type (`i32`, `f64`, `bool`).
    Primitive(Primitive),

    /// A literal type — TypeScript `"foo"`, `42`, `true`; C++ non-type value.
    Literal(ConstExpr),

    /// A fixed-length, heterogeneous product (`(i32, String)`). Empty = unit.
    Tuple(List<EntryIndex<Type>>),

    /// A dynamically-sized view into a contiguous sequence (`[u8]`, `[]T`).
    Slice(EntryIndex<Type>),

    /// A fixed-size contiguous sequence (`[i32; 4]`, `std::array<int, 4>`).
    Array {
        ty: EntryIndex<Type>,
        length: ConstExpr,
    },

    /// A raw, unmanaged pointer (`*mut T`, `*const T`, `int*`).
    Pointer {
        mutable: bool,
        pointee: EntryIndex<Type>,
    },

    /// A managed reference / borrow (`&'a mut T`, `T&`, `T&&`).
    Reference {
        lifetime: Option<EntryIndex<Generic>>,
        mutable: bool,
        kind: RefKind,
        pointee: EntryIndex<Type>,
    },

    /// A C/C++ cv-qualified type (`const T`, `volatile T`, `const volatile T`).
    Qualified {
        qualifiers: CvQualifiers,
        ty: EntryIndex<Type>,
    },

    /// A C++ pointer-to-member (`int Class::*`, `R (Class::*)(Args)`).
    MemberPointer {
        class: EntryIndex<Type>,
        member: EntryIndex<Type>,
    },

    /// A function type or function pointer.
    /// Ex: `fn(i32) -> bool`, `int (*)(int)`, `(a: number) => string`.
    Function(FnType),

    /// A dynamically-dispatched trait object / interface type.
    /// Ex: `dyn Display + Send`, `Runnable`, `IFoo`.
    DynTrait {
        traits: List<TraitRef>,
        lifetime: Option<EntryIndex<Generic>>,
    },

    /// An opaque, bound-constrained existential (`impl Iterator<Item = u8>`).
    Impl(List<Bound>),

    /// A fully-qualified associated-type projection (`<T as Trait>::Item`).
    QualifiedPath {
        self_ty: EntryIndex<Type>,
        as_trait: Option<TraitRef>,
        name: String,
        args: List<GenericArg>,
    },

    /// A variadic / parameter-pack expansion (`...T`, `Args...`).
    Pack(EntryIndex<Type>),

    /// An untagged union of types (`string | number`).
    Union(List<EntryIndex<Type>>),

    /// A structural intersection (`Serializable & Cloneable`).
    Intersection(List<EntryIndex<Type>>),

    /// A type-level operator (`keyof T`, `readonly T`, `typeof x`,
    /// `decltype(e)`).
    Operator {
        op: TypeOperator,
        ty: EntryIndex<Type>,
    },

    /// A conditional type (`T extends U ? X : Y`).
    Conditional {
        check: EntryIndex<Type>,
        extends: EntryIndex<Type>,
        then: EntryIndex<Type>,
        otherwise: EntryIndex<Type>,
    },

    /// A mapped type (`{ [K in keyof T]: T[K] }`).
    Mapped {
        readonly: Option<Modifier>,
        optional: Option<Modifier>,
        param: EntryIndex<Generic>,
        constraint: EntryIndex<Type>,
        value: Option<EntryIndex<Type>>,
    },

    /// A type predicate (`value is Foo`, `asserts this is Bar`).
    Predicate {
        asserts: bool,
        subject: PredicateSubject,
        ty: Option<EntryIndex<Type>>,
    },

    /// A placeholder to be inferred (`_`, `auto`, `var`).
    Infer,

    /// The bottom / uninhabited type (`!`, `never`, `NoReturn`).
    Never,

    /// The top / unknown type (`any`, `unknown`, `object`).
    Any,
}

/// Whether a C++ [`Type::Reference`] is an lvalue or rvalue reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum RefKind {
    /// An lvalue reference (`T&`, Rust `&T`).
    Lvalue,

    /// An rvalue reference (`T&&`).
    Rvalue,
}

/// C/C++ `const` / `volatile` qualification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct CvQualifiers {
    pub is_const: bool,
    pub is_volatile: bool,
}

/// A function type / function pointer signature.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct FnType {
    /// The parameters of the signature.
    pub params: List<EntryIndex<Param>>,

    /// The return type (`None` denotes an unwritten / inferred return).
    pub ret: Option<EntryIndex<Type>>,

    /// Signature modifiers (`async`, `unsafe`, `const`, …).
    pub modifiers: List<FnModifier>,

    /// A trailing C-style variadic (`printf(const char*, ...)`).
    pub c_variadic: bool,
}

/// A whole-type operator applied to another type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum TypeOperator {
    /// TypeScript `keyof T`.
    KeyOf,

    /// TypeScript `readonly T`.
    ReadOnly,

    /// TypeScript `typeof x`.
    TypeOf,

    /// C++ `decltype(e)` / TS-style unique/`infer` markers.
    DeclType,

    /// TypeScript `unique symbol`.
    Unique,
}

/// The add/remove/preserve modifier applied in a mapped type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Modifier {
    /// Leave the modifier as-is (`readonly`).
    Preserve,

    /// Add the modifier (`+readonly`).
    Add,

    /// Remove the modifier (`-readonly`).
    Remove,
}

/// The subject of a [`Type::Predicate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum PredicateSubject {
    /// `this is Foo`.
    This,

    /// `param is Foo` — the narrowed parameter.
    Param(EntryIndex<Param>),
}

/// A language-level primitive (scalar) type, independent of target
/// architecture.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Primitive {
    Integer {
        signed: bool,
        width: Width,
    },

    Float(Width),

    Bool,

    Char,

    /// Type of a primitive string slice (`str`, `string`), not an owned string.
    Str,

    /// The C/C++ `void` type / absence of a value.
    Void,

    /// An arbitrary language builtin not otherwise modelled (`Date`, `symbol`).
    Builtin(String),
}

/// The bit-width of a primitive numeric type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Width {
    Fixed(NonZeroU16),

    /// Machine-dependent / pointer-sized (`usize`, `isize`). Not for floats.
    Arch,
}

impl Width {
    /// 8-bit width.
    pub const W8: Self = Width::Fixed(NonZero::new(8).unwrap());

    /// 16-bit width.
    pub const W16: Self = Width::Fixed(NonZero::new(16).unwrap());

    /// 32-bit width.
    pub const W32: Self = Width::Fixed(NonZero::new(32).unwrap());

    /// 64-bit width.
    pub const W64: Self = Width::Fixed(NonZero::new(64).unwrap());

    /// 80-bit width (x87 extended precision).
    pub const W80: Self = Width::Fixed(NonZero::new(80).unwrap());

    /// 128-bit width.
    pub const W128: Self = Width::Fixed(NonZero::new(128).unwrap());
}
