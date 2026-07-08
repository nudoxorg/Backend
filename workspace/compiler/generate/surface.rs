//! Generating the API surface resolution (the `ir::Index`) for a package.
//!
//! Dispatches on the package's ecosystem to the matching `crate::languages`
//! backend, which drives that language's documentation/type oracle and lowers it
//! into the shared IR.

use heart::{Language, PackageVersion, RegistryOrigin};
use ir::{entry::Index, pipeline::{Collected, Ir}};

use crate::{error::GenerateError, generate::PackageInput, languages};

/// Lower a package's source into the collected IR, dispatching on ecosystem.
pub fn collect(input: &PackageInput) -> Result<Ir<Collected>, GenerateError> {
	match input.coordinates.ecosystem() {
		Language::Python => {
			let context = languages::python::context::PythonContext::new();
			let index = context.lower_package(&input.root);
			// lower_package indexes eagerly; re-enter the typestate at
			// Collected so build() owns the (idempotent) indexing step.
			Ok(Ir::from_entries(index.entries_by_path.into_values().collect()))
		}
		Language::Rust => {
			let PackageVersion::Cargo(version) = &input.coordinates.version else {
				// Coordinates are ecosystem-validated at construction; a Rust
				// package always carries a Cargo version.
				return Err(GenerateError::UnsupportedRust);
			};
			// Registry tarballs document the public surface; direct repos
			// (custom origins) also get the private-items + workspace pass.
			let direct_repo = matches!(input.coordinates.origin, RegistryOrigin::Custom { .. });
			let package = languages::rust::RustPackage {
				name: input.coordinates.name.original().to_string(),
				direct_repo,
			};
			let (collected, _sources) = package
				.generate_ir_with_sources(&input.root, version)
				.map_err(GenerateError::from)?;
			Ok(collected)
		}
		Language::Typescript => {
			let PackageVersion::Npm(_version) = &input.coordinates.version else {
				// Coordinates are ecosystem-validated at construction; a
				// TypeScript package always carries an npm version.
				return Err(GenerateError::UnsupportedTypescript);
			};
			// The lowering documents the materialized root as-is; the version
			// only selects which root gets materialized upstream.
			let package = languages::typescript::TypescriptPackage {
				name: input.coordinates.name.original().to_string(),
			};
			// Direct ? works because GenerateError::LowerTypescript(#[from] languages::typescript::Package)
			// and the TS Package error now contains the full exploded taxonomy (Ts* subs + PathBufs + sources).
			Ok(package.generate_ir(&input.root)?)
		}
		Language::Go => {
			let index = languages::go::lower_package(&input.root)?;
			Ok(Ir::from_entries(index.entries_by_path.into_values().collect()))
		}
		Language::Java => {
			let index = languages::java::package::lower_package(&input.root)?;
			Ok(Ir::from_entries(index.entries_by_path.into_values().collect()))
		}
	}
}

/// Lower a package's source into the indexed API surface (the `ir::Index`).
pub fn build(input: &PackageInput) -> Result<Index, GenerateError> {
	Ok(collect(input)?.index().into_index())
}
