//! Isolating the multi-parent setup: a package can be reachable through more
//! than one parent/source, and registry search must present a single coherent
//! view over that fan-in.
//!
//! IMPLEMENT HERE: the de-duplication/merge that hides multi-parent from callers.
