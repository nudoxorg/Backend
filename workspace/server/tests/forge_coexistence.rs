//! Two `ForgeRuntime`s coexist in one process with independent CAS + observers
//! (DAEMON-PLAN §5 Phase 4 gate — the multi-tenant smoke test).
//!
//! Proves the compile plane holds no process globals: distinct node ids,
//! isolated content-addressed stores, and independent observer counters.

use std::sync::Arc;

use bytes::Bytes;
use cas::ContentHash;
use compiler::languages::producer::ForgeContext;
use sandbox::{
	Cage, CageCaps, CageId, CancelToken, CapabilityBudget, Captured, CountingObserver, Policy,
	ProcessEnd, SealedCommand,
};
use server::forge::{ForgeConfig, ForgeRuntime};

/// A test-double cage that never spawns anything: every run "succeeds" with an
/// empty capture. Proves the `Cage` boundary is injectable.
#[derive(Debug, Default)]
struct FakeCage {
	runs: std::sync::atomic::AtomicU64,
}

impl Cage for FakeCage {
	fn id(&self) -> CageId {
		CageId("fake-cage")
	}
	fn capabilities(&self) -> CageCaps {
		CageCaps::default()
	}
	fn run(&self, _cmd: SealedCommand, _cancel: &CancelToken) -> Result<Captured, sandbox::CageError> {
		self.runs
			.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		Ok(Captured {
			stdout: Vec::new(),
			stderr: Vec::new(),
			end: ProcessEnd::Exited(exited_zero()),
			wall: std::time::Duration::ZERO,
			peak_mem: None,
		})
	}
}

#[cfg(unix)]
fn exited_zero() -> std::process::ExitStatus {
	use std::os::unix::process::ExitStatusExt;
	std::process::ExitStatus::from_raw(0)
}

#[test]
fn fake_cage_is_a_usable_cage() {
	// The FakeCage double satisfies the trait and is exercisable in isolation.
	let cage = FakeCage::default();
	assert_eq!(cage.id().as_str(), "fake-cage");
	let budget = CapabilityBudget::new(
		sandbox::FsGrant::scratch(std::env::temp_dir()),
		sandbox::NetGrant::Off,
		sandbox::Env::empty(),
		sandbox::ProducerProfile::Tiny.limits(),
	);
	let cmd = SealedCommand::new("/bin/true", Vec::<String>::new(), budget);
	let out = cage.run(cmd, &CancelToken::never()).expect("fake run succeeds");
	assert!(out.success());
	assert_eq!(cage.runs.load(std::sync::atomic::Ordering::Relaxed), 1);
}

/// Build a development `ForgeRuntime` with a memory CAS and a distinct observer.
fn dev_runtime(observer: Arc<CountingObserver>) -> Arc<ForgeRuntime> {
	let cfg = ForgeConfig {
		node: None, // from_host → distinct per instance
		overrides: Default::default(),
		cas_root: None, // memory-only, isolated per runtime
		observer,
		l3: None, // development: no distributed L3
	};
	Arc::new(
		ForgeRuntime::assemble(Policy::Development, cfg, tokio::runtime::Handle::current())
			.expect("dev runtime assembles"),
	)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_runtimes_coexist_with_independent_state() {
	let obs_a = Arc::new(CountingObserver::default());
	let obs_b = Arc::new(CountingObserver::default());

	let a = dev_runtime(obs_a.clone());
	let b = dev_runtime(obs_b.clone());

	// Distinct node identities (no shared process::id-derived global).
	assert_ne!(
		a.node().as_str(),
		b.node().as_str(),
		"each runtime must have a distinct node id"
	);

	// Put a blob in A's CAS; it must NOT be visible through B's CAS.
	let key = ContentHash::of_bytes(b"only-in-a");
	a.cas()
		.put_keyed(key, Bytes::from_static(b"payload"))
		.await
		.expect("put into A");

	assert!(
		a.cas().get(key).await.expect("A get").is_some(),
		"A's own CAS sees its blob"
	);
	assert!(
		b.cas().get(key).await.expect("B get").is_none(),
		"B's CAS must not see A's blob — the stores are independent"
	);

	// Observers are independent: recording on A leaves B at zero.
	a.observer().cache_hit();
	a.observer().cache_hit();
	b.observer().cache_miss();

	use std::sync::atomic::Ordering::Relaxed;
	assert_eq!(obs_a.cache_hits.load(Relaxed), 2);
	assert_eq!(obs_a.cache_misses.load(Relaxed), 0);
	assert_eq!(obs_b.cache_hits.load(Relaxed), 0);
	assert_eq!(obs_b.cache_misses.load(Relaxed), 1);
}
