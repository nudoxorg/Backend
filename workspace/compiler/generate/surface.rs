//! Generating the API surface resolution (the `ir::Index`) for a package.
//!
//! Dispatches on the package's ecosystem to the matching `crate::languages`
//! backend, which drives that language's documentation/type oracle and lowers it
//! into the shared IR.

use heart::Language;
use ir::{entry::Index, pipeline::{Collected, Ir}};

use crate::{error::GenerateError, generate::PackageInput};

/// Lower a package's source into the collected IR, dispatching on ecosystem.
pub fn collect(input: &PackageInput) -> Result<Ir<Collected>, GenerateError> {
	match input.coordinates.ecosystem() {
		Language::Python => todo!("drive crate::languages::python lowering"),
		Language::Rust => todo!("drive crate::languages::rust lowering"),
		Language::Typescript => todo!("drive crate::languages::typescript lowering"),
	}
}

/// Lower a package's source into the indexed API surface (the `ir::Index`).
pub fn build(input: &PackageInput) -> Result<Index, GenerateError> {
	Ok(collect(input)?.index().into_index())
}
