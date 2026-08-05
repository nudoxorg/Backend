//! Authorization composition: the axum extractors and cap-minting entrypoints.
//!
//! The capability *vocabulary* — [`TenantId`], the sealed [`ReadCap`]/
//! [`WriteCap`]/[`AdminCap`] witnesses (with their composition-only `mint`
//! constructors), and the [`Principal`]/[`AdminPrincipal`] resolved identities —
//! lives in [`heart::client::authz`] (§8: shared client vocabulary). It is
//! re-exported here so the moved handlers keep resolving `crate::authz::…`.
//!
//! What is genuinely *composition* stays here: the axum
//! [`FromRequestParts`](axum::extract::FromRequestParts) extractors (which need
//! the HTTP request) and the [`Server`]-bound `authorize_read`/`_write`/`_admin`
//! minting entrypoints (the only place a cap is issued).
//!
//! # Policy
//! Today the policy is **allow-all**: every well-formed request mints a cap.
//! The deny arm is wired (returning [`ForbiddenReason`]) so enforcement becomes
//! a one-line change. Tests exercise the deny arm directly to prove the 403
//! projection is reachable.

#[allow(unused_imports)]
use crate::registry;
use crate::registry::vector::EmbeddingModel;
use axum::{extract::FromRequestParts, http::request::Parts};

use crate::Server;
use crate::error::{ForbiddenReason, ServerError};

// ── Capability vocabulary (re-exported from heart::client::authz) ─────────────
// The cap witnesses + `TenantId` moved to `heart::client::authz` during the
// server dissolution; these re-exports keep the moved `crate::authz::…` paths
// resolving against the single source of truth. Minting stays composition-local
// (below), using each cap's `mint` constructor.
pub use heart::client::authz::{AdminCap, ReadCap, TenantId, WriteCap};

// ── Principal / AdminPrincipal (composition — the axum extraction targets) ────
// These are the resolved-identity types the axum `FromRequestParts` extractors
// materialize. Because the extractor impls are foreign-trait-on-the-type, the
// types must be *local* (orphan rule), so they stay here rather than being
// re-exported from `heart::client::authz` (whose copies are the pure vocab).

/// The caller's identity as resolved from the HTTP request.
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
#[derive(Debug, Clone)]
pub struct AdminPrincipal {
    /// The resolved tenant identity.
    pub tenant: TenantId,
}

// ── Extractors (composition — need the HTTP request) ──────────────────────────

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for Principal
where
    S: Send + Sync,
{
    type Rejection = ServerError;

    async fn from_request_parts(_parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Allow-all: every request is a valid principal with the anonymous tenant.
        // Future: extract bearer token / trusted-proxy header here.
        Ok(Principal::anonymous())
    }
}

#[async_trait::async_trait]
impl<S> FromRequestParts<S> for AdminPrincipal
where
    S: Send + Sync,
{
    type Rejection = ServerError;

    async fn from_request_parts(_parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        // Allow-all: every request is admitted as admin.
        // Future: enforce an admin claim / role in the bearer token here.
        //
        // To flip to deny-all for a test:
        //   return Err(ForbiddenReason::ActionDenied { action: "admin" }.into());
        Ok(AdminPrincipal {
            tenant: TenantId::anonymous(),
        })
    }
}

// ── Minting (composition — the only place a cap is issued) ─────────────────────

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
            Ok(ReadCap::mint(tenant.clone()))
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
            Ok(WriteCap::mint(tenant.clone()))
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
            Ok(AdminCap::mint(tenant.clone()))
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
    /// exercises the 403 path directly — it verifies that the deny variant is
    /// reachable and maps correctly.
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
        let reason = ForbiddenReason::ActionDenied {
            action: "admin.rebuild",
        };
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
