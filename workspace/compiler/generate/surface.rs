//! Generating the API surface resolution (the `ir::Index`) for a package.
//!
//! One dispatch path for all six languages (DAEMON-PLAN §2.3):
//!
//! ```text
//! PackageInput → seal → Producer::produce (plan → cage/worker → decode)
//! ```

use heart::{Language, PackageVersion, RegistryOrigin};
use ir::{
	entry::Index,
	pipeline::{Collected, Ir},
};
use sandbox::ProducerProfile;

use crate::languages::producer::Producer;
use crate::{
	error::GenerateError,
	generate::PackageInput,
	languages::{
		go::GoProducer, java::JavaProducer, nix::NixProducer, producer, python::PythonProducer,
		rust::RustProducer, typescript::TypescriptProducer,
	},
};

/// Lower a package's source into the collected IR, dispatching on ecosystem.
pub fn collect(input: &PackageInput) -> Result<Ir<Collected>, GenerateError> {
	let out = run_producer(input)?;
	Ok(Ir::from_entries(
		out.index.entries_by_path.into_values().collect(),
	))
}

/// Lower a package's source into the indexed API surface (the `ir::Index`).
pub fn build(input: &PackageInput) -> Result<Index, GenerateError> {
	Ok(collect(input)?.index().into_index())
}

/// Seal + produce for the package's language — the only surface dispatch.
fn run_producer(input: &PackageInput) -> Result<producer::ProducerOutput, GenerateError> {
	match input.coordinates.ecosystem() {
		Language::Rust => {
			let PackageVersion::Cargo(version) = &input.coordinates.version else {
				return Err(GenerateError::UnsupportedRust);
			};
			let direct_repo = matches!(input.coordinates.origin, RegistryOrigin::Custom { .. });
			let p = RustProducer {
				name: input.coordinates.name.original().to_string(),
				version: version.clone(),
				direct_repo,
			};
			let sealed = producer::seal_package(&input.root, ProducerProfile::Rust);
			p.produce(&sealed).map_err(GenerateError::from)
		}
		Language::Go => {
			let p = GoProducer;
			let sealed = producer::seal_package(&input.root, ProducerProfile::Go);
			p.produce(&sealed).map_err(GenerateError::from)
		}
		Language::Java => {
			let p = JavaProducer;
			let sealed = producer::seal_package(&input.root, ProducerProfile::Java);
			p.produce(&sealed).map_err(GenerateError::from)
		}
		Language::Python => {
			let p = PythonProducer;
			let sealed = producer::seal_package(&input.root, p.tier().profile());
			p.produce(&sealed).map_err(GenerateError::from)
		}
		Language::Typescript => {
			let PackageVersion::Npm(_) = &input.coordinates.version else {
				return Err(GenerateError::UnsupportedTypescript);
			};
			let p = TypescriptProducer {
				name: input.coordinates.name.original().to_string(),
			};
			let sealed = producer::seal_package(&input.root, p.tier().profile());
			p.produce(&sealed).map_err(GenerateError::from)
		}
		Language::Nix => {
			let p = NixProducer;
			let sealed = producer::seal_package(&input.root, ProducerProfile::Nix);
			p.produce(&sealed).map_err(GenerateError::from)
		}
	}
}
