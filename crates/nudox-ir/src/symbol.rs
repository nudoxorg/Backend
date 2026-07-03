use std::{path::PathBuf, range::Range};

use ecow::EcoString;

#[derive(Debug, PartialEq, Eq)]
pub struct NudoxPath;

#[derive(Debug, PartialEq, Eq)]
pub enum Visibility {
	Public,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Symbol {
	pub name:          EcoString,
	pub path:          NudoxPath,
	pub visibility:    Visibility,
	pub documentation: Option<EcoString>,
	pub source:        PathBuf,
	pub span:          Range<usize>,
}
