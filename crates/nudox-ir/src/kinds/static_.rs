use crate::{kinds::Type, visitor::Visitor};

// FIXME: the initializer expression (a `ConstExpr`) and linkage attributes are
// not yet ported; they await the const-expression and attribute subsystems.

/// A static variable declaration.
///
/// The static's name, visibility, and documentation live on the owning
/// [`Entry`](crate::entry::Entry)'s [`Symbol`](crate::entry::Symbol).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Static {
    /// The declared type of the static variable.
    pub ty: Type,

    /// Whether the static may be mutated (`static mut` in Rust).
    pub mutable: bool,
}

#[bon::bon]
impl Static {
    #[builder]
    pub fn new(ty: Type, mutable: bool) -> Self {
        Static { ty, mutable }
    }
}
