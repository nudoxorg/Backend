//! Embedding *models*: the sealed brand trait, the concrete catalog of brands,
//! and the [`ModelId`]. Split into its own folder so "what models exist" is one
//! place, distinct from the embedding *vector* value type and the embedder
//! interface (which live in [`super::embedding`]).

pub mod catalog;
pub mod id;

pub use catalog::{E5Small, OpenAi3Small};
pub use id::ModelId;

mod sealed {
	pub trait Sealed {}
}

/// A compile-time embedding-model brand: which model produced a vector, its
/// dimensionality, and its stable id. Sealed — only the known models in
/// [`catalog`] exist, so a brand always corresponds to a real, configured model.
pub trait EmbeddingModel: sealed::Sealed + Send + Sync + 'static {
	/// The model's output dimensionality.
	const DIMENSIONS: usize;

	/// The stable model id — stamped alongside every stored vector and used as
	/// the migration key when the model changes.
	fn id() -> ModelId;
}
