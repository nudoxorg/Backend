use crate::List;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Function {
	pub input_params:  List<Param>,
	pub output_params: List<Param>,
	// TODO: rest
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Param {}
