use crate::{List, kinds::Type, visitor::Visitor};

// FIXME: `default_value` (a `ConstExpr`) is not yet ported — it depends on
// the const-expression subsystem, which is intentionally deferred.

/// A single parameter of a [`Function`](crate::kinds::Function).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Param {
    /// The declared type of the parameter, if present.
    ///
    /// Dynamically-typed languages (and inferred bindings) may omit this.
    pub ty: Option<Type>,

    /// Calling-convention and modifier attributes that cannot be inferred from
    /// the type alone.
    pub attributes: List<ParamAttribute>,
}

#[bon::bon]
impl Param {
    #[builder]
    pub fn new(
        ty: Option<Type>,
        #[builder(default, with = FromIterator::from_iter)] attributes: List<ParamAttribute>,
    ) -> Self {
        Param { ty, attributes }
    }
}

/// A calling-convention or modifier attribute on a [`Param`].
///
/// These flags capture language-level modifiers that affect how a value is
/// passed into or out of a function and cannot be recovered from the type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum ParamAttribute {
    /// Passed by mutable reference (Swift `inout`, C++ `&`).
    Inout,

    /// Locally re-assignable but not passed by reference.
    Mutable,

    /// Ownership is transferred to the callee (Swift `consuming`).
    Consuming,

    /// A shared borrow — read without taking ownership.
    Borrowing,

    /// Isolated to a particular actor or concurrency domain.
    Isolated,

    /// Accepts zero or more trailing arguments of the same type.
    Variadic,

    /// May be omitted entirely at call-sites.
    Optional,
}
