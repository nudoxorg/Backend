use crate::{
    List,
    index::{EntryIndex, UntypedEntryIndex},
    kinds::Type,
    visitor::Visitor,
};

/// A compile-time constant expression.
///
/// Appears wherever a value must be known at compile time: array lengths,
/// const-generic arguments, field/parameter defaults, enum discriminants, and
/// dependent-type annotations.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum ConstExpr {
    /// An integer literal (`4`, `-1`). Stored wide enough for any source width.
    Int(i128),

    /// A floating-point literal, stored as its IEEE-754 bit pattern so the
    /// expression tree stays `Eq`/`Hash`-able and lossless. See
    /// [`ConstExpr::float`].
    Float(u64),

    /// A boolean literal.
    Bool(bool),

    /// A string literal.
    Str(String),

    /// A resolved reference to a named binding — a
    /// [`Const`](crate::kinds::Const) or a const
    /// [`Generic`](crate::kinds::Generic) parameter.
    Path(UntypedEntryIndex),

    /// An as-yet-unresolved named binding (e.g. `MAX`, `N`).
    Name(String),

    /// A binary operation (`N + 1`, `SIZE * 2`).
    BinOp {
        op: BinOp,
        lhs: Box<ConstExpr>,
        rhs: Box<ConstExpr>,
    },

    /// A unary operation (`-N`, `!flag`).
    UnaryOp {
        op: UnaryOp,
        operand: Box<ConstExpr>,
    },

    /// A const function / intrinsic call (`size_of::<T>()`, `min(A, B)`).
    Call { func: String, args: List<ConstExpr> },

    /// A type ascription used in dependent contexts (`(expr : Ty)`).
    Ascription {
        expr: Box<ConstExpr>,
        ty: EntryIndex<Type>,
    },
}

impl ConstExpr {
    /// Construct a [`ConstExpr::Float`] from an `f64`, storing its bit pattern.
    pub fn float(value: f64) -> Self {
        ConstExpr::Float(value.to_bits())
    }
}

/// A binary operator permitted inside a [`ConstExpr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

/// A unary operator permitted inside a [`ConstExpr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum UnaryOp {
    /// Arithmetic negation (`-x`).
    Neg,

    /// Logical or bitwise negation (`!x`).
    Not,

    /// Borrow / address-of (`&x`).
    Ref,

    /// Dereference (`*x`).
    Deref,
}
