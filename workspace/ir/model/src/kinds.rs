pub mod alias;
pub mod const_;
pub mod facts;
pub mod function;
pub mod generics;
pub mod impl_;
pub mod param;
pub mod record;
pub mod reexport;
pub mod static_;
pub mod sum;
pub mod trait_;
pub mod ty;
// pub mod protocols;
// pub mod syntax;

use crate::visitor::Visitor;

#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Module;

pub use self::{
    alias::Alias,
    const_::Const,
    facts::{AutoFact, AutoState, AutoTrait, Sealed, TriState},
    function::{FnModifier, Function, Receiver},
    generics::{GenericParam, WherePred, lifetime_label},
    impl_::{Impl, ImplFlags},
    param::{Param, ParamAttribute},
    record::{Field, FieldAttribute, FieldKey, Record, RecordForm},
    reexport::Reexport,
    static_::Static,
    sum::{Enum, Variant, VariantForm},
    trait_::{Trait, TraitFlags},
    ty::Type,
};
