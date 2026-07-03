//! Real tests for `heart::access` — the multi-source federation + access-control
//! vocabulary. Every external provider is reached through a [`Source`]; a
//! definitive base plus self-hosted overlays federate via [`Federation`]; records
//! carry [`Visibility`]; and every read/write resolves through an
//! [`AccessPolicy`] choke point keyed on [`Tenant`].

use heart::{
	Id,
	access::{
		AccessContext, AccessDecision, AccessPolicy, Action, Federation, Principal, Source,
		SourceRole, Tenant, Visibility,
	},
	error::BackendKind,
};

/// A minimal policy for exercising the access vocabulary: public records are
/// world-readable; otherwise a principal may act only on records its own tenant
/// owns.
struct TestPolicy;
impl AccessPolicy for TestPolicy {
	fn decide(
		&self,
		ctx: &AccessContext,
		action: Action,
		visibility: Visibility,
		owner: Tenant,
	) -> AccessDecision {
		if action == Action::Read && visibility == Visibility::Public {
			return AccessDecision::Allow;
		}
		if ctx.principal.tenant.id() == owner.id() {
			return AccessDecision::Allow;
		}
		AccessDecision::Deny { reason: "not owner and not public" }
	}
}

fn principal_for(tenant: Tenant) -> Principal {
	Principal { id: Id::new_random(), tenant }
}

/// Every external provider is addressed through a `Source`, so the rest of the
/// system is provider-agnostic.
#[test]
fn providers_are_addressed_through_sources() {
	for backend in
		[BackendKind::Terminus, BackendKind::Qdrant, BackendKind::Tantivy, BackendKind::Postgres]
	{
		let source = Source { id: Id::new_random(), name: "provider".into(), backend };
		assert_eq!(source.backend, backend);
	}
}

/// A definitive base plus self-hosted overlays coexist, and resolve in
/// precedence order: overlays first (override), then the base (extend).
#[test]
fn multiple_sources_can_coexist() {
	let base = Id::new_random();
	let overlay_a = Id::new_random();
	let overlay_b = Id::new_random();

	// Handles are irrelevant here — federation is generic over them.
	let fed = Federation::new(base, ()).with_overlay(overlay_a, ()).with_overlay(overlay_b, ());

	assert_eq!(fed.len(), 3, "base + two overlays");
	assert_eq!(fed.base_id(), base);

	let order: Vec<(_, SourceRole)> =
		fed.in_precedence().map(|s| (s.source, s.role)).collect();
	// Overlays first (in declared order), then the definitive base last.
	assert_eq!(
		order,
		vec![
			(overlay_a, SourceRole::Overlay),
			(overlay_b, SourceRole::Overlay),
			(base, SourceRole::Definitive),
		]
	);
}

/// Visibility forms a lattice used for access decisions.
#[test]
fn records_carry_visibility() {
	assert!(Visibility::Public.subsumes(Visibility::Private));
	assert!(Visibility::Private.subsumes(Visibility::Personal));
	assert!(!Visibility::Personal.subsumes(Visibility::Public));
	assert!(Visibility::Public.subsumes(Visibility::Public));
}

/// A tenant may read its own + public records, but not another tenant's private
/// ones — enforced through the single policy choke point.
#[test]
fn access_control_enforces_tenancy() {
	let alice = Tenant::Individual(Id::new_random());
	let bob = Tenant::Enterprise(Id::new_random());
	let ctx = AccessContext { principal: principal_for(alice), sources: Vec::new() };

	// Public record owned by bob → alice may read.
	assert!(TestPolicy.decide(&ctx, Action::Read, Visibility::Public, bob).is_allowed());
	// Private record owned by bob → alice may not read.
	assert!(!TestPolicy.decide(&ctx, Action::Read, Visibility::Private, bob).is_allowed());
	// Private record owned by alice → alice may read.
	assert!(TestPolicy.decide(&ctx, Action::Read, Visibility::Private, alice).is_allowed());
}

/// Writes route through the same choke point: a principal cannot write another
/// tenant's records, only its own.
#[test]
fn access_control_gates_writes() {
	let alice = Tenant::Individual(Id::new_random());
	let bob = Tenant::Enterprise(Id::new_random());
	let ctx = AccessContext { principal: principal_for(alice), sources: Vec::new() };

	// Writing another tenant's record is denied even if it's public.
	assert!(!TestPolicy.decide(&ctx, Action::Write, Visibility::Public, bob).is_allowed());
	// Writing your own is allowed.
	assert!(TestPolicy.decide(&ctx, Action::Write, Visibility::Private, alice).is_allowed());
}
