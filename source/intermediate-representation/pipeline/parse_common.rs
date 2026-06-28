use crate::{parameter::{LiteralParameter, Parameter}, ty::Type};

pub fn output_parameters_from_type(ty: Type) -> Option<Vec<Parameter>> {
	Some(vec![Parameter::Literal(LiteralParameter {
		name:          String::new(),
		r#type:        Some(ty),
		attributes:    None,
		default_value: None,
		description:   None,
	})])
}

pub fn parameter_link_key(prefix: &str, idx: usize, total: usize, name: &str) -> String {
	if !name.is_empty() {
		format!("{prefix}.{name}")
	} else if total == 1 {
		prefix.to_string()
	} else {
		format!("{prefix}.{idx}")
	}
}
