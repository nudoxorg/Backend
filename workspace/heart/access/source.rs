//! A `Source` — one configured instance of an external provider (e.g. a
//! self-hosted TerminusDB, a private Qdrant, a shared registry), so the system
//! can federate across many at once.
//!
//! IMPLEMENT HERE: the source identity/handle + the per-source connection
//! coordinates, so a query can fan out across (or be pinned to) sources.
