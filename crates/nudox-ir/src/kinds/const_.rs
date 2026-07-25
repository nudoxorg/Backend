use crate::{kinds::Type, visitor::Visitor};

// FIXME: the initializer expression (a `ConstExpr`) is not yet ported; it
// awaits the const-expression subsystem.

/// A compile-time constant declaration.
///
/// The const's name, visibility, and documentation live on the owning
/// [`Entry`](crate::entry::Entry)'s [`Symbol`](crate::entry::Symbol).
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Const {
    /// The declared type of the constant.
    pub ty: Type,
}

#[bon::bon]
impl Const {
    #[builder]
    pub fn new(ty: Type) -> Self {
        Const { ty }
    }
}
