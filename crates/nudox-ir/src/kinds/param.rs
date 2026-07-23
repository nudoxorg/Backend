use crate::{
    List,
    index::EntryIndex,
    kinds::{ConstExpr, Type},
    visitor::Visitor,
};

/// A single value parameter of a [`Function`](crate::kinds::Function) or function
/// type.
///
/// The parameter's name lives on the owning [`Entry`](crate::entry::Entry)'s
/// [`Symbol`](crate::entry::Symbol); this carries only the parameter-specific data.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Param {
    /// The declared type of the parameter, if present.
    pub ty: Option<EntryIndex<Type>>,

    /// Calling-convention and modifier attributes that cannot be inferred from
    /// the type alone.
    pub attributes: List<ParamAttribute>,

    /// The default value used when the caller omits this argument.
    pub default: Option<ConstExpr>,
}

#[bon::bon]
impl Param {
    #[builder]
    pub fn new(
        ty: Option<EntryIndex<Type>>,
        #[builder(with = FromIterator::from_iter)] attributes: List<ParamAttribute>,
        default: Option<ConstExpr>,
    ) -> Self {
        Param {
            ty,
            attributes,
            default,
        }
    }
}

/// A calling-convention or modifier attribute on a [`Param`].
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
