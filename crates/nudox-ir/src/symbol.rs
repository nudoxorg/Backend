use std::{path::PathBuf, range::Range};

use ecow::{EcoString, EcoVec};

use crate::arena::EntryIdx;

pub struct NudoxPath;
pub enum Visibility {}

pub struct Symbol {
	pub name:          EcoString,
	pub path:          NudoxPath,
	pub visibility:    Visibility,
	pub documentation: Option<EcoString>,
	pub parent:        Option<EntryIdx<()>>,
	pub children:      EcoVec<EntryIdx<()>>,
	pub source:        PathBuf,
	pub span:          Range<usize>,
}
