use crate::{
    List,
    index::EntryIndex,
    kinds::{Generics, Param, Type},
    visitor::Visitor,
};

/// A function, method, or lambda with its full signature.
///
/// Parameters and generic parameters are promoted to their own entries and
/// referenced by index; the name lives on the owning entry's
/// [`Symbol`](crate::entry::Symbol).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Function {
    /// Generic parameters and `where`-clause of the function.
    pub generics: Generics,

    /// How the function receives its `self`/`this`, if at all.
    pub receiver: Option<Receiver>,

    /// The input parameters, in declaration order.
    pub input_params: List<EntryIndex<Param>>,

    /// The output parameters. Most languages have a single return; multi-value
    /// returns (Go) and out-parameters are additional entries here.
    pub output_params: List<EntryIndex<Param>>,

    /// Signature-level modifiers (`async`, `const`, `unsafe`, …).
    pub modifiers: List<FnModifier>,

    /// Whether the function has a body. `false` for a required trait method or
    /// an `extern`/abstract declaration.
    pub implemented: bool,
    // FIXME: explicitly-implemented protocols, overload sets, and the parsed
    // tree-sitter body are not yet ported; overloads and bodies are large
    // sub-designs of their own.
}

#[bon::bon]
impl Function {
    #[builder]
    pub fn new(
        #[builder(default)] generics: Generics,
        receiver: Option<Receiver>,
        #[builder(with = FromIterator::from_iter)] input_params: List<EntryIndex<Param>>,
        #[builder(with = FromIterator::from_iter)] output_params: List<EntryIndex<Param>>,
        #[builder(with = FromIterator::from_iter)] modifiers: List<FnModifier>,
        #[builder(default = true)] implemented: bool,
    ) -> Self {
        Function {
            generics,
            receiver,
            input_params,
            output_params,
            modifiers,
            implemented,
        }
    }
}

/// How a method receives its instance (`self`, `this`, …).
///
/// A free function (or a static/class method) carries no receiver at all
/// (`Option::None`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Receiver {
    /// Takes ownership (`self`, Swift `consuming`).
    Owned,

    /// Immutable reference (`&self`, Swift `borrowing`).
    SharedRef,

    /// Mutable reference (`&mut self`, Swift `mutating`).
    MutRef,

    /// An arbitrary receiver type (Rust arbitrary self types, `self: Rc<Self>`).
    Arbitrary(EntryIndex<Type>),
}

/// A signature-level modifier on a [`Function`] or function type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum FnModifier {
    /// Runs asynchronously (`async`).
    Async,

    /// Evaluable at compile time (`const`, `constexpr`, `consteval`).
    Const,

    /// Requires an unsafe context (Rust `unsafe`).
    Unsafe,

    /// Free of observable side effects (`pure`, `[[gnu::pure]]`).
    Pure,

    /// Can suspend and resume between yields (a generator/coroutine).
    Generator,

    /// A C++ `noexcept` / non-throwing function.
    NoThrow,

    /// A virtual method (C++ `virtual`, overridable dynamic dispatch).
    Virtual,

    /// An abstract / pure-virtual method (`virtual … = 0`, `abstract`).
    Abstract,

    /// An `extern` / foreign-ABI function.
    Extern,
}
