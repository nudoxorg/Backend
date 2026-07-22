pub mod function;
pub mod record;
pub mod ty;
// pub mod generics;
// pub mod parameter;
// pub mod protocols;
// pub mod syntax;

use crate::visitor::Visitor;

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Module;

pub use self::{
    function::Function,
    record::{Field, Record},
    ty::Type,
};
