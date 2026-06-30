//! Parse coordination — postgres is in charge of load balancing, coordination,
//! and the status of every parse: the queue of work, which tier owns it, and
//! freshness (pulling a library down again when its hash is stale).
//!
//! IMPLEMENT HERE: the parse queue + tier/load-balancing coordination.
