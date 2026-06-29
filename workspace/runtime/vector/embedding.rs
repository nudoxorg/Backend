//! I figure we don't need a special embedding type -- it seems a nonempty vec
//! would do all of the heavy lifting we need it to do.

use nonempty::NonEmpty;

pub enum EmbeddingPurpose {
	/// We're creating embeddings based on the code/surface
	Code,
	/// We're creating embeddings based on the documentation around a code object
	/// (like this right here)
	Documentation,
}

pub type Embedding = NonEmpty<f32>;

/// Not worth keeping this as an enum, as they're proprietary for the embedding
/// model
const EMBEDDING_MODEL: &'static str = "text-embedding-3-small";

// Embedders that turn a source chunk into a vector for the semantic store.
//
// Backs both ingest-time index embeddings and query-time embeddings, behind a
// single trait so the in-process, remote (OpenAI-compatible), placeholder, and
// mock implementations are interchangeable.

