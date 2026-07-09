//! Generating the API surface resolution (the `ir::Index`) for a package.
//!
//! Dispatches on the package's ecosystem to the matching `crate::languages`
//! backend, which drives that language's documentation/type oracle and lowers it
//! into the shared IR.
//!
//! Interpreters (nix / typescript / python) prefer the pooled worker path so a
//! parser crash cannot take down the indexer (design §7.4–7.6 / P3).

use heart::{Language, PackageVersion, RegistryOrigin};
use ir::{
	entry::Index,
	pipeline::{Collected, Ir},
};
use sandbox::worker::WorkerLang;

use crate::{
	error::GenerateError,
	generate::PackageInput,
	languages::{self, isolate},
};

/// Lower a package's source into the collected IR, dispatching on ecosystem.
pub fn collect(input: &PackageInput) -> Result<Ir<Collected>, GenerateError> {
	match input.coordinates.ecosystem() {
		Language::Python => {
			if let Some(body) = worker_index(WorkerLang::Python, &input.root)? {
				return Ok(Ir::from_entries(body.entries_by_path.into_values().collect()));
			}
			let context = languages::python::context::PythonContext::new();
			let index = context.lower_package(&input.root);
			Ok(Ir::from_entries(index.entries_by_path.into_values().collect()))
		}
		Language::Rust => {
			let PackageVersion::Cargo(version) = &input.coordinates.version else {
				return Err(GenerateError::UnsupportedRust);
			};
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
				return Err(GenerateError::UnsupportedTypescript);
			};
			if let Some(body) = worker_index(WorkerLang::Typescript, &input.root)? {
				return Ok(Ir::from_entries(body.entries_by_path.into_values().collect()));
			}
			let package = languages::typescript::TypescriptPackage {
				name: input.coordinates.name.original().to_string(),
			};
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
		Language::Nix => {
			if let Some(body) = worker_index(WorkerLang::Nix, &input.root)? {
				return Ok(Ir::from_entries(body.entries_by_path.into_values().collect()));
			}
			let index = languages::nix::lower_package(&input.root)?;
			Ok(Ir::from_entries(index.entries_by_path.into_values().collect()))
		}
	}
}

/// Lower a package's source into the indexed API surface (the `ir::Index`).
pub fn build(input: &PackageInput) -> Result<Index, GenerateError> {
	Ok(collect(input)?.index().into_index())
}

fn worker_index(lang: WorkerLang, root: &std::path::Path) -> Result<Option<Index>, GenerateError> {
	match isolate::try_worker_lower(lang, root) {
		None => Ok(None),
		Some(Ok(body)) => {
			let index: Index = serde_json::from_str(&body).map_err(|e| {
				GenerateError::Archive(std::io::Error::new(
					std::io::ErrorKind::InvalidData,
					e.to_string(),
				))
			})?;
			Ok(Some(index))
		}
		Some(Err(e)) => Err(GenerateError::Archive(std::io::Error::new(
			std::io::ErrorKind::Other,
			e.to_string(),
		))),
	}
}
