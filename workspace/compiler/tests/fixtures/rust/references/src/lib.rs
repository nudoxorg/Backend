//! Fixture for reference-edge extraction: a small, unambiguous call graph.
//!
//! `hello` calls `yo` twice; `report` calls `hello`. The resolved occurrence
//! corpus must attribute those calls to their enclosing functions and the graph
//! must reify them as `Reference` edges (REFERENCES-PLAN §6.4).

/// The callee everyone reaches for.
pub fn yo() -> i32 {
	42
}

/// Calls [`yo`] twice.
pub fn hello() -> i32 {
	yo() + yo()
}

/// Calls [`hello`].
pub fn report() -> i32 {
	hello() + 1
}
