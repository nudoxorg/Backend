//! Health + parse-status coordination.
//!
//! Surfaces a project's health (has it been parsed, is it loaded into the
//! runtime stores) and drives load balancing / parse-status coordination,
//! reading from the registry's postgres-backed health + queue.
//!
//! IMPLEMENT HERE: the health/coordination read + control surface.
