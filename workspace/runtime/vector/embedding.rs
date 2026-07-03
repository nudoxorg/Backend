//! Model-branded embedding vectors and the (fallible, batched) embedder that
//! produces them.
//!
//! The vector's *model* is lifted into the type via [`EmbeddingModel`], not just
//! its numeric dimension. So an `Embedding<OpenAi3Large>` and an
//! `Embedding<Qwen3>` are different types even if their dimensions happened to
//! match — feeding one model's vectors to another model's store is a *compile*
//! error, not a silent semantic bug. The brand also supplies the dimension
//! (`M::DIMENSIONS`) and the stable [`ModelId`] (`M::id()`), so those never have
//! to be threaded or re-stated.
//!
//! The model brand/catalog/[`ModelId`] themselves live in [`super::model`]; this
//! file is only the embedding *value* type and the *embedder* interface.
//!
//! Storage is a validated `Arc<[f32]>`: length is checked once, at the
//! construction boundary, against `M::DIMENSIONS`. This drops the fixed-array
//! `serde_arrays` hack and removes any need for `generic_const_exprs`.

use std::{marker::PhantomData, sync::Arc};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::model::{EmbeddingModel, ModelId};
use crate::error::EmbedError;

/// What a piece of text is being embedded *as*. The same text embedded under two
/// purposes yields two distinct vectors, so code-vs-doc search stay separable in
/// one collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EmbeddingPurpose {
	/// Embedding the code/API surface of a symbol.
	Code,
	/// Embedding the documentation/prose around a code object (like this).
	Documentation,
}

/// A model-branded embedding vector. The brand `M` fixes both the dimension and
/// the producing model at the type level; the values are length-validated
/// against `M::DIMENSIONS` at construction.
pub struct Embedding<M: EmbeddingModel> {
	values: Arc<[f32]>,
	_model: PhantomData<fn() -> M>,
}

impl<M: EmbeddingModel> Embedding<M> {
	/// Compile-time `DIMENSIONS > 0` guard, forced by touching it in every
	/// constructor — a zero-dimension model fails to compile rather than to run.
	const NON_ZERO: () =
		assert!(M::DIMENSIONS > 0, "an embedding model must have at least one dimension");

	/// Build from a runtime slice, validating it is exactly `M::DIMENSIONS` long.
	/// This is the boundary where a model's returned vector is checked.
	pub fn from_slice(values: &[f32]) -> Result<Self, EmbedError> {
		let () = Self::NON_ZERO;
		if values.len() != M::DIMENSIONS {
			return Err(EmbedError::LengthMismatch { expected: M::DIMENSIONS, found: values.len() });
		}
		Ok(Self { values: Arc::from(values), _model: PhantomData })
	}

	/// Build from an owned vector, validating its length against `M::DIMENSIONS`.
	pub fn from_vec(values: Vec<f32>) -> Result<Self, EmbedError> {
		let () = Self::NON_ZERO;
		if values.len() != M::DIMENSIONS {
			return Err(EmbedError::LengthMismatch { expected: M::DIMENSIONS, found: values.len() });
		}
		Ok(Self { values: Arc::from(values), _model: PhantomData })
	}

	/// The all-zero vector — a valid neutral element for accumulation.
	pub fn zeroed() -> Self {
		let () = Self::NON_ZERO;
		Self { values: Arc::from(vec![0.0f32; M::DIMENSIONS]), _model: PhantomData }
	}

	/// The raw values.
	pub fn as_slice(&self) -> &[f32] { &self.values }

	/// The dimension, from the brand (no instance needed).
	pub const fn dimensions() -> usize { M::DIMENSIONS }

	/// The id of the model that produced this vector.
	pub fn model_id() -> ModelId { M::id() }
}

// Hand-written impls so `Embedding<M>` does not spuriously require `M: Clone`
// etc. (the brand is a zero-sized marker; the data is a cheap-to-clone `Arc`).
impl<M: EmbeddingModel> Clone for Embedding<M> {
	fn clone(&self) -> Self { Self { values: Arc::clone(&self.values), _model: PhantomData } }
}
impl<M: EmbeddingModel> std::fmt::Debug for Embedding<M> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Embedding")
			.field("model", &M::id())
			.field("dimensions", &M::DIMENSIONS)
			.finish_non_exhaustive()
	}
}
impl<M: EmbeddingModel> PartialEq for Embedding<M> {
	fn eq(&self, other: &Self) -> bool { self.values == other.values }
}

impl<M: EmbeddingModel> Serialize for Embedding<M> {
	fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
		self.values.as_ref().serialize(s)
	}
}
impl<'de, M: EmbeddingModel> Deserialize<'de> for Embedding<M> {
	fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
		let values = Vec::<f32>::deserialize(d)?;
		Embedding::from_vec(values).map_err(serde::de::Error::custom)
	}
}

/// Anything that turns source/doc text into an embedding under a specific model.
/// The model is an associated brand, so an embedder's output can only feed a
/// store of the *same* model — a mismatch is a compile error.
///
/// Embedding is **fallible and batched**: a network model can fail, time out, or
/// rate-limit, and batching amortizes the round-trip over many symbols.
#[diagnostic::on_unimplemented(
	message = "`{Self}` is not an `Embedder`",
	note = "implement `Embedder` with an `EmbeddingModel` brand; its output feeds only a store of the same model"
)]
pub trait Embedder: Send + Sync {
	/// The model brand this embedder produces vectors for.
	type Model: EmbeddingModel;

	/// The model id (matches `Self::Model::id()`); stamped on stored vectors.
	fn model(&self) -> &ModelId;

	/// Embed a single piece of `text` for the given `purpose`.
	async fn embed(
		&self,
		text: &str,
		purpose: EmbeddingPurpose,
	) -> Result<Embedding<Self::Model>, EmbedError>;

	/// Embed a batch in one round-trip; the result is positionally aligned with
	/// `texts`. An all-or-nothing failure is reported via `Err`.
	async fn embed_batch(
		&self,
		texts: &[&str],
		purpose: EmbeddingPurpose,
	) -> Result<Vec<Embedding<Self::Model>>, EmbedError>;
}

/// Assert an embedder's futures are `Send` (so it composes into a multi-threaded
/// server) without boxing — via `return_type_notation` bounds, mirroring the
/// graph/vector store guards. Keeps the trait signatures plain `async fn`.
pub fn assert_embedder_futures_send<E>()
where
	E: Embedder<embed(..): Send, embed_batch(..): Send>,
{
}
