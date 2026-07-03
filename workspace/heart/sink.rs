//! The remote-upload contract every derived store speaks — a thin wrapper over a
//! [`tower::Service`].
//!
//! Rather than re-implement a retry loop per backend, a [`Sink`] wraps whatever
//! `Service<Req>` a store exposes with one shared [`RetryTransient`] policy: it
//! consults [`Retryable`] to back off on transient faults and surface permanent
//! ones immediately. [`BatchSink`] is the same, for services that take a batch
//! per call. Stacking further tower layers (concurrency limit, rate limit,
//! timeout) is then just `ServiceBuilder` composition at the wrap site.

use std::future::{Ready, ready};

use serde::{Deserialize, Serialize};
use tower::{
	Service, ServiceExt,
	retry::{Policy, Retry},
};

use crate::error::Retryable;

/// Which derived store a record fans out to. The relational spine is the source
/// of truth; these are the read-optimized projections kept in sync from it.
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	Hash,
	Serialize,
	Deserialize,
	strum::Display,
	strum::EnumString,
	strum::EnumIter,
)]
pub enum DerivedStore {
	/// The semantic vector store (qdrant).
	Vector,

	/// The relationship graph store (terminus).
	Graph,

	/// The full-text search index (tantivy).
	Text,
}

/// The shared tower retry policy: retry only *transient* failures (per
/// [`Retryable`]), and only until a fixed budget is spent — so a poison-pill
/// upload can neither livelock nor be silently dropped.
#[derive(Debug, Clone)]
pub struct RetryTransient {
	remaining: usize,
}

impl RetryTransient {
	/// The default number of retry attempts a transient failure gets.
	pub const DEFAULT_BUDGET: usize = 4;

	/// A policy with an explicit retry budget.
	pub const fn new(budget: usize) -> Self { Self { remaining: budget } }
}

impl Default for RetryTransient {
	fn default() -> Self { Self::new(Self::DEFAULT_BUDGET) }
}

impl<Req, Res, E> Policy<Req, Res, E> for RetryTransient
where
	Req: Clone,
	E: Retryable,
{
	type Future = Ready<()>;

	fn retry(&mut self, _req: &mut Req, result: &mut Result<Res, E>) -> Option<Self::Future> {
		match result {
			Ok(_) => None,
			Err(e) if self.remaining > 0 && e.is_retryable() => {
				self.remaining -= 1;
				Some(ready(()))
			}
			Err(_) => None,
		}
	}

	fn clone_request(&mut self, req: &Req) -> Option<Req> { Some(req.clone()) }
}

/// A remote-upload endpoint: a [`tower::Service`] wrapped with the shared
/// transient-retry policy. Any module ships a record out through
/// [`Sink::deliver`] and gets backpressure-aware retries for free.
#[derive(Debug, Clone)]
pub struct Sink<S> {
	inner:  S,
	policy: RetryTransient,
}

impl<S> Sink<S> {
	/// Wrap a service as a retrying sink with the default retry budget.
	pub fn new(inner: S) -> Self { Self { inner, policy: RetryTransient::default() } }

	/// Wrap a service with an explicit retry budget.
	pub fn with_budget(inner: S, budget: usize) -> Self {
		Self { inner, policy: RetryTransient::new(budget) }
	}

	/// Deliver one item, retrying transient failures per [`Retryable`].
	pub async fn deliver<Req>(&self, item: Req) -> Result<S::Response, S::Error>
	where
		S: Service<Req> + Clone,
		Req: Clone,
		S::Error: Retryable,
	{
		Retry::new(self.policy.clone(), self.inner.clone()).oneshot(item).await
	}
}

/// Like [`Sink`], but the wrapped service accepts a *batch* of records per call —
/// one round-trip for many items.
#[derive(Debug, Clone)]
pub struct BatchSink<S> {
	inner:  S,
	policy: RetryTransient,
}

impl<S> BatchSink<S> {
	/// Wrap a batch service with the default retry budget.
	pub fn new(inner: S) -> Self { Self { inner, policy: RetryTransient::default() } }

	/// Wrap a batch service with an explicit retry budget.
	pub fn with_budget(inner: S, budget: usize) -> Self {
		Self { inner, policy: RetryTransient::new(budget) }
	}

	/// Deliver a batch, retrying transient failures per [`Retryable`].
	pub async fn deliver_batch<Item>(&self, items: Vec<Item>) -> Result<S::Response, S::Error>
	where
		S: Service<Vec<Item>> + Clone,
		Item: Clone,
		S::Error: Retryable,
	{
		Retry::new(self.policy.clone(), self.inner.clone()).oneshot(items).await
	}
}
