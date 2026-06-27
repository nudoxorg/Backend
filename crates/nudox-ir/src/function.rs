use ecow::EcoVec;

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
	pub input_params:  EcoVec<Param>,
	pub output_params: EcoVec<Param>,
	// TODO: rest
}

#[derive(Debug, Clone, PartialEq)]
pub enum Param {}
