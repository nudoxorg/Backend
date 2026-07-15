use std::{ops::Range, path::PathBuf};

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Visibility {
	Public,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Symbol {
	pub name:          String,
	pub visibility:    Visibility,
	pub documentation: Option<String>,
	pub source:        PathBuf,
	pub span:          Range<usize>,
}
