//! Generating the API surface resolution (the `ir::Index`) for a package.
//!
//! One dispatch path for all six languages (DAEMON-PLAN §2.3):
//!
//! ```text
//! PackageInput → seal → run_producer (cas.get → cage/worker → cas.put)
//! ```

use heart::{Language, PackageVersion, RegistryOrigin};
use ir::{
	entry::Index,
	pipeline::{Collected, Ir},
};

use crate::compile::producer::{self, ForgeContext, Producer, SandboxKey};
use crate::{
	error::GenerateError,
	generate::{PackageInput, parse_cache},
	languages::{
		go::GoProducer, java::JavaProducer, nix::NixProducer, python::PythonProducer,
		rust::RustProducer, typescript::TypescriptProducer,
	},
};

/// Lower a package's source into the collected IR, dispatching on ecosystem.
pub fn collect<C: ForgeContext>(
	ctx: &C,
	input: &PackageInput,
) -> Result<Ir<Collected>, GenerateError> {
	let out = run_producer(ctx, input)?;
	Ok(Ir::from_entries(
		out.index.entries_by_path.into_values().collect(),
	))
}

/// Lower a package's source into the indexed API surface (the `ir::Index`).
pub fn build<C: ForgeContext>(ctx: &C, input: &PackageInput) -> Result<Index, GenerateError> {
	Ok(collect(ctx, input)?.index().into_index())
}

/// The per-package [`SandboxKey`] for override resolution.
fn package_key(input: &PackageInput) -> SandboxKey {
	SandboxKey::package(
		input.coordinates.origin.token().into_owned(),
		input.coordinates.name.original().to_string(),
		input.coordinates.version.canonical(),
	)
}

/// Seal + run the producer for the package's language — the only surface dispatch.
///
/// [`producer::run_producer`] is the sole cache client: the sealed input's job
/// key drives `ctx.cas().get` (hit → decoded [`ProducerOutput`]) / miss (run,
/// then `put`). Scratch lives in [`producer::SealedPackage`] (RAII).
fn run_producer<C: ForgeContext>(
	ctx: &C,
	input: &PackageInput,
) -> Result<producer::ProducerOutput, GenerateError> {
	let source_hash = parse_cache::hash_source_tree(&input.root)
		.unwrap_or_else(|_| heart::ContentHash::of_bytes(input.root.to_string_lossy().as_bytes()));
	let dep_lock = parse_cache::hash_dep_lock(&input.root);
	let package = package_key(input);

	macro_rules! seal_and_run {
		($p:expr) => {{
			let p = $p;
			let sealed = producer::seal_package(
				ctx.toolchains(),
				ctx.overrides(),
				&input.root,
				p.profile(),
				p.tier(),
				Some(&package),
				source_hash,
				dep_lock,
			)?;
			producer::run_producer(ctx, &p, sealed.input()).map_err(GenerateError::from)
		}};
	}

	match input.coordinates.ecosystem() {
		Language::Rust => {
			let PackageVersion::Cargo(version) = &input.coordinates.version else {
				return Err(GenerateError::UnsupportedRust);
			};
			let direct_repo = matches!(input.coordinates.origin, RegistryOrigin::Custom { .. });
			seal_and_run!(RustProducer {
				name: input.coordinates.name.original().to_string(),
				version: version.clone(),
				direct_repo,
			})
		}
		Language::Go => seal_and_run!(GoProducer),
		Language::Java => seal_and_run!(JavaProducer),
		Language::Python => seal_and_run!(PythonProducer),
		Language::Typescript => {
			let PackageVersion::Npm(_) = &input.coordinates.version else {
				return Err(GenerateError::UnsupportedTypescript);
			};
			seal_and_run!(TypescriptProducer {
				name: input.coordinates.name.original().to_string(),
			})
		}
		Language::Nix => seal_and_run!(NixProducer),
	}
}
