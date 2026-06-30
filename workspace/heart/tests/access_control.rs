//! Diagram: **for all external providers (terminus/qdrant/etc) an enterprise is
//! expected to self-host on their own infrastructure, so all are abstracted over
//! personal/private and support multiple sources — which makes access control
//! easy for enterprises and individuals alike.**
//!
//! TDD specs (`todo!()`) for `heart::access`.

/// Every external provider is reachable through a `Source` abstraction.
///
/// Assert: terminus / qdrant / tantivy / postgres are each addressed via a
///   `Source`, so the rest of the system is provider-agnostic.
#[test]
fn providers_are_addressed_through_sources() {
    todo!("assert providers are wrapped behind Source");
}

/// Multiple sources of the same provider can coexist (federation).
///
/// Assert: two self-hosted terminus sources can both be registered and queried.
#[test]
fn multiple_sources_can_coexist() {
    todo!("assert multi-source federation");
}

/// Records carry personal/private/public visibility.
///
/// Assert: a record's visibility is part of its identity for access decisions.
#[test]
fn records_carry_visibility() {
    todo!("assert personal/private/public visibility");
}

/// A tenant may only read records its visibility/source permits.
///
/// Assert: access control denies a tenant reading another tenant's private
///   records, and allows its own + public ones.
#[test]
fn access_control_enforces_tenancy() {
    todo!("assert tenancy-scoped read access");
}

/// Writes are gated by the same access-control choke point.
///
/// Assert: a tenant cannot write to a source it lacks permission on.
#[test]
fn access_control_gates_writes() {
    todo!("assert write access control");
}
