//! Cross-cutting access control + multi-source federation.
//!
//! Every read and write in the system passes one [`AccessContext`] describing
//! *who* is asking, and every record carries a [`Visibility`] and a
//! [`source::Source`] it belongs to. [`control`] is the single choke point that
//! turns (principal, visibility, source) into an [`AccessDecision`], so
//! authorization is never re-implemented per backend.
//!
//! The types are threaded through every search/publish signature *now*, even
//! though the policy is not yet implemented, because adding a parameter to
//! every trait later is the most expensive retrofit there is.

pub mod control;
pub mod federation;
pub mod source;
pub mod tenant;
pub mod visibility;

pub use control::{AccessContext, AccessDecision, AccessPolicy, Action};
pub use federation::{Federation, Overlay, SourceRole, Sourced};
pub use source::{Source, SourceId};
pub use tenant::{Principal, Tenant};
pub use visibility::Visibility;
