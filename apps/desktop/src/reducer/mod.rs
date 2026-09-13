//! The reducer that holds exactly one coherent view root and one cursor.
//! It admits every batch against the exact prior root and never invents state.
//! Behaviour is unchanged from the surface this rewrite replaced.

pub(crate) mod model;
