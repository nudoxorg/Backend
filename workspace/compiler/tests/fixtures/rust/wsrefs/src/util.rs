//! Cross-module usage via a `use` import.

use crate::math::add;

/// Calls `math::add` through an imported binding.
pub fn u() -> i32 {
	add(3, 4)
}
