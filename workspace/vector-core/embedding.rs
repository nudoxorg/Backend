//! The model-branded embedding value type.
//!
//! Mirrors `registry::runtime::vector::embedding` in shape: the brand `M`
//! carries dimension + model at the type level, storage is a validated
//! `Arc<[f32]>`, and the validation boundary is construction (I11: a vector of
//! the wrong model/dimension cannot exist as a typed value).

use std::{marker::PhantomData, sync::Arc};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::model::EmbeddingModel;

/// Embedding failures — construction validation plus backend transport.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmbedError {
	/// The vector's length does not match `M::DIMENSIONS` (I11).
	#[error("embedding dimension mismatch: expected {expected}, got {got}")]
	DimensionMismatch { expected: usize, got: usize },

	/// The vector contains a NaN or infinity — poisoned similarity math.
	#[error("embedding contains a non-finite component")]
	NonFinite,

	/// The producing backend (ORT session, HTTP API) failed.
	#[error("embedding backend failure: {0}")]
	Backend(String),

	/// The input text exceeded the model's sequence budget and the caller
	/// forbade truncation.
	#[error("input of {tokens} tokens exceeds the model window of {max_seq_len}")]
	InputTooLong { tokens: usize, max_seq_len: usize },
}

/// A model-branded embedding vector: exactly `M::DIMENSIONS` finite floats.
pub struct Embedding<M: EmbeddingModel> {
	values: Arc<[f32]>,
	_model: PhantomData<fn() -> M>,
}

impl<M: EmbeddingModel> Embedding<M> {
	/// Build from an owned vector, validating exact length *and* finiteness —
	/// the single boundary where a model's raw output becomes typed (I11).
	pub fn from_vec(values: Vec<f32>) -> Result<Self, EmbedError> {
		if values.len() != M::DIMENSIONS {
			return Err(EmbedError::DimensionMismatch {
				expected: M::DIMENSIONS,
				got: values.len(),
			});
		}
		if !values.iter().all(|v| v.is_finite()) {
			return Err(EmbedError::NonFinite);
		}
		Ok(Self { values: Arc::from(values), _model: PhantomData })
	}

	/// The raw components.
	pub fn as_slice(&self) -> &[f32] { &self.values }

	/// The dimension, from the brand.
	pub const fn dimensions() -> usize { M::DIMENSIONS }

	/// L2-normalize to unit length, so cosine reduces to dot product at the
	/// store (both canonical models are cosine-metric — `M::METRIC`). The
	/// zero vector is returned unchanged (it has no direction).
	pub fn l2_normalize(self) -> Self {
		let norm = self.values.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>().sqrt();
		if norm == 0.0 {
			return self;
		}
		let scaled: Vec<f32> = self.values.iter().map(|&v| (f64::from(v) / norm) as f32).collect();
		Self { values: Arc::from(scaled), _model: PhantomData }
	}

	/// Cosine similarity, accumulated in `f64` for determinism across planes
	/// (matches the registry `similarity.rs` convention). Either vector being
	/// zero yields `0.0`.
	pub fn cosine(&self, other: &Self) -> f32 {
		let mut dot = 0.0f64;
		let mut na = 0.0f64;
		let mut nb = 0.0f64;
		for (&a, &b) in self.values.iter().zip(other.values.iter()) {
			dot += f64::from(a) * f64::from(b);
			na += f64::from(a) * f64::from(a);
			nb += f64::from(b) * f64::from(b);
		}
		let denom = na.sqrt() * nb.sqrt();
		if denom == 0.0 { 0.0 } else { (dot / denom) as f32 }
	}
}

impl<M: EmbeddingModel> AsRef<[f32]> for Embedding<M> {
	fn as_ref(&self) -> &[f32] { &self.values }
}

/// In-place L2 normalization of a raw component slice — the untyped twin of
/// [`Embedding::l2_normalize`] for backends that produce vectors before the
/// typed boundary. The zero vector is left unchanged.
pub fn l2_normalize(values: &mut [f32]) {
	let norm = values.iter().map(|&v| f64::from(v) * f64::from(v)).sum::<f64>().sqrt();
	if norm == 0.0 {
		return;
	}
	for v in values {
		*v = (f64::from(*v) / norm) as f32;
	}
}

// Hand-written impls so `Embedding<M>` never requires bounds on the brand.
impl<M: EmbeddingModel> Clone for Embedding<M> {
	fn clone(&self) -> Self { Self { values: Arc::clone(&self.values), _model: PhantomData } }
}
impl<M: EmbeddingModel> PartialEq for Embedding<M> {
	fn eq(&self, other: &Self) -> bool { self.values == other.values }
}
impl<M: EmbeddingModel> std::fmt::Debug for Embedding<M> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Embedding")
			.field("model", &M::id())
			.field("dimensions", &M::DIMENSIONS)
			.finish_non_exhaustive()
	}
}

impl<M: EmbeddingModel> Serialize for Embedding<M> {
	fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
		self.values.as_ref().serialize(s)
	}
}
impl<'de, M: EmbeddingModel> Deserialize<'de> for Embedding<M> {
	fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
		Embedding::from_vec(Vec::<f32>::deserialize(d)?).map_err(serde::de::Error::custom)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::model::JinaCodeV2;

	fn unit(index: usize) -> Embedding<JinaCodeV2> {
		let mut v = vec![0.0f32; 768];
		v[index] = 1.0;
		Embedding::from_vec(v).unwrap()
	}

	#[test]
	fn from_vec_rejects_wrong_dimension() {
		let err = Embedding::<JinaCodeV2>::from_vec(vec![0.0; 767]).unwrap_err();
		assert_eq!(err, EmbedError::DimensionMismatch { expected: 768, got: 767 });
	}

	#[test]
	fn from_vec_rejects_non_finite() {
		let mut v = vec![0.0f32; 768];
		v[3] = f32::NAN;
		assert_eq!(Embedding::<JinaCodeV2>::from_vec(v).unwrap_err(), EmbedError::NonFinite);
		let mut v = vec![0.0f32; 768];
		v[0] = f32::INFINITY;
		assert_eq!(Embedding::<JinaCodeV2>::from_vec(v).unwrap_err(), EmbedError::NonFinite);
	}

	#[test]
	fn normalize_yields_unit_length_and_keeps_zero() {
		let mut v = vec![0.0f32; 768];
		v[0] = 3.0;
		v[1] = 4.0;
		let n = Embedding::<JinaCodeV2>::from_vec(v).unwrap().l2_normalize();
		let norm: f64 = n.as_slice().iter().map(|&x| f64::from(x) * f64::from(x)).sum();
		assert!((norm - 1.0).abs() < 1e-6);

		let zero = Embedding::<JinaCodeV2>::from_vec(vec![0.0; 768]).unwrap();
		assert_eq!(zero.clone().l2_normalize(), zero);
	}

	// ── adversarial: serde boundary — wrong-dim and NaN JSON ────────────────

	/// Deserializing a JSON array of the wrong length must fail with
	/// DimensionMismatch propagated as a serde error.
	#[test]
	fn deserialize_wrong_dim_json_fails() {
		// 767 elements — one short of JinaCodeV2::DIMENSIONS (768).
		let short: Vec<f32> = vec![0.0f32; 767];
		let json = serde_json::to_string(&short).unwrap();
		let result: Result<Embedding<JinaCodeV2>, _> = serde_json::from_str(&json);
		assert!(result.is_err(), "wrong-dim JSON deserialization must fail");
		let err_str = result.unwrap_err().to_string();
		assert!(err_str.contains("768") || err_str.contains("dimension") || err_str.contains("mismatch"),
			"error should mention dimension mismatch: {}", err_str);
	}

	/// The deserialize boundary funnels through `from_vec`, so a non-finite
	/// component fails there even when the wire format (JSON) cannot itself
	/// carry NaN — `null` in a float array is the closest JSON attack shape.
	#[test]
	fn deserialize_nan_json_fails() {
		let mut elements = vec!["0.0"; 768];
		elements[3] = "null";
		let json = format!("[{}]", elements.join(","));
		let result: Result<Embedding<JinaCodeV2>, _> = serde_json::from_str(&json);
		assert!(result.is_err(), "null-in-float-array must not deserialize");

		let mut v = vec![0.0f32; 768];
		v[0] = f32::INFINITY;
		let result = Embedding::<JinaCodeV2>::from_vec(v);
		assert!(
			matches!(result, Err(EmbedError::NonFinite)),
			"infinity in from_vec must give NonFinite"
		);
	}

	/// Serde roundtrip of a valid Embedding via JSON preserves all values.
	#[test]
	fn serde_roundtrip_valid_embedding() {
		let mut v = vec![0.0f32; 768];
		v[0] = 0.5;
		v[767] = -0.5;
		let emb = Embedding::<JinaCodeV2>::from_vec(v.clone()).unwrap();
		let json = serde_json::to_string(&emb).unwrap();
		let restored: Embedding<JinaCodeV2> = serde_json::from_str(&json).unwrap();
		assert_eq!(emb, restored, "serde roundtrip must preserve all values");
	}

	/// Deserializing a 769-element array (one too many) must fail.
	#[test]
	fn deserialize_too_many_dims_json_fails() {
		let long: Vec<f32> = vec![0.0f32; 769];
		let json = serde_json::to_string(&long).unwrap();
		let result: Result<Embedding<JinaCodeV2>, _> = serde_json::from_str(&json);
		assert!(result.is_err(), "too-many-dims JSON deserialization must fail");
	}

	#[test]
	fn cosine_orthogonal_identical_zero() {
		let a = unit(0);
		let b = unit(1);
		assert_eq!(a.cosine(&b), 0.0);
		assert!((a.cosine(&a) - 1.0).abs() < 1e-6);
		let zero = Embedding::<JinaCodeV2>::from_vec(vec![0.0; 768]).unwrap();
		assert_eq!(a.cosine(&zero), 0.0);
	}
}
