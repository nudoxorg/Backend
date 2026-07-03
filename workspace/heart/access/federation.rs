//! Layered source federation.
//!
//! The deployment model is a single **definitive** registry (hosted centrally)
//! plus zero or more self-hosted **overlays** that *extend* it (add packages
//! the base lacks) and *override* it (shadow packages the base also has).
//! Resolution precedence is: overlays first (in declared order, highest
//! precedence first), then the definitive base. So a self-hosted overlay can
//! shadow a definitive package with its own build, and add private packages,
//! without forking.
//!
//! The single-base invariant is *structural*: [`Federation::base`] is a plain
//! field, not `Option` and not a list — "zero definitive registries" and "two
//! definitive registries" are both unrepresentable.

use serde::{Deserialize, Serialize};

use super::source::SourceId;

/// The role a source plays in a federation: the single definitive base, or one of
/// the precedence-ordered overlays that extend/override it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, strum::Display)]
pub enum SourceRole {
	/// The centrally-hosted definitive base.
	Definitive,
	/// A self-hosted overlay that extends and can override the base.
	Overlay,
}

/// A value tagged with the source it was resolved from, so a caller can tell an
/// overlay-provided (overriding) result from a definitive one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sourced<T> {
	/// The resolved value.
	pub value:  T,
	/// The source it came from.
	pub source: SourceId,
	/// Whether that source is the definitive base or an overlay.
	pub role:   SourceRole,
}

impl<T> Sourced<T> {
	/// Map the value, preserving the source tag.
	pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Sourced<U> {
		Sourced { value: f(self.value), source: self.source, role: self.role }
	}
}

/// One overlay source and its handle. `S` is whatever per-source handle the
/// caller federates over (in the server, a bundle of that source's live
/// stores).
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
	base_id:  SourceId,
	base:     S,
	/// Overlays in precedence order — highest-precedence (queried first) at index
	/// 0.
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
	pub fn in_precedence(&self) -> impl Iterator<Item = Sourced<&S>> {
		self.overlays
			.iter()
			.map(|o| Sourced { value: &o.handle, source: o.id, role: SourceRole::Overlay })
			.chain(std::iter::once(Sourced {
				value:  &self.base,
				source: self.base_id,
				role:   SourceRole::Definitive,
			}))
	}
}
