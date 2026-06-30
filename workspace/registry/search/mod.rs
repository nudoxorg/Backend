//! Registry search — searching the *registry itself* (finding packages), as
//! opposed to symbol/code search. Postgres is the source; tantivy is the
//! abstraction layered over it.
//!
//! IMPLEMENT HERE: the registry-search entry point that the server's admin/read
//! surface calls.

pub mod multi_parent;
pub mod tantivy;
