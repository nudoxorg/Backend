//! Shared parser helpers.
//!
//! The free helpers below were byte-identical across the Rust and TypeScript
//! parsers and now live here once. Each parser keeps its own concrete
//! `from_doc` / `parse` inherent methods — dispatch over languages is the
//! `PackageHandle` enum, not a trait.

use ir::{parameter::{LiteralParameter, Parameter}, ty::Type};

/// Wrap a single type as the sole, unnamed return parameter of a function
/// signature. Identical across the Rust and TypeScript parsers.
pub(crate) fn output_parameters_from_type(ty: Type) -> Option<Vec<Parameter>> {
	Some(vec![Parameter::Literal(LiteralParameter {
		name:          String::new(),
		r#type:        Some(ty),
		attributes:    None,
		default_value: None,
		description:   None,
	})])
}

/// Build the stable link key for the `idx`-th parameter of a signature.
///
/// Named parameters key on their name; a sole positional parameter keys on the
/// bare prefix; otherwise the positional index disambiguates. Identical across
/// both parsers.
pub(crate) fn parameter_link_key(prefix: &str, idx: usize, total: usize, name: &str) -> String {
	if !name.is_empty() {
		format!("{prefix}.{name}")
	} else if total == 1 {
		prefix.to_string()
	} else {
		format!("{prefix}.{idx}")
	}
}
