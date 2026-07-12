//! Phase-6 typestate + threat-tier invariants (buck2-runnable).
//!
//! The *negative* invariants (a `Job<Sealed>` cannot carry `NetGrant::On`; a
//! `NixCaps<Hermetic>` cannot call `enable_impure`) are encoded as `compile_fail`
//! doctests on `sandbox::job::NetOff` / `compiler::compile::nix::eval::NixCaps`.
//! Those documents the type-level guarantee and are runnable under `cargo test
//! --doc`; the buck2 doctest runner in this repo is on a mismatched rustc
//! toolchain (stable vs the nightly the crates are built with — E0514), so it
//! cannot execute doctests. These integration tests carry the *positive* path
//! and the runtime backstops that buck2 *can* run.

use heart::JobKey;
use sandbox::{
	Acquiring, Env, FsGrant, Job, NetGrant, ProducerProfile, Sealed, SealedBudget, ThreatTier,
};

fn acquiring_job(net: NetGrant, profile: ProducerProfile) -> Job<Acquiring> {
	let key = JobKey::derive(b"typestate-test", b"", b"root", b"");
	Job::acquiring(
		key,
		"/pkg",
		FsGrant::scratch(std::env::temp_dir()),
		Env::empty(),
		profile.limits(),
		net,
	)
}

#[test]
fn sealed_job_is_always_net_off() {
	// Even when acquisition had the network ON, sealing drops it.
	let acquiring = acquiring_job(NetGrant::On, ProducerProfile::Nix);
	assert_eq!(acquiring.net(), NetGrant::On);

	let sealed: Job<Sealed> = acquiring.seal(ThreatTier::Hostile);
	let budget: SealedBudget = sealed.budget();

	// `budget.net` is the `NetOff` marker; its only possible lowering is Off.
	let net: NetGrant = budget.net.into();
	assert_eq!(net, NetGrant::Off);

	// And projecting to a dynamic CapabilityBudget can never yield net-on.
	let cap = sealed.into_input();
	assert_eq!(cap.budget.net, NetGrant::Off);
}

#[test]
fn hostile_tier_clamps_below_a_loose_profile() {
	// Rust profile is 6 GiB / 15 min; Hostile ceiling is 2 GiB / 5 min.
	let sealed = acquiring_job(NetGrant::Off, ProducerProfile::Rust).seal(ThreatTier::Hostile);
	let r = sealed.budget().resources;
	assert!(
		r.mem_bytes.get() <= 2 * 1024 * 1024 * 1024,
		"hostile mem clamp"
	);
	assert!(r.wall.as_secs() <= 5 * 60, "hostile wall clamp");
	assert!(r.cpu_secs.get() <= 300, "hostile cpu clamp");
}

#[test]
fn trusted_tier_passes_profile_through() {
	// Trusted has no ceiling: the profile ceilings stand unclamped.
	let profile = ProducerProfile::Rust;
	let sealed = acquiring_job(NetGrant::Off, profile).seal(ThreatTier::Trusted);
	let r = sealed.budget().resources;
	assert_eq!(r.mem_bytes, profile.limits().mem_bytes);
	assert_eq!(r.wall, profile.limits().wall);
}

#[test]
fn untrusted_tier_leaves_normal_profiles_untouched() {
	// The Untrusted ceiling (8 GiB / 20 min) is above every real profile, so a
	// legitimate compile is never clamped.
	for profile in [ProducerProfile::Rust, ProducerProfile::Go, ProducerProfile::Java] {
		let sealed = acquiring_job(NetGrant::Off, profile).seal(ThreatTier::Untrusted);
		let r = sealed.budget().resources;
		assert_eq!(
			r.mem_bytes,
			profile.limits().mem_bytes,
			"untrusted must not clamp a normal profile"
		);
	}
}
