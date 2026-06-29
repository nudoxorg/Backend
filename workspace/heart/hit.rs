//! The generic scored-result wrapper, shared by every search surface.

/// A hit for a search result or otherwise
pub struct Hit<T> {
	pub value: T,
	pub score: Option<f32>
}
