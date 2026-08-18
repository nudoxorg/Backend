//! `Param` kind plus the `ParamAttribute` modifier enum.
use crate::{
    List,
    kinds::{ConstExpr, Type},
    visitor::Visitor,
};

/// A single parameter of a [`Function`](crate::kinds::Function).
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Param {
    /// The declared type of the parameter, if present.
    ///
    /// Dynamically-typed languages (and inferred bindings) may omit this.
    pub ty: Option<Type>,

    /// The argument value used when this parameter is omitted.
    pub default_value: Option<ConstExpr>,

    /// Calling-convention and modifier attributes that cannot be inferred from
    /// the type alone.
    pub attributes: List<ParamAttribute>,
}

#[bon::bon]
impl Param {
    #[builder]
    pub fn new(
        ty: Option<Type>,
        default_value: Option<ConstExpr>,
        #[builder(default, with = FromIterator::from_iter)] attributes: List<ParamAttribute>,
    ) -> Self {
        Param {
            ty,
            default_value,
            attributes,
        }
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

    /// Accepts zero or more trailing *keyword* arguments (`**kwargs`).
    ///
    /// Distinct from [`Self::Variadic`]: a positional rest parameter (`*args`)
    /// and a keyword rest parameter (`**kwargs`) are different calling
    /// conventions, and collapsing them makes the two unrepresentable as
    /// distinct API (docs/ISSUES.md L49-cs).
    Kwargs,

    /// Write-only output slot (C# `out`, no definite-assignment at the call site).
    ///
    /// Distinct from [`Self::Inout`]: `ref` requires the argument to be
    /// definitely assigned before the call; `out` does not. Mapping both to
    /// `Inout` loses that contract (docs/ISSUES.md L49-cs).
    Out,

    /// May be omitted entirely at call-sites.
    Optional,

    /// Must be passed by keyword; cannot be passed positionally.
    ///
    /// Python parameters after a bare `*` (`def f(a, *, b)`), and the same
    /// distinction in any language that has it. Without this the caller
    /// contract is misreported: a keyword-only parameter looks positional.
    KeywordOnly,
}
