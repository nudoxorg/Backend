///! We store our data as a nested tree structure to ensure maximum composability for building the structure, and the ease of Serde and so on and so forth. We're able to store references to other objects using absolute paths, and during upload time, a graph is composed once. A (highly) unsafe structure should be considered in the future to avoid this reconstruction between backends.
use std::{
	collections::{HashMap, HashSet},
	path::Path,
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::kind::{Kind, Visibility};

/// A representation of a documented API entry.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Entry {
	// Required
	pub name: String,    // Semantic name for the entry (std::time, or to_string)
	pub path: NudoxPath, // The absolute path leading to the first/canonical instance of this entry

	// aliases = re-exports/other instances of path occurences of same id
	pub aliases: Option<HashSet<Vec<String>>>,
	pub kind: Kind,                     // The kind of entry this is
	pub visibility: Option<Visibility>, // The visibility of this entry (public, private, flags?)

	pub documentation: Option<String>, // The associated documentation

	// TODO: Attempt to repalce this with targeted expansions along the Module type for example
	pub members: Option<Vec<EntryRef>>, // References instead of duplicates
}

/// Stores the path to an exact location within the Nudox registry
pub enum NudoxPath {
	/// Is this path from an external dependency?
	External { path: Path, dependency: String },

	/// Is this path local to the project/crate/whatever
	Local(Path),
}

/// Top-level table for the IR entries/roots
/// TerminusDB integration is much smoother with this
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Index {
	pub root_ids: Vec<i64>,
	pub entries_by_id: HashMap<i64, Entry>,
}

/// Reference to Entry with ID and path if an API entry used in members
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct EntryRef {
	pub id: i64,
	pub path: Vec<String>,
}
