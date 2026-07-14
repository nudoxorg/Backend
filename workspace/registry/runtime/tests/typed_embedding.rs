//! Real (passing) tests for the model-branded embedding type — the model (and
//! thus dimension) is carried in the type, so a wrong-length vector cannot be
//! built and two different models' vectors are distinct, incomparable types.

use runtime::vector::{
	embedding::Embedding,
	model::{E5Small, EmbeddingModel, ModelId, OpenAi3Small},
};

/// An embedding knows its dimension from its brand, and validates length at the
/// construction boundary.
#[test]
fn embedding_carries_its_model_dimension() {
	assert_eq!(OpenAi3Small::DIMENSIONS, 1536);
	let e = Embedding::<OpenAi3Small>::from_vec(vec![0.0; 1536]).expect("correct length");
	assert_eq!(e.as_slice().len(), 1536);
	assert_eq!(Embedding::<OpenAi3Small>::dimensions(), 1536);
}

/// A wrong-length slice is rejected at construction rather than producing a
/// malformed vector.
#[test]
fn wrong_length_is_rejected() {
	let bad = Embedding::<OpenAi3Small>::from_slice(&[0.0; 10]);
	assert!(bad.is_err(), "a 10-long slice is not a valid OpenAi3Small embedding");
}

/// Two different models are *different types* — there is no runtime path on
/// which their vectors could be confused, even though both are valid embeddings.
/// (Uncommenting the last line will not compile.)
#[test]
fn different_models_are_different_types() {
	let small = Embedding::<OpenAi3Small>::zeroed();
	let tiny = Embedding::<E5Small>::zeroed();
	assert_eq!(small.as_slice().len(), 1536);
	assert_eq!(tiny.as_slice().len(), 384);
	// let _mismatch: Embedding<OpenAi3Small> = tiny; // <- type error
}

/// A blank model id is unconstructible (the qdrant collection key can never be
/// empty).
#[test]
fn model_id_rejects_blank() {
	assert!(ModelId::try_new("   ").is_err());
	assert!(ModelId::try_new("").is_err());
	assert!(ModelId::try_new("text-embedding-3-small").is_ok());
}
