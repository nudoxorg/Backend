//! The language-agnostic *occurrence* contract.
//!
//! An [`Occurrence`] is one span in one source file that either *is* a
//! definition or *uses* one, resolved to the [`NudoxPath`] identity the IR,
//! graph, and search layers already speak. Tree-sitter is the universal
//! baseline producer; semantic oracles later emit the *same* artifact at higher
//! [`Confidence`]. Consumers never learn which tier produced a row.
//!
//! This sits beside the surface IR (never inside `ir::Entry`): occurrences are a
//! sibling corpus keyed by path + file/span, not a new shape of declaration.

use std::{ops::Range, path::PathBuf};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{entry::NudoxPath, syntax::types::ReferenceKind};

/// Whether this span *is* the thing or *uses* the thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Role {
	/// The span names the item at its declaration site.
	Definition,
	/// The span refers to an item declared elsewhere.
	Reference,
}

/// How the target path was established.
///
/// The ordering is meaningful: higher variants are more trustworthy, so the
/// derived [`Ord`] drives both the resolution ladder's "keep the best match"
/// logic and the graph's assertion policy (`>= Index` is graph-worthy).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Confidence {
	/// Raw identifier only; target is a best-effort name. Never graph-asserted.
	Syntactic,
	/// Unique last-segment match against the package index.
	Suffix,
	/// Exact/alias match against the package index, or module-scope sibling.
	Index,
	/// Resolved through the file's import table (incl. external deps).
	Import,
	/// Resolved by the language's semantic oracle (RA / go-types / oxc / pyrefly).
	Oracle,
}

/// One resolved occurrence: a span, what it targets, and how sure we are.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Occurrence {
	/// Byte range in the file (tree-sitter native; line/col derived lazily).
	pub span: Range<usize>,
	/// Fully-qualified target. External deps use [`NudoxPath::External`].
	pub target: NudoxPath,
	/// The syntactic role of the reference (`MethodCall` finally emitted).
	pub kind: ReferenceKind,
	/// Whether the span defines or uses `target`.
	pub role: Role,
	/// Innermost enclosing definition (syntactic FQN), `None` at module top
	/// level (the target then attributes to the module entry).
	pub enclosing: Option<NudoxPath>,
	/// True when `enclosing` matched an entry in the surface index.
	pub anchored: bool,
	/// How the target was established.
	pub confidence: Confidence,
}

/// Every occurrence found in one source file, in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct FileOccurrences {
	/// The file, relative to the package root.
	pub path: PathBuf,
	/// The occurrences within it, sorted by span start.
	pub occurrences: Vec<Occurrence>,
}

/// A resolved corpus for a whole package: the per-file tables plus the honesty
/// meter that makes precision observable across oracle upgrades.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct OccurrenceSet {
	/// One entry per parsed source file, sorted by path.
	pub files: Vec<FileOccurrences>,
	/// Totals per kind × confidence and the unresolved tally.
	pub stats: ResolutionStats,
}

/// A single `(kind, confidence)` bucket count. Kept as a sorted `Vec` (rather
/// than a map) so the postcard encoding is deterministic and self-describing.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct KindConfidenceCount {
	pub kind: ReferenceKind,
	pub confidence: Confidence,
	pub count: u64,
}

/// The resolution "honesty meter": how many occurrences resolved at each
/// `(kind, confidence)`, and how many references resolved to nothing.
///
/// Unresolved references are *not* stored as occurrences — they are only
/// tallied here, so precision is measurable without polluting the wire.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ResolutionStats {
	/// Resolved-occurrence counts bucketed by `(kind, confidence)`, sorted.
	pub buckets: Vec<KindConfidenceCount>,
	/// References that matched no path at any tier.
	pub unresolved: u64,
}

impl ResolutionStats {
	/// Record one resolved occurrence in the `(kind, confidence)` bucket.
	pub fn record(&mut self, kind: ReferenceKind, confidence: Confidence) {
		match self
			.buckets
			.binary_search_by(|b| (b.kind, b.confidence).cmp(&(kind, confidence)))
		{
			Ok(i) => self.buckets[i].count += 1,
			Err(i) => self.buckets.insert(i, KindConfidenceCount { kind, confidence, count: 1 }),
		}
	}

	/// Record one reference that resolved to nothing.
	pub fn record_unresolved(&mut self) {
		self.unresolved += 1;
	}

	/// Total resolved occurrences at `>= confidence`, across all kinds.
	pub fn resolved_at_least(&self, confidence: Confidence) -> u64 {
		self.buckets
			.iter()
			.filter(|b| b.confidence >= confidence)
			.map(|b| b.count)
			.sum()
	}
}
