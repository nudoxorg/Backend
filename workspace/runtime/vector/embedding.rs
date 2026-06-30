//! I figure we don't need a special embedding type -- it seems a nonempty vec
//! would do all of the heavy lifting we need it to do.
//!
//! ...except dimensionality. By lifting the dimension `DIM` into the type, an
//! embedder and the store it feeds can never disagree on vector length — the
//! whole "vector dim mismatch" runtime error family becomes a compile error.

use std::future::Future;

use heart::ModelId;

pub enum EmbeddingPurpose {
	/// We're creating embeddings based on the code/surface
	Code,
	/// We're creating embeddings based on the documentation around a code object
	/// (like this right here)
	Documentation,
}

/// A fixed-dimension embedding vector. `DIM` is part of the type and is
/// asserted to be non-zero at construction, so an `Embedding<0>` is
/// unrepresentable and a wrong-length vector cannot be built.
pub struct Embedding<const DIM: usize>([f32; DIM]);

/// Compile-time `DIM > 0` check
struct DimCheck<const D: usize>;
impl<const D: usize> DimCheck<D> {
	const NON_ZERO: () = assert!(D > 0, "an embedding must have at least one dimension");
}

impl<const DIM: usize> Embedding<DIM> {
	/// Wrap a `DIM`-long array as an embedding.
	pub fn new(values: [f32; DIM]) -> Self {
		let () = DimCheck::<DIM>::NON_ZERO;
		Self(values)
	}

	pub fn as_slice(&self) -> &[f32] { &self.0 }

	pub const fn dimensions(&self) -> usize { DIM }

	/// Concatenate two embeddings. This is rather absurdly done on the type level
	/// !
	pub fn concat<const OTHER: usize>(self, other: Embedding<OTHER>) -> Embedding<{ DIM + OTHER }>
	where
		[(); DIM + OTHER]:,
	{
		let mut values = [0.0f32; DIM + OTHER];
		values[..DIM].copy_from_slice(&self.0);
		values[DIM..].copy_from_slice(&other.0);
		Embedding::new(values)
	}
}

/// Anything that turns a piece of source/doc text into a `DIM`-dimensional
/// embedding. The dimension is on the trait, so an `Embedder<256>` cannot be
/// handed to a store expecting `Embedding<1536>`.
#[diagnostic::on_unimplemented(
	message = "`{Self}` is not an `Embedder<{DIM}>`",
	label = "cannot embed into {DIM} dimensions",
	note = "the embedder's output dimension must match the vector store's `DIM`"
)]
pub trait Embedder<const DIM: usize>: Send + Sync {
	/// The model these embeddings were produced by.
	fn model(&self) -> &ModelId;

	/// Embed `text` for the given purpose.
	fn embed(
		&self,
		text: &str,
		purpose: EmbeddingPurpose,
	) -> impl Future<Output = Embedding<DIM>> + Send;
}
