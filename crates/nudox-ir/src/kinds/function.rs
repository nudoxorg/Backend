use crate::{
    List,
    index::Ref,
    kinds::{GenericParam, Param, WherePred},
    visitor::Visitor,
};

// Generics and where-clauses are now modelled via `generics:
// List<GenericParam>` and `wheres: List<WherePred>` (bounds represented as
// `List<Type>`). Remaining deferred items: overloads, explicitly-implemented
// protocols, the parsed function body, const-expressions, and variance.

/// A function, method, or lambda with its full signature.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Function {
    /// How the function receives its `self`/`this`, if at all.
    ///
    /// Free functions (and static/class methods) carry no receiver.
    pub receiver: Option<Receiver>,

    /// The input parameters, in declaration order.
    pub input_params: List<Ref<Param>>,

    /// The output parameters.
    ///
    /// Most languages have a single return type; multi-value returns (Go) and
    /// out-parameters are modelled as additional entries here.
    pub output_params: List<Ref<Param>>,

    /// Signature-level modifiers (`async`, `const`, `unsafe`, …).
    pub modifiers: List<FnModifier>,

    /// Generic parameters declared on this function, in declaration order.
    pub generics: List<GenericParam>,

    /// Where-clause predicates for this function, in declaration order.
    pub wheres: List<WherePred>,
}

#[bon::bon]
impl Function {
    #[builder]
    pub fn new(
        receiver: Option<Receiver>,
        #[builder(default, with = FromIterator::from_iter)] input_params: List<Ref<Param>>,
        #[builder(default, with = FromIterator::from_iter)] output_params: List<Ref<Param>>,
        #[builder(default, with = FromIterator::from_iter)] modifiers: List<FnModifier>,
        #[builder(default, with = FromIterator::from_iter)] generics: List<GenericParam>,
        #[builder(default, with = FromIterator::from_iter)] wheres: List<WherePred>,
    ) -> Self {
        Function {
            receiver,
            input_params,
            output_params,
            modifiers,
            generics,
            wheres,
        }
    }
}

/// How a method receives its instance (`self`, `this`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Receiver {
    /// Takes ownership (`self` in Rust, `consuming` in Swift).
    Owned,

    /// Immutable reference (`&self`, `borrowing` in Swift).
    SharedRef,

    /// Mutable reference (`&mut self`, `mutating` in Swift).
    MutRef,

    /// An arbitrary receiver type (Rust's arbitrary self types).
    // FIXME: arbitrary receivers can name a concrete type (e.g. `self: Rc<Self>`);
    // that type is not yet represented.
    Arbitrary,
}

/// A signature-level modifier on a [`Function`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum FnModifier {
    /// Runs asynchronously (`async`).
    Async,

    /// Evaluable at compile time (`const`, `constexpr`).
    Const,

    /// Requires an unsafe context (Rust `unsafe`).
    Unsafe,

    /// Free of observable side effects.
    Pure,

    /// Can suspend and resume between yields (a generator/coroutine).
    Generator,
}
