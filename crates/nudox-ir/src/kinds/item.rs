use crate::{
    List,
    index::EntryIndex,
    kinds::{Bound, ConstExpr, Generics, Type},
    visitor::Visitor,
};

/// A named constant or immutable binding — a free `const`, a class constant, or
/// a trait's required/provided associated const.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Const {
    /// The declared type of the constant.
    pub ty: Option<EntryIndex<Type>>,

    /// The initializer. `None` for a *required* associated const with no
    /// default.
    pub value: Option<ConstExpr>,
}

#[bon::bon]
impl Const {
    #[builder]
    pub fn new(ty: Option<EntryIndex<Type>>, value: Option<ConstExpr>) -> Self {
        Const { ty, value }
    }
}

/// A mutable global or static variable.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Static {
    /// The declared type of the static.
    pub ty: Option<EntryIndex<Type>>,

    /// Whether the static is mutable (`static mut`, a non-`const` global).
    pub mutable: bool,

    /// The initializer, if any.
    pub value: Option<ConstExpr>,
}

#[bon::bon]
impl Static {
    #[builder]
    pub fn new(
        ty: Option<EntryIndex<Type>>,
        #[builder(default)] mutable: bool,
        value: Option<ConstExpr>,
    ) -> Self {
        Static { ty, mutable, value }
    }
}

/// A type alias / `typedef` / `using`, or a trait's associated type.
///
/// A plain alias has a `target`; an associated-type *requirement* omits it and
/// may carry `bounds` (`type Item: Display;`).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Alias {
    /// Generic parameters of the alias (`type Pair<T> = (T, T)`).
    pub generics: Generics,

    /// Bounds on an associated type requirement (`type Item: Display`).
    pub bounds: List<Bound>,

    /// The aliased type. `None` for a required associated type with no default.
    pub target: Option<EntryIndex<Type>>,
}

#[bon::bon]
impl Alias {
    #[builder]
    pub fn new(
        #[builder(default)] generics: Generics,
        #[builder(with = FromIterator::from_iter)] bounds: List<Bound>,
        target: Option<EntryIndex<Type>>,
    ) -> Self {
        Alias {
            generics,
            bounds,
            target,
        }
    }
}

/// A macro definition, exposed as a first-class API symbol.
///
/// `macro_rules!` macros exported via `#[macro_export]`, procedural macros, and
/// C preprocessor `#define`s are all real, referenceable public-API items.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Macro {
    /// What flavour of macro this is.
    pub kind: MacroKind,
}

#[bon::bon]
impl Macro {
    #[builder]
    pub fn new(kind: MacroKind) -> Self {
        Macro { kind }
    }
}

/// The flavour of a [`Macro`] definition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum MacroKind {
    /// A declarative macro (`macro_rules!`, Rust 2.0 `macro`).
    Declarative,

    /// A function-like procedural macro (`foo!(…)`).
    ProcFunction,

    /// An attribute procedural macro (`#[foo]`).
    ProcAttribute,

    /// A derive procedural macro (`#[derive(Foo)]`).
    ProcDerive,

    /// A C/C++ preprocessor `#define`.
    Preprocessor,
}
