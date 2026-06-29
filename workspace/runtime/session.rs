//! Per-session graph state: as a user explores, their accumulated nodes/edges
//! are merged into a session-scoped graph (structurally shared, optionally
//! persisted) so subsequent expansions build on what they have already seen.
