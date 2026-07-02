//! Real (passing) tests for the model-branded embedding type — the model (and
//! thus dimension) is carried in the type, so a wrong-length vector cannot be
//! built and two different models' vectors are distinct, incomparable types.

use runtime::vector::embedding::{
	Embedding, EmbeddingModel,
	models::{OpenAi3Large, OpenAi3Small},
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
	let large = Embedding::<OpenAi3Large>::zeroed();
	assert_eq!(small.as_slice().len(), 1536);
	assert_eq!(large.as_slice().len(), 3072);
	// let _mismatch: Embedding<OpenAi3Small> = large; // <- type error
}
