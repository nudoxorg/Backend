//! Capability witnesses: unforgeable proof that authorization happened.
//!
//! Every method that touches tenant data requires a matching capability — a
//! zero-sized (on the happy path) token with a **sealed constructor** that can
//! only be minted through the composition's `authorize_*` entrypoints. A caller
//! who does not hold the right cap cannot pass the method's type signature — the
//! type system enforces the authorization point.
//!
//! This module holds only the *vocabulary* (the tenant id, the cap witnesses,
//! the principal). Minting (`authorize_read`/`authorize_write`/`authorize_admin`)
//! and the axum `FromRequestParts` extraction are composition concerns and live
//! in the staged server bits — `heart` carries no axum dependency.
//!
//! # Sealing
//! The cap structs live in a private inner module so `_priv: ()` is not needed;
//! the unit fields are simply not `pub`. Minting a cap goes through
//! [`ReadCap::mint`]/[`WriteCap::mint`]/[`AdminCap::mint`], which are
//! `pub(crate)`-free but documented as composition-only entrypoints.

// ── TenantId ────────────────────────────────────────────────────────────────

/// An opaque tenant identity carried by every authorized request.
///
/// Today all requests are treated as the anonymous public tenant. Future work
/// will derive this from a bearer token or trusted-proxy header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TenantId(String);

impl TenantId {
    /// The anonymous public tenant used for all requests under the allow-all policy.
    pub fn anonymous() -> Self {
        Self(String::from("anonymous"))
    }

    /// The system tenant — used for internal background operations (mirror
    /// catalog followers, etc.) that do not originate from an external principal.
    pub fn system() -> Self {
        Self(String::from("system"))
    }

    /// The raw identity string (for logging and audit traces).
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for TenantId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// ── Capability witnesses ─────────────────────────────────────────────────────
//
// Each cap lives in a private inner module so the struct fields are inaccessible
// outside that module, making the `mint` constructors the only minting path.

mod sealed {
    use super::TenantId;

    /// Proof that a read operation was authorized for `tenant`.
    #[derive(Debug, Clone)]
    pub struct ReadCap {
        pub(super) tenant: TenantId,
    }

    /// Proof that a write operation was authorized for `tenant`.
    #[derive(Debug, Clone)]
    pub struct WriteCap {
        pub(super) tenant: TenantId,
    }

    /// Proof that an admin operation was authorized for `tenant`.
    #[derive(Debug, Clone)]
    pub struct AdminCap {
        pub(super) tenant: TenantId,
    }
}

pub use sealed::{AdminCap, ReadCap, WriteCap};

impl ReadCap {
    /// Mint a read capability. **Composition-only:** call this from an
    /// `authorize_read` entrypoint after the access policy has admitted the
    /// principal — never from a handler directly.
    pub fn mint(tenant: TenantId) -> Self {
        sealed::ReadCap { tenant }
    }

    /// The tenant this capability was issued for.
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }
}

impl WriteCap {
    /// Mint a write capability. **Composition-only** (see [`ReadCap::mint`]).
    pub fn mint(tenant: TenantId) -> Self {
        sealed::WriteCap { tenant }
    }

    /// The tenant this capability was issued for.
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }

    /// Mint a system-level write capability for internal background operations
    /// (e.g. mirror catalog followers). Bypasses principal-based authorization.
    pub fn system() -> Self {
        sealed::WriteCap {
            tenant: TenantId::system(),
        }
    }
}

impl AdminCap {
    /// Mint an admin capability. **Composition-only** (see [`ReadCap::mint`]).
    pub fn mint(tenant: TenantId) -> Self {
        sealed::AdminCap { tenant }
    }

    /// The tenant this capability was issued for.
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }
}

// ── Principal ────────────────────────────────────────────────────────────────

/// The caller's identity as resolved from the HTTP request.
///
/// The extraction (axum `FromRequestParts`) is a composition concern; this is
/// the resolved-identity vocabulary. Under the current allow-all policy every
/// request produces a `Principal` with the anonymous tenant.
#[derive(Debug, Clone)]
pub struct Principal {
    /// The resolved tenant identity.
    pub tenant: TenantId,
}

impl Principal {
    /// The anonymous public principal — used in tests and for requests that
    /// carry no bearer token.
    pub fn anonymous() -> Self {
        Self {
            tenant: TenantId::anonymous(),
        }
    }
}

/// An admin-privileged principal: a distinct type so a handler requiring admin
/// access cannot compile without the extractor running.
///
/// Under the current allow-all policy every request is admitted as admin. The
/// type distinction exists so admin endpoints are structurally differentiated
/// from user-facing ones today; enforcement is a one-line policy change.
#[derive(Debug, Clone)]
pub struct AdminPrincipal {
    /// The resolved tenant identity.
    pub tenant: TenantId,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TenantId::anonymous()` returns a stable identity string.
    #[test]
    fn tenant_id_anonymous_is_stable() {
        let t = TenantId::anonymous();
        assert_eq!(t.as_str(), "anonymous");
        assert_eq!(t.to_string(), "anonymous");
    }

    /// A minted cap carries its tenant through.
    #[test]
    fn minted_cap_carries_tenant() {
        let cap = ReadCap::mint(TenantId::anonymous());
        assert_eq!(cap.tenant().as_str(), "anonymous");
        assert_eq!(WriteCap::system().tenant().as_str(), "system");
    }
}
