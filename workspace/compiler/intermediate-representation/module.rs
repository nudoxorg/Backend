#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::entry::NudoxPath;

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Module {
	pub members: Option<Vec<NudoxPath>>,
}
