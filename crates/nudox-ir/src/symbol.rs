use std::{path::PathBuf, range::Range};

#[derive(Debug, PartialEq, Eq)]
pub struct NudoxPath;

#[derive(Debug, PartialEq, Eq)]
pub enum Visibility {
	Public,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Symbol {
	pub name:          String,
	pub path:          NudoxPath,
	pub visibility:    Visibility,
	pub documentation: Option<String>,
	pub source:        PathBuf,
	pub span:          Range<usize>,
}
