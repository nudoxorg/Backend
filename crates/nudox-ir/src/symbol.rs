use std::{path::PathBuf, range::Range};

use ecow::EcoString;

pub struct NudoxPath;
pub enum Visibility {
	Public,
}

pub struct Symbol {
	pub name:          EcoString,
	pub path:          NudoxPath,
	pub visibility:    Visibility,
	pub documentation: Option<EcoString>,
	pub source:        PathBuf,
	pub span:          Range<usize>,
}
