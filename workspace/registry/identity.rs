//! Deterministic global identification.
//!
//! Every library symbol gets a version-agnostic GUID derived (UUID v5, fixed
//! namespace) from its terminus instance and entry URI, so any system can
//! recompute the same id offline — we never mint random ids for library
//! symbols.
