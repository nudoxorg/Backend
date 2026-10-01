//! Package browsing facts: what a project depends on, why, and in what role.
//!
//! This module is pure. The local service runs Cargo and reads the lockfile
//! and the advisory authority; everything here turns those inputs into one
//! [`ProjectTree`] the desktop, CLI and MCP all read the same way. Sentences
//! about the tree are spelled in `backend-present`, never here.

mod cargo;
mod roles;
mod tree;

#[cfg(test)]
mod tests;

pub use cargo::{CargoTreeError, lockfile_input, metadata_input};
pub use roles::{RoleEvidence, RoleId};
pub use tree::{
    AdvisoryObserver, AdvisorySourceState, DirectDependency, Duplicate, DuplicateCopy,
    LockedInactiveCoverage, LockfileGraphCoverage, LockfileWorkspaceMembership, MAX_TREE_PACKAGES,
    MemberEdge, PROJECT_TREE_SCHEMA, PackageOrigin, PackageRole, ProjectTree, TreeAdvisory,
    TreeEdge, TreeHealth, TreeInput, TreeInputPackage, TreeMember, TreePackage, TreeSource, WhyHop,
    build_tree,
};
