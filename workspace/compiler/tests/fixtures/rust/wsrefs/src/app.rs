//! Cross-module (fully-qualified) and cross-crate usage.

/// Calls `math::add` by its crate-absolute path and `helper::greet` from the
/// path-dependency.
pub fn run() -> i32 {
	crate::math::add(1, 2) + helper::greet()
}
