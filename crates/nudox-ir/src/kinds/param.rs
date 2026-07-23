use crate::{List, index::EntryIndex, kinds::Type, visitor::Visitor};

/// A single parameter of a [`Function`](crate::kinds::Function).
///
/// The parameter's name, visibility, and documentation live on the owning
/// [`Entry`](crate::entry::Entry)'s [`Symbol`](crate::entry::Symbol); this
/// carries only the parameter-specific data.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Param {
    /// The declared type of the parameter, if present.
    ///
    /// Dynamically-typed languages (and inferred bindings) may omit this.
    pub ty: Option<EntryIndex<Type>>,

    /// Calling-convention and modifier attributes that cannot be inferred from
    /// the type alone.
    pub attributes: List<ParamAttribute>,
    // FIXME: `default_value` (a `ConstExpr`) is not yet ported — it depends on
    // the const-expression subsystem, which is intentionally deferred.
}

#[bon::bon]
impl Param {
    #[builder]
    pub fn new(
        ty: Option<EntryIndex<Type>>,
        #[builder(with = FromIterator::from_iter)] attributes: List<ParamAttribute>,
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
