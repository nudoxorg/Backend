//! Phase-6 seal-path invariants (buck2-runnable integration tests).
//!
//! Proves the *positive* wiring the DAEMON-PLAN Phase-6 asks for:
//!  - `ThreatTier` clamps the sealed budget on the `seal_package` path.
//!  - A per-package `SandboxKey::Package(..)` override resolves through
//!    `OverrideTable::resolve` at seal time and changes the sealed limits.
//!  - `NixCaps::hermetic()` builds a working, still-pure evaluator.
//!
//! The *negative* invariants (net-on in `Job<Sealed>`, `enable_impure` on
//! `NixCaps<Hermetic>`) are `compile_fail` doctests on the types themselves —
//! see the module note in `sandbox/tests/typestate.rs` for why the buck2
//! doctest runner cannot execute them here (rustc toolchain mismatch, E0514).

use std::collections::HashMap;
use std::num::{NonZeroU32, NonZeroU64};

use compiler::languages::producer::{ThreatTier, seal_package};
use sandbox::{
	LimitOverride, NetGrant, OverrideTable, ProducerProfile, SandboxKey, ToolchainSet,
};

fn zero_hash() -> heart::ContentHash {
	heart::ContentHash::of_bytes(b"")
}

#[test]
fn package_override_changes_sealed_limits() {
	let toolchains = ToolchainSet::empty();
	let tmp = tempfile::tempdir().unwrap();

	let base = seal_package(
		&toolchains,
		&OverrideTable::empty(),
		tmp.path(),
		ProducerProfile::Go,
		ThreatTier::Untrusted,
		None,
		zero_hash(),
		zero_hash(),
	)
	.unwrap();
	let base_pids = base.input().budget.resources.pids.get();

	let key = SandboxKey::package("crates.io", "serde", "1.0.0");
	let mut map = HashMap::new();
	map.insert(
		"crates.io/serde@1.0.0".to_string(),
		LimitOverride {
			pids: NonZeroU32::new(base_pids + 11),
			..LimitOverride::none()
		},
	);
	let table = OverrideTable::from_config(map);

	let sealed = seal_package(
		&toolchains,
		&table,
		tmp.path(),
		ProducerProfile::Go,
		ThreatTier::Untrusted,
		Some(&key),
		zero_hash(),
		zero_hash(),
	)
	.unwrap();
	assert_eq!(
		sealed.input().budget.resources.pids.get(),
		base_pids + 11,
		"package-scoped override must change the resolved sealed limits"
	);
}

#[test]
fn hostile_tier_clamps_sealed_budget() {
	let toolchains = ToolchainSet::empty();
	let tmp = tempfile::tempdir().unwrap();
	let sealed = seal_package(
		&toolchains,
		&OverrideTable::empty(),
		tmp.path(),
		ProducerProfile::Rust, // 6 GiB / 15 min
		ThreatTier::Hostile,   // ceiling 2 GiB / 5 min
		None,
		zero_hash(),
		zero_hash(),
	)
	.unwrap();
	let r = &sealed.input().budget.resources;
	assert!(r.mem_bytes.get() <= 2 * 1024 * 1024 * 1024);
	assert!(r.wall.as_secs() <= 5 * 60);
	assert_eq!(sealed.input().budget.net, NetGrant::Off);
}

#[test]
fn tier_clamps_even_a_looser_package_override() {
	let toolchains = ToolchainSet::empty();
	let tmp = tempfile::tempdir().unwrap();
	let key = SandboxKey::package("npm", "evil", "9.9.9");
	let mut map = HashMap::new();
	map.insert(
		"npm/evil@9.9.9".to_string(),
		LimitOverride {
			mem_bytes: NonZeroU64::new(64 * 1024 * 1024 * 1024), // 64 GiB attempt
			..LimitOverride::none()
		},
	);
	let table = OverrideTable::from_config(map);
	let sealed = seal_package(
		&toolchains,
		&table,
		tmp.path(),
		ProducerProfile::StaticParser,
		ThreatTier::Hostile,
		Some(&key),
		zero_hash(),
		zero_hash(),
	)
	.unwrap();
	assert!(
		sealed.input().budget.resources.mem_bytes.get() <= 2 * 1024 * 1024 * 1024,
		"tier ceiling must clamp a package override that tries to exceed it"
	);
}

#[test]
fn nix_caps_hermetic_builds_and_stays_pure() {
	use std::rc::Rc;

	use compiler::languages::nix::eval::{DocsIO, NixCaps};
	use snix_eval::EvalIO;

	let io: Rc<dyn EvalIO> = Rc::new(DocsIO::new(std::env::temp_dir(), None));
	let evaluation = NixCaps::hermetic().build(io);
	let result = evaluation.evaluate("1 + 1", None);
	assert!(result.errors.is_empty());
	assert!(matches!(result.value, Some(snix_eval::Value::Integer(2))));

	// getEnv still absent under hermetic caps (runtime backstop).
	unsafe {
		std::env::set_var("AWS_SECRET_ACCESS_KEY", "must-not-appear-caps");
	}
	let io2: Rc<dyn EvalIO> = Rc::new(DocsIO::new(std::env::temp_dir(), None));
	let ev2 = NixCaps::hermetic().build(io2);
	let r2 = ev2.evaluate(r#"builtins.getEnv "AWS_SECRET_ACCESS_KEY""#, None);
	if let Some(v) = &r2.value {
		assert!(!format!("{v:?}").contains("must-not-appear-caps"));
	}
	unsafe {
		std::env::remove_var("AWS_SECRET_ACCESS_KEY");
	}
}
