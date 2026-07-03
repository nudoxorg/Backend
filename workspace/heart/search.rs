//! Shared search vocabulary.
//!
//! One response-pagination type for every search surface (symbols, packages,
//! anything). A *hit* is always a [`Scored<T>`] — there is deliberately no
//! per-surface `Match`/`Hit` struct — and a *page* of hits is a [`Page<T>`]
//! carrying the opaque [`Cursor`] token to resume from.

use serde::{Deserialize, Serialize};

use crate::score::Scored;

/// One keyset-paginated page of results. `T` is the hit payload (e.g. `Symbol`,
/// `GlobalPackage`); the stream yields `Scored<T>` and the page bundles them with
/// the opaque token to resume from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
	/// The scored hits on this page, in descending relevance.
	pub items: Vec<Scored<T>>,

	/// The opaque [`crate::Cursor`] token to resume after the last item, or `None`
	/// if this is the last page.
	pub next: Option<String>,
}
