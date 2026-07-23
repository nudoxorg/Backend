use std::{ops::Range, path::PathBuf};

use crate::visitor::Visitor;

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub enum Visibility {
    Public,
}

#[derive(Debug, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Symbol {
    pub name: String,
    pub visibility: Visibility,
    pub documentation: String,
    pub source: PathBuf,
    pub span: Range<usize>,
}
