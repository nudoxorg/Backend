use crate::{List, visitor::Visitor};

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Function {
    pub input_params: List<Param>,
    pub output_params: List<Param>,
    // TODO: rest
}

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Param {}
