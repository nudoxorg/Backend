//! A relevance score that is *provably* finite — NaN and ±∞ are unrepresentable,
//! so a total order (`Ord`/`Eq`) is sound and ranking can never panic.

use nutype::nutype;

#[nutype(
    validate(finite),
    derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Display)
)]
pub struct Score(f32);
