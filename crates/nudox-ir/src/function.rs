#[derive(Debug, Clone, PartialEq)]
pub struct Function {
	pub input_params:  Vec<Param>,
	pub output_params: Vec<Param>,
	// TODO: rest
}

#[derive(Debug, Clone, PartialEq)]
pub enum Param {}
