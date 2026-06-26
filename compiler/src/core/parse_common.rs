//! Shared parser machinery.
//!
//! [`LanguageParser`] is the single contract every documentation frontend
//! implements: it lowers a language's raw doc artifact (rustdoc `Crate`,
//! deno-doc documents, …) into the shared [`Entry`] IR. The free helpers below
//! were byte-identical across the Rust and TypeScript parsers and now live here
//! once.

use ir::{kind::Entry, parameter::{LiteralParameter, Parameter}, ty::Type};
use lang_types::Language;

use crate::pipeline::{Collected, Ir};

/// A documentation frontend that lowers one language's raw doc artifact into the
/// shared [`Entry`] IR.
///
/// Implementors keep their language-specific construction (`from_doc`) and walk
/// (`parse`) private to their module; callers go through [`lower`](Self::lower)
/// to get a typestate [`Ir<Collected>`] in one step, language-agnostically.
pub trait LanguageParser: Sized {
	/// The raw documentation artifact this parser consumes.
	type Input;

	/// The parser's error type. Bounded so callers can box or wrap it uniformly.
	type Error: std::error::Error + Send + Sync + 'static;

	/// The language this parser produces IR for.
	const LANGUAGE: Language;

	/// Build a parser from a raw documentation artifact.
	fn from_doc(input: Self::Input) -> Result<Self, Self::Error>;

	/// Lower the loaded documentation into a flat list of IR entries.
	fn parse(&mut self) -> Result<Vec<Entry>, Self::Error>;

	/// One-shot: construct, parse, and wrap in the typestate IR. This is the
	/// language-agnostic entry point the ingestion pipeline should prefer.
	fn lower(input: Self::Input) -> Result<Ir<Collected>, Self::Error> {
		let mut parser = Self::from_doc(input)?;
		Ok(Ir::from_entries(parser.parse()?))
	}
}

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
