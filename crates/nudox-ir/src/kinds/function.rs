use crate::{List, index::EntryIndex, kinds::Param, visitor::Visitor};

// FIXME: generics, overloads, explicitly-implemented protocols, and the
// parsed function body are not yet ported. Generics in particular await the
// dedicated generics subsystem, which is intentionally deferred.

/// A function, method, or lambda with its full signature.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Function {
    /// How the function receives its `self`/`this`, if at all.
    ///
    /// Free functions (and static/class methods) carry no receiver.
    pub receiver: Option<Receiver>,

    /// The input parameters, in declaration order.
    pub input_params: List<EntryIndex<Param>>,

    /// The output parameters.
    ///
    /// Most languages have a single return type; multi-value returns (Go) and
    /// out-parameters are modelled as additional entries here.
    pub output_params: List<EntryIndex<Param>>,

    /// Signature-level modifiers (`async`, `const`, `unsafe`, …).
    pub modifiers: List<FnModifier>,
}

#[bon::bon]
impl Function {
    #[builder]
    pub fn new(
        receiver: Option<Receiver>,
        #[builder(with = FromIterator::from_iter)] input_params: List<EntryIndex<Param>>,
        #[builder(with = FromIterator::from_iter)] output_params: List<EntryIndex<Param>>,
        #[builder(with = FromIterator::from_iter)] modifiers: List<FnModifier>,
    ) -> Self {
        Function {
            receiver,
            input_params,
            output_params,
            modifiers,
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
