#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Function {
	pub input_params:  Vec<Param>,
	pub output_params: Vec<Param>,
	// TODO: rest
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Param {}
