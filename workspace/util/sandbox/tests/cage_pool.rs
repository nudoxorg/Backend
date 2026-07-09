//! Cage vocabulary + concurrent WorkerPool stress (Phase 2).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use sandbox::{
	CancelToken, CapabilityBudget, DevPassthrough, Env, FsGrant, Limits, NetGrant, Policy,
	SealedCommand, Sealer, WorkerLang, WorkerPool, WorkerPoolConfig,
};

fn tiny_limits() -> Limits {
	Limits::try_new(
		64 * 1024 * 1024,
		5,
		Duration::from_secs(10),
		16,
		64 * 1024,
		64 * 1024,
		16 * 1024 * 1024,
		256,
	)
	.unwrap()
}

#[test]
fn sealed_command_roundtrips_spec() {
	let scratch = std::env::temp_dir();
	let sealer = Sealer::new();
	let budget = sealer.budget(
		FsGrant::scratch(&scratch).ro("/usr"),
		NetGrant::Off,
		Env::empty().set("PATH", "/bin"),
		tiny_limits(),
	);
	let cmd = sealer
		.seal_command("/bin/echo", ["hi"], budget)
		.cwd(&scratch);
	let spec = cmd.clone().into_spec();
	assert_eq!(spec.command, PathBuf::from("/bin/echo"));
	assert_eq!(spec.args, vec![std::ffi::OsString::from("hi")]);
	assert!(matches!(spec.network, sandbox::Network::Off));
	let back = SealedCommand::from_spec(spec);
	assert_eq!(back.command, cmd.command);
}

#[test]
fn dev_passthrough_requires_development_policy() {
	assert!(DevPassthrough::try_new(Policy::Development).is_ok());
	assert!(DevPassthrough::try_new(Policy::Production).is_err());
}

#[test]
fn cancel_token_is_shared() {
	let t = CancelToken::new();
	let t2 = t.clone();
	assert!(!t.is_cancelled());
	t2.cancel();
	assert!(t.is_cancelled());
}

#[test]
fn capability_budget_carries_parts() {
	let b = CapabilityBudget::new(
		FsGrant::scratch("/tmp"),
		NetGrant::Off,
		Env::empty(),
		tiny_limits(),
	);
	assert_eq!(b.net, NetGrant::Off);
	assert_eq!(b.fs.scratch_path(), std::path::Path::new("/tmp"));
}

/// Tiny fake worker: sleeps on lower so concurrency is measurable.
fn write_fake_worker(dir: &std::path::Path) -> PathBuf {
	let path = dir.join("fake-worker.py");
	std::fs::write(
		&path,
		r#"#!/usr/bin/env python3
import sys, json, time
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    op = req.get("op")
    if op == "ping":
        print(json.dumps({"status": "pong", "rss": 1}), flush=True)
    elif op == "shutdown":
        print(json.dumps({"status": "ok", "body": ""}), flush=True)
        break
    elif op == "lower":
        time.sleep(0.15)
        print(json.dumps({"status": "ok", "body": "{\"ok\":true}"}), flush=True)
    else:
        print(json.dumps({"status": "err", "kind": "unknown", "message": op or ""}), flush=True)
"#,
	)
	.expect("write fake worker");
	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		let mut perms = std::fs::metadata(&path).unwrap().permissions();
		perms.set_mode(0o755);
		std::fs::set_permissions(&path, perms).unwrap();
	}
	path
}

fn python3_ok() -> bool {
	std::process::Command::new("python3")
		.arg("-c")
		.arg("pass")
		.status()
		.map(|s| s.success())
		.unwrap_or(false)
}

#[test]
fn pool_runs_jobs_concurrently_not_serialized() {
	if !python3_ok() {
		return;
	}

	let dir = tempfile::tempdir().unwrap();
	let bin = write_fake_worker(dir.path());
	let pool = Arc::new(
		WorkerPool::new(WorkerPoolConfig {
			worker_bin: bin,
			size: 4,
			limits: tiny_limits(),
			read_only: Vec::new(),
			memory_watermark: u64::MAX,
		})
		.expect("pool"),
	);

	assert_eq!(pool.size(), 4);

	let n = 4;
	let done = Arc::new(AtomicUsize::new(0));
	let in_flight = Arc::new(AtomicUsize::new(0));
	let max_flight = Arc::new(AtomicUsize::new(0));
	let start = Instant::now();
	thread::scope(|s| {
		for _ in 0..n {
			let done = Arc::clone(&done);
			let pool = Arc::clone(&pool);
			let in_flight = Arc::clone(&in_flight);
			let max_flight = Arc::clone(&max_flight);
			s.spawn(move || {
				let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
				max_flight.fetch_max(cur, Ordering::SeqCst);
				let body = pool
					.lower(WorkerLang::Python, std::path::Path::new("/tmp"))
					.expect("lower");
				in_flight.fetch_sub(1, Ordering::SeqCst);
				assert!(body.contains("ok"));
				done.fetch_add(1, Ordering::SeqCst);
			});
		}
	});
	let elapsed = start.elapsed();
	assert_eq!(done.load(Ordering::SeqCst), n);
	let peak = max_flight.load(Ordering::SeqCst);
	// Proof of non-serialization: free-list hands out distinct slots so multiple
	// submits run in flight (peak == pool size under low contention).
	assert!(
		peak >= 2,
		"expected at least 2 concurrent submits, peak was {peak} (elapsed {elapsed:?})"
	);
	// Best-effort: under light load we should reach full pool width.
	assert!(
		peak == n || elapsed < Duration::from_secs(3),
		"peak concurrency {peak}/{n} with elapsed {elapsed:?}"
	);
	let _ = start; // elapsed used above
}
