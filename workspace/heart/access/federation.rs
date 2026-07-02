//! Layered source federation.
//!
//! The deployment model is a single **definitive** registry (hosted centrally)
//! plus zero or more self-hosted **overlays** that *extend* it (add packages the
//! base lacks) and *override* it (shadow packages the base also has). Resolution
//! precedence is: overlays first (in declared order, highest precedence first),
//! then the definitive base. So a self-hosted overlay can shadow a definitive
//! package with its own build, and add private packages, without forking.
//!
//! The single-base invariant is *structural*: [`Federation::base`] is a plain
//! field, not `Option` and not a list — "zero definitive registries" and "two
//! definitive registries" are both unrepresentable.

use serde::{Deserialize, Serialize};

use super::source::SourceId;

/// The role a source plays within a [`Federation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::Display)]
pub enum SourceRole {
	/// The single definitive registry (centrally hosted). Lowest precedence.
	Definitive,
	/// A self-hosted overlay that extends and overrides the definitive registry.
	Overlay,
}

/// A value tagged with the source it was resolved from, so a caller can tell an
/// overlay-provided (overridden) result from a definitive one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sourced<T> {
	/// The resolved value.
	pub value: T,
	/// The source it came from.
	pub source: SourceId,
	/// That source's role (whether this was an override).
	pub role: SourceRole,
}

impl<T> Sourced<T> {
	/// Tag a value with the source + role it resolved from.
	pub const fn new(value: T, source: SourceId, role: SourceRole) -> Self {
		Self { value, source, role }
	}

	/// Whether this value came from an overlay (i.e. shadows/extends the base).
	pub const fn is_override(&self) -> bool { matches!(self.role, SourceRole::Overlay) }

	/// Map the value, preserving the source tag.
	pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Sourced<U> {
		Sourced { value: f(self.value), source: self.source, role: self.role }
	}
}

/// One overlay source and its handle. `S` is whatever per-source handle the
/// caller federates over (in the server, a bundle of that source's live stores).
#[derive(Debug, Clone)]
pub struct Overlay<S> {
	/// The overlay's stable id.
	pub id: SourceId,
	/// The per-source handle.
	pub handle: S,
}

/// A federation of registries: exactly one definitive base plus zero or more
/// precedence-ordered overlays.
pub struct Federation<S> {
	base_id: SourceId,
	base: S,
	/// Overlays in precedence order — highest-precedence (queried first) at index 0.
	overlays: Vec<Overlay<S>>,
}

impl<S> Federation<S> {
	/// Start a federation from its definitive base.
	pub fn new(base_id: SourceId, base: S) -> Self { Self { base_id, base, overlays: Vec::new() } }

	/// Append an overlay at the lowest overlay precedence (queried after existing
	/// overlays but before the base). Returns `self` for builder-style assembly.
	pub fn with_overlay(mut self, id: SourceId, handle: S) -> Self {
		self.overlays.push(Overlay { id, handle });
		self
	}

	/// The definitive base handle.
	pub fn base(&self) -> &S { &self.base }

	/// The definitive base's source id.
	pub const fn base_id(&self) -> SourceId { self.base_id }

	/// The overlays, in precedence order (highest first).
	pub fn overlays(&self) -> &[Overlay<S>] { &self.overlays }

	/// The number of sources (base + overlays).
	pub fn len(&self) -> usize { self.overlays.len() + 1 }

	/// Every source in resolution-precedence order: overlays (highest first),
	/// then the definitive base. The canonical order to walk for
	/// override-then-extend resolution.
	pub fn in_precedence(&self) -> impl Iterator<Item = (SourceId, SourceRole, &S)> {
		self.overlays
			.iter()
			.map(|o| (o.id, SourceRole::Overlay, &o.handle))
			.chain(std::iter::once((self.base_id, SourceRole::Definitive, &self.base)))
	}
}
