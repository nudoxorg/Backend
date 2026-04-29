/// ! We store our data as a nested tree structure to ensure maximum
/// composability for building the structure, and the ease of Serde and so on
/// and so forth. We're able to store references to other objects using absolute
/// paths, and during upload time, a graph is composed once. A (highly) unsafe
/// structure should be considered in the future to avoid this reconstruction
/// between backends.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::kind::EntryKind;

/// A representation of a documented API entry.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Entry {
	/// Semantic name of the entry (e.g., `"std::time"`, `"to_string"`).
	pub name: String,

	/// Absolute path to the first / canonical occurrence of this entry.
	pub path: NudoxPath,

	/// Alternate paths (re-exports, aliased imports, etc.).
	pub aliases: Option<HashSet<Vec<String>>>,

	/// The syntactic / semantic kind of this entry.
	pub kind: EntryKind,
}

/// A path to a location within the Nudox registry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum NudoxPath {
	/// A path inside an external dependency, bundled with its crate/package name.
	External { path: PathBuf, dependency: String },

	/// A path local to the current project / crate / package.
	Local(PathBuf),
}

/// Top-level index table for IR entries.
///
/// A flat `HashMap` keyed by stable integer IDs keeps TerminusDB integration
/// straightforward and avoids deep nesting at the root level.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Index {
	pub root_ids:        Vec<NudoxPath>,
	pub entries_by_path: HashMap<NudoxPath, Entry>,
}
