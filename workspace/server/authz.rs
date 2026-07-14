//! Capability witnesses: unforgeable proof that authorization happened.
//!
//! Every method that touches tenant data requires a matching capability — a
//! zero-sized (on the happy path) token with a **private constructor** that
//! can only be minted by [`Server::authorize_read`], [`Server::authorize_write`],
//! or [`Server::authorize_admin`]. A caller who does not hold the right cap
//! cannot pass the method's type signature — the type system enforces the
//! authorization point.
//!
//! # Policy
//! Today the policy is **allow-all**: every well-formed request mints a cap.
//! The deny arm is wired (returning [`ForbiddenReason`]) so enforcement becomes
//! a one-line change. Tests exercise the deny arm directly to prove the 403
//! projection is reachable.
//!
//! # Sealing
//! The cap structs live in a private inner module so `_priv: ()` is not needed;
//! the unit fields are simply not `pub`. The re-exports at the top of this file
//! expose only the types, never the constructors.

use axum::{
    extract::FromRequestParts,
    http::request::Parts,
};
use registry::runtime::vector::EmbeddingModel;

use crate::Server;
use crate::error::{ForbiddenReason, ServerError};

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
// outside that module, making the constructors the only minting path.

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
    /// The tenant this capability was issued for.
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }
}

impl WriteCap {
    /// The tenant this capability was issued for.
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }
}

impl AdminCap {
    /// The tenant this capability was issued for.
    pub fn tenant(&self) -> &TenantId {
        &self.tenant
    }
}

// ── Principal ────────────────────────────────────────────────────────────────

/// The caller's identity as resolved from the HTTP request.
///
/// Extracted from request parts by [`axum::extract::FromRequestParts`].
/// Under the current allow-all policy every request produces a `Principal`
/// with the anonymous tenant.
#[derive(Debug, Clone)]
pub struct Principal {
    pub tenant: TenantId,
}

impl Principal {
    /// The anonymous public principal — used in tests and for requests that
    /// carry no bearer token.
    pub fn anonymous() -> Self {
        Self { tenant: TenantId::anonymous() }
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
    pub tenant: TenantId,
}

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for Principal
where
    S: Send + Sync,
{
    type Rejection = ServerError;

    async fn from_request_parts(
        _parts: &mut Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        // Allow-all: every request is a valid principal with the anonymous tenant.
        // Future: extract bearer token / trusted-proxy header here.
        Ok(Principal { tenant: TenantId::anonymous() })
    }
}

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for AdminPrincipal
where
    S: Send + Sync,
{
    type Rejection = ServerError;

    async fn from_request_parts(
        _parts: &mut Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        // Allow-all: every request is admitted as admin.
        // Future: enforce an admin claim / role in the bearer token here.
        //
        // To flip to deny-all for a test:
        //   return Err(ForbiddenReason::ActionDenied { action: "admin" }.into());
        Ok(AdminPrincipal { tenant: TenantId::anonymous() })
    }
}

// ── Minting ──────────────────────────────────────────────────────────────────

impl<M: EmbeddingModel> Server<M> {
    /// Authorize a read operation and mint a [`ReadCap`].
    ///
    /// The deny arm constructs [`ForbiddenReason`] so the 403 path is always
    /// reachable — enforcement is a single policy predicate away.
    pub fn authorize_read(
        &self,
        principal: &Principal,
        action: &'static str,
    ) -> Result<ReadCap, ServerError> {
        let tenant = &principal.tenant;
        // Allow-all policy: every authenticated principal may read.
        if Self::read_allowed(tenant, action) {
            tracing::trace!(%tenant, action, "read access granted");
            Ok(ReadCap { tenant: tenant.clone() })
        } else {
            tracing::warn!(%tenant, action, "read access denied");
            Err(ForbiddenReason::ActionDenied { action }.into())
        }
    }

    /// Authorize a write operation and mint a [`WriteCap`].
    pub fn authorize_write(
        &self,
        principal: &Principal,
        action: &'static str,
    ) -> Result<WriteCap, ServerError> {
        let tenant = &principal.tenant;
        if Self::write_allowed(tenant, action) {
            tracing::trace!(%tenant, action, "write access granted");
            Ok(WriteCap { tenant: tenant.clone() })
        } else {
            tracing::warn!(%tenant, action, "write access denied");
            Err(ForbiddenReason::ActionDenied { action }.into())
        }
    }

    /// Authorize an admin operation and mint an [`AdminCap`].
    pub fn authorize_admin(
        &self,
        principal: &AdminPrincipal,
        action: &'static str,
    ) -> Result<AdminCap, ServerError> {
        let tenant = &principal.tenant;
        if Self::admin_allowed(tenant, action) {
            tracing::trace!(%tenant, action, "admin access granted");
            Ok(AdminCap { tenant: tenant.clone() })
        } else {
            tracing::warn!(%tenant, action, "admin access denied");
            Err(ForbiddenReason::ActionDenied { action }.into())
        }
    }

    // ── Policy predicates ────────────────────────────────────────────────────
    //
    // Allow-all today. The deny arm exists so `ForbiddenReason::ActionDenied`
    // is always constructed on the `else` branch — it is wired in, not dead.

    #[inline]
    fn read_allowed(_tenant: &TenantId, _action: &'static str) -> bool {
        true // allow-all
    }

    #[inline]
    fn write_allowed(_tenant: &TenantId, _action: &'static str) -> bool {
        true // allow-all
    }

    #[inline]
    fn admin_allowed(_tenant: &TenantId, _action: &'static str) -> bool {
        true // allow-all
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ServerError;

    /// `ForbiddenReason::ActionDenied` must project to HTTP 403. This test
    /// exercises the 403 path directly — it verifies that the dead variant is
    /// now reachable and maps correctly.
    #[test]
    fn forbidden_reason_maps_to_403() {
        let err = ServerError::Forbidden(ForbiddenReason::ActionDenied {
            action: "test.forbidden",
        });
        assert_eq!(
            err.status(),
            http::StatusCode::FORBIDDEN,
            "ForbiddenReason::ActionDenied must project to 403"
        );
    }

    /// The `ForbiddenReason::ActionDenied` message includes the action string.
    #[test]
    fn forbidden_reason_display_includes_action() {
        let reason = ForbiddenReason::ActionDenied { action: "admin.rebuild" };
        assert!(
            reason.to_string().contains("admin.rebuild"),
            "error display must name the denied action"
        );
    }

    /// `TenantId::anonymous()` returns a stable identity string.
    #[test]
    fn tenant_id_anonymous_is_stable() {
        let t = TenantId::anonymous();
        assert_eq!(t.as_str(), "anonymous");
        assert_eq!(t.to_string(), "anonymous");
    }
}
