pub mod function;
pub mod generic;
pub mod item;
pub mod konst;
pub mod param;
pub mod record;
pub mod sum;
pub mod traits;
pub mod ty;

use crate::visitor::Visitor;

/// A namespace, package, or module — a container for other entries.
///
/// Its members are the owning entry's children; the module itself carries no
/// inline data.
#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Module;

pub use self::{
    function::{FnModifier, Function, Receiver},
    generic::{
        Bound, ConstParam, Constraint, Generic, GenericArg, Generics, LifetimeParam, TraitRef,
        TypeKind, TypeParam, TypeParamOrigin, Variance,
    },
    item::{Alias, Const, Static},
    konst::{BinOp, ConstExpr, UnaryOp},
    param::{Param, ParamAttribute},
    record::{Field, FieldAttribute, FieldKey, Record},
    sum::{Enum, Variant},
    traits::{Impl, ImplAttribute, ImplPolarity, Trait, TraitAttribute},
    ty::{
        CvQualifiers, FnType, Modifier, PredicateSubject, Primitive, RefKind, Type, TypeOperator,
        Width,
    },
};
