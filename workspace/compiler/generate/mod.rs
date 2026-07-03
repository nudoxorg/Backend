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
pub mod source_archive;
pub mod surface;
pub mod tar;

use std::path::PathBuf;

use heart::{ContentHash, PackageCoordinates, Toolchain};
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
pub fn generate(input: &PackageInput) -> Result<GeneratedPackage, GenerateError> {
	let _ = input;
	todo!("surface::build -> cst::extract -> source_archive::build -> blob_info::assemble")
}
