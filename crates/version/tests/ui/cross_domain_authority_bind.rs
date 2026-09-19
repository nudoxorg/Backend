//! Negative type proof for compact content-authority binding.
//! A dependency-set authority must never construct an object-domain identity.
//! Successful compilation would expose a cross-domain substitution vulnerability.
use backend_version::{ContentAuthority, ContentId, DependencySetDomain, ObjectDomain};

/// Attempts the forbidden cross-domain bind that the type checker must reject.
fn cross_domain(authority: ContentAuthority<DependencySetDomain>) {
    let _: ContentId<ObjectDomain> = authority.bind([0_u8; 31]);
}

/// Keeps the compile-fail fixture executable without invoking the impossible helper.
fn main() {}
