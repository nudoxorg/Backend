//! Generate — the "runs computer" half of the indexing flow: take a materialized
//! package and produce the resolutions that become a blob and the graph
//! documents.
//!
//! - [`surface`]: the API surface (`ir::Index`), lowered by `crate::languages`;
//! - [`cst`]: the concrete-syntax-tree resolution (tree-sitter) as *serializable*
//!   resolved-reference spans (the live tree is transient — see [`cst`]);
//! - [`source_archive`] + [`tar`]: the condensed, content-addressed source;
//! - [`blob_info`]: the canonical [`heart::Generation`] + per-file digests the
//!   registry blob layer consumes;
//! - [`linked_data`]: streamed JSON-LD graph documents for the graph store.
//!
//! The output is aligned with the registry's content-addressed blob model: the
//! same per-file [`heart::ContentHash`]es and the canonical [`heart::Generation`]
//! the registry keys storage and freshness on.

pub mod blob_info;
pub mod cst;
pub mod linked_data;
pub mod parse_cache;
pub mod source_archive;
pub mod surface;
pub mod tar;

use std::path::PathBuf;

use heart::{ContentHash, Toolchain};
use registry::identity::PackageCoordinates;
use ir::entry::Index;

pub use blob_info::BlobInfo;
pub use cst::CstSet;
pub use source_archive::{FileDigest, SourceArchive};

use crate::error::GenerateError;

/// A materialized package ready to generate from: its verified identity, the
/// toolchain it was resolved against, and the root of its extracted source.
pub struct PackageInput {
	/// The package's canonical coordinates (origin × name × version).
	pub coordinates: PackageCoordinates,
	/// The toolchain the source was resolved/analyzed against.
	pub toolchain: Toolchain,
	/// The root directory of the (already sanitized, extracted) source tree.
	pub root: PathBuf,
}

/// All generated resolutions for one package, plus the canonical generation hash
/// that ties them to the registry's content-addressed storage.
pub struct GeneratedPackage {
	/// The package identity these resolutions describe.
	pub coordinates: PackageCoordinates,
	/// The toolchain used.
	pub toolchain: Toolchain,
	/// The API surface.
	pub surface: Index,
	/// The per-file CST resolution (serializable reference spans).
	pub cst: CstSet,
	/// The content-addressed source archive.
	pub archive: SourceArchive,
	/// The assembled, sink-ready blob info (file digests + snapshot hash).
	pub blob_info: BlobInfo,
	/// The canonical content hash of this package snapshot.
	pub snapshot: ContentHash,
}

/// Run the full generation pipeline over a materialized package: lower the
/// surface, extract CSTs, build the content-addressed archive, assemble the blob
/// info, and compute the canonical generation.
///
/// CPU-bound and deterministic — the caller runs this on a blocking pool /
/// worker, never on an async serving thread.
///
/// Re-indexing the same `(producer, toolchain, source, lock)` is a content-
/// addressed cache hit (design §10), not a re-parse.
pub fn generate(input: &PackageInput) -> Result<GeneratedPackage, GenerateError> {
	let source_hash = parse_cache::hash_source_tree(&input.root).unwrap_or_else(|_| {
		ContentHash::of_bytes(input.root.to_string_lossy().as_bytes())
	});
	let dep_lock = parse_cache::hash_dep_lock(&input.root);
	let cache_key = parse_cache::key(&input.toolchain, source_hash, dep_lock);

	let surface = if let Some(bytes) = parse_cache::get(&cache_key) {
		tracing::debug!(key = %cache_key.hex(), "parse cache hit");
		serde_json::from_slice(&bytes).map_err(|e| {
			GenerateError::Archive(std::io::Error::new(
				std::io::ErrorKind::InvalidData,
				format!("cached IR: {e}"),
			))
		})?
	} else {
		let surface = surface::build(input)?;
		if let Ok(bytes) = serde_json::to_vec(&surface) {
			parse_cache::put(cache_key, bytes);
		}
		surface
	};

	let cst = cst::extract(input)?;
	let archive = source_archive::build(input)?;

	// BlobInfo owns its archive copy (it travels to the sink independently of
	// the GeneratedPackage), so the archive is cloned rather than split.
	let blob_info = BlobInfo::assemble(&surface, &cst, archive.clone());
	let snapshot = blob_info.snapshot;

	Ok(GeneratedPackage {
		coordinates: input.coordinates.clone(),
		toolchain: input.toolchain.clone(),
		surface,
		cst,
		archive,
		blob_info,
		snapshot,
	})
}
