#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::ir::{
    kind::{Kind, Visibility},
    parameter::Parameter,
};

/// A representation of a documented API entry.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Entry {
    // Required
    pub name: String,      // Semantic name for the entry (std::time, or to_string)
    pub id: i64,           // A real nice way to link together other entries (i64 for more range)
    pub path: Vec<String>, // The absolute path leading to the first instance of this entry
    pub kind: Kind,        // The kind of entry this is
    pub visibility: Option<Visibility>, // The visibility of this entry (public, private, flags?)

    pub documentation: Option<String>, // The associated documentation

    // Added missing properties
    pub members: Option<Vec<EntryRef>>, // TODO fix Entry to EntryRef -> referring to local (current version) linkage.
    pub input_parameters: Option<Vec<Parameter>>,
    pub output_parameters: Option<Vec<Parameter>>,
    pub type_parameters: Option<Vec<String>>,
}

/// Reference to Entry with ID and path if an API entry used in members
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct EntryRef {
    pub id: i64,
    pub path: Vec<String>,
}
