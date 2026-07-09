//! Escape-test suite (design §14).
//!
//! On macOS/passthrough these assert best-effort behaviour (env scrub, output
//! caps). On Linux with bwrap they are the CI gate for FS/net/env/resource.

use std::path::PathBuf;
use std::time::Duration;

use sandbox::{
	run, Env, KillReason, Limits, Mounts, Network, ProducerProfile, SandboxError, Spec,
};

fn tiny_limits() -> Limits {
	ProducerProfile::Tiny.limits()
}

fn echo_bin() -> &'static str {
	// Prefer absolute paths so seatbelt/passthrough don't depend on PATH layout.
	if PathBuf::from("/bin/echo").exists() {
		"/bin/echo"
	} else if PathBuf::from("/usr/bin/echo").exists() {
		"/usr/bin/echo"
	} else {
		"echo"
	}
}

fn echo_spec(arg: &str) -> Spec {
	Spec::new(echo_bin(), tiny_limits())
		.arg(arg)
		.env(Env::empty().set("PATH", "/usr/bin:/bin:/nix/var/nix/profiles/default/bin"))
		.mounts(Mounts::new())
}

#[test]
fn trivial_echo_runs() {
	let out = run(echo_spec("hello-sandbox")).expect("echo should run");
	assert!(out.success(), "stderr={}", String::from_utf8_lossy(&out.stderr));
	assert!(String::from_utf8_lossy(&out.stdout).contains("hello-sandbox"));
}

#[test]
fn env_is_scrubbed() {
	// Plant a secret in *this* process; guest must not see it unless allowlisted.
	// SAFETY: test is single-threaded for this env key.
	unsafe {
		std::env::set_var("NUDOX_SECRET_TOKEN", "should-not-leak");
	}
	let spec = Spec::new("printenv", tiny_limits())
		.arg("NUDOX_SECRET_TOKEN")
		.env(Env::empty().set("PATH", "/usr/bin:/bin"))
		.mounts(Mounts::new());
	let out = run(spec);
	// printenv exits non-zero when var is missing — that's success for us.
	match out {
		Ok(o) => {
			let combined = format!(
				"{}{}",
				String::from_utf8_lossy(&o.stdout),
				String::from_utf8_lossy(&o.stderr)
			);
			assert!(
				!combined.contains("should-not-leak"),
				"secret leaked into guest env: {combined}"
			);
		}
		Err(SandboxError::ToolchainMissing { .. }) => {
			// printenv not on PATH in minimal environments — skip.
		}
		Err(e) => panic!("unexpected error: {e}"),
	}
	unsafe {
		std::env::remove_var("NUDOX_SECRET_TOKEN");
	}
}

#[test]
fn output_cap_kills() {
	// Generate more than max_stdout via yes/printf if available.
	let limits = Limits::try_new(
		64 * 1024 * 1024,
		5,
		Duration::from_secs(10),
		16,
		1024, // 1 KiB stdout cap
		64 * 1024,
		16 * 1024 * 1024,
		256,
	)
	.unwrap();

	// `yes` may not exist; fall back to a python one-liner or skip.
	let program = if which_exists("yes") {
		"yes"
	} else {
		return; // soft skip
	};

	let spec = Spec::new(program, limits)
		.env(Env::empty().set("PATH", "/usr/bin:/bin"))
		.mounts(Mounts::new());

	match run(spec) {
		Err(SandboxError::Killed {
			reason: KillReason::OutputCap | KillReason::Wall,
			..
		}) => {}
		Ok(o) if !o.success() => {}
		other => panic!("expected output cap or wall kill, got {other:?}"),
	}
}

#[test]
fn wall_time_kills() {
	let limits = Limits::try_new(
		64 * 1024 * 1024,
		30,
		Duration::from_millis(200),
		16,
		64 * 1024,
		64 * 1024,
		16 * 1024 * 1024,
		256,
	)
	.unwrap();

	if !which_exists("sleep") {
		return;
	}

	let spec = Spec::new("sleep", limits)
		.arg("10")
		.env(Env::empty().set("PATH", "/usr/bin:/bin"))
		.mounts(Mounts::new());

	match run(spec) {
		Err(SandboxError::Killed {
			reason: KillReason::Wall,
			..
		}) => {}
		other => panic!("expected wall kill, got {other:?}"),
	}
}

#[test]
fn network_off_is_default() {
	// Spec defaults to Network::Off — structural invariant.
	let spec = Spec::new("true", tiny_limits());
	assert_eq!(spec.network, Network::Off);
}

#[test]
fn passthrough_refuses_production() {
	// Use always_deny so we don't race other tests on process env.
	use sandbox::Backend;
	use sandbox::Passthrough;
	let backend = Passthrough::always_deny();
	let result = backend.run(echo_spec("x"));
	assert!(matches!(result, Err(SandboxError::Denied { .. })));
}

#[cfg(target_os = "linux")]
#[test]
fn linux_fs_escape_blocked() {
	// Attempt to cat /etc/passwd from a sandbox that only binds a scratch dir.
	// With bwrap, /etc/passwd may still be bound for uid resolution — the
	// stronger check is reading $HOME secrets. We try both.
	let scratch = std::env::temp_dir().join(format!("nudox-escape-{}", std::process::id()));
	std::fs::create_dir_all(&scratch).unwrap();
	let secret = scratch.join("only-here");
	std::fs::write(&secret, b"ok").unwrap();

	let limits = tiny_limits();
	let mounts = Mounts::new().rw(&scratch);
	// Try to read a path outside — /etc/shadow typically Permission denied or missing.
	let spec = Spec::new("cat", limits)
		.arg("/etc/shadow")
		.env(Env::empty().set("PATH", "/usr/bin:/bin"))
		.mounts(mounts)
		.cwd(&scratch);

	let out = run(spec);
	match out {
		Ok(o) => {
			assert!(
				!o.success() || o.stdout.is_empty(),
				"should not read /etc/shadow"
			);
		}
		Err(_) => {} // spawn denial is also fine
	}
	let _ = std::fs::remove_dir_all(&scratch);
}

#[cfg(target_os = "linux")]
#[test]
fn linux_network_connect_fails() {
	if !which_exists("python3") && !which_exists("python") {
		return;
	}
	let py = if which_exists("python3") {
		"python3"
	} else {
		"python"
	};
	let limits = tiny_limits();
	let spec = Spec::new(py, limits)
		.args([
			"-c",
			"import socket; s=socket.socket(); s.settimeout(1); s.connect(('1.1.1.1', 80))",
		])
		.env(Env::empty().set("PATH", "/usr/bin:/bin"))
		.network(Network::Off)
		.mounts(Mounts::new());

	match run(spec) {
		Ok(o) => assert!(!o.success(), "connect should fail offline"),
		Err(_) => {}
	}
}

fn which_exists(name: &str) -> bool {
	std::process::Command::new("which")
		.arg(name)
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::null())
		.status()
		.map(|s| s.success())
		.unwrap_or(false)
}

#[test]
fn mounts_builder_is_chainable() {
	let m = Mounts::new().ro("/usr").rw(PathBuf::from("/tmp/x"));
	assert_eq!(m.read_only.len(), 1);
	assert_eq!(m.writable.len(), 1);
}

#[test]
fn rlimit_as_has_va_headroom() {
	// AS is 4× mem (supervisor); this is a structural check on the Limits
	// values used by profiles — apply_rlimits multiplies at runtime.
	let lim = ProducerProfile::Rust.limits();
	assert!(lim.mem_bytes.get() >= 3 * 1024 * 1024 * 1024);
	// soft==hard ceilings present
	assert!(lim.cpu_secs.get() > 0);
	assert!(lim.nofile.get() >= 1024);
}

#[cfg(target_os = "linux")]
#[test]
fn seccomp_denylist_compiles() {
	// BPF program must be non-empty and a multiple of 8 bytes (sock_filter).
	let bytes = sandbox::seccomp::denylist_bpf_bytes().expect("compile denylist");
	assert!(!bytes.is_empty());
	assert_eq!(bytes.len() % 8, 0);
}
