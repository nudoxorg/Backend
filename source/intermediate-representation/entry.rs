/// ! We store our data as a nested tree structure to ensure maximum
/// composability for building the structure, and the ease of Serde and so on
/// and so forth. We're able to store references to other objects using absolute
/// paths, and during upload time, a graph is composed once. A (highly) unsafe
/// structure should be considered in the future to avoid this reconstruction
/// between backends.
use rustc_hash::FxHashMap as HashMap;
use std::path::PathBuf;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

pub use crate::kind::Entry;

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
