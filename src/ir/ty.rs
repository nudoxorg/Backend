use crate::ir::{
    generics::{GenericArg, TraitRef, TypeParam},
    parameter::Parameter,
    primitives::Primitive,
    protocols::GenericBound,
    record::SumVariant,
};

use super::function;

/// Universal representation of types across languages.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))] // Example for tagged enum
pub enum Type {
    #[cfg_attr(feature = "serde", serde(rename = "resolvedPath"))]
    ResolvedPath(Path),
    #[cfg_attr(feature = "serde", serde(rename = "dynTrait"))]
    DynTrait(DynTrait),
    #[cfg_attr(feature = "serde", serde(rename = "genericParam"))]
    GenericParam(String),
    Primitive(Primitive),
    #[cfg_attr(feature = "serde", serde(rename = "functionPointer"))]
    FunctionPointer(FunctionPointer),
    Tuple(Vec<Type>),
    Slice(Box<Type>),
    Array {
        ty: Box<Type>,
        length: usize,
    },
    Pattern {
        ty: Box<Type>,
    },
    #[cfg_attr(feature = "serde", serde(rename = "implTrait"))]
    ImplTrait(Vec<GenericBound>),
    Infer,
    #[cfg_attr(feature = "serde", serde(rename = "rawPointer"))]
    RawPointer {
        is_mutable: bool,
        ty: Box<Type>,
    },
    Union(Vec<Type>),
    Sum(Vec<SumVariant>),
    Intersection(Vec<Type>),
    #[cfg_attr(feature = "serde", serde(rename = "borrowedRef"))]
    BorrowedRef {
        lifetime: Option<String>,
        is_mutable: bool,
        ty: Box<Type>,
    },
    #[cfg_attr(feature = "serde", serde(rename = "qualifiedPath"))]
    QualifiedPath(QualifiedPath),
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Path {
    pub path: String,
    pub generic_args: Option<Vec<GenericArg>>,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct QualifiedPath {
    pub name: String,
    pub generic_arguments: Option<Vec<GenericArg>>,
    pub self_type: Box<Type>,
    pub tr: Option<Path>,
}
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct DynTrait {
    pub traits: Vec<PolyTrait>,
    pub lifetime: Option<String>,
}

// MARK: - FunctionPointer

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FunctionPointer {
    pub inputs: Option<Vec<Parameter>>,
    pub outputs: Option<Vec<Parameter>>,
    pub generic_params: Option<Vec<TypeParam>>,
    pub attributes: Option<Vec<function::Attribute>>,
}

// MARK: - PolyTrait

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct PolyTrait {
    pub tr: TraitRef,
    pub lifetimes: Vec<String>,
}
