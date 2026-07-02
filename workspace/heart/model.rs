//! A non-empty embedding-model identifier. Constructing one with a blank name is
//! impossible, so "which model produced this vector" is always answerable.

use nutype::nutype;

#[nutype(
    sanitize(trim),
    validate(not_empty),
    derive(Debug, Clone, PartialEq, Eq, Hash, Display, AsRef, Serialize, Deserialize)
)]
pub struct ModelId(String);
