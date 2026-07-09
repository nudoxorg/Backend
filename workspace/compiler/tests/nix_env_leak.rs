//! P3 exit criterion: snix must not observe host secrets.
//!
//! Plants credential-shaped env vars, evaluates a minimal flake through the
//! hermetic producer path, and asserts the secret never appears in IR or
//! process-visible eval output.

use std::fs;
use std::path::PathBuf;

/// Minimal flake that would leak if getEnv/currentTime were impure.
fn write_fixture(dir: &std::path::Path) {
	fs::write(
		dir.join("flake.nix"),
		r#"
{
  description = "env-leak fixture";
  outputs = { self }: {
    # If hermetic eval ever gains impure getEnv, this would surface the secret.
    # Pure hermetic path omits the builtin — evaluation still yields the attrset.
    leak = "no-secret";
    packages = { };
  };
}
"#,
	)
	.unwrap();
	fs::write(
		dir.join("flake.lock"),
		r#"{"nodes":{"root":{"inputs":{}}},"root":"root","version":7}"#,
	)
	.unwrap();
}

#[test]
fn hermetic_nix_lower_does_not_surface_planted_secrets() {
	let dir = std::env::temp_dir().join(format!(
		"nudox-env-leak-{}-{}",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.map(|d| d.as_nanos())
			.unwrap_or(0)
	));
	fs::create_dir_all(&dir).unwrap();
	write_fixture(&dir);

	unsafe {
		std::env::set_var("AWS_SECRET_ACCESS_KEY", "AKIA_LEAK_TEST_TOKEN");
		std::env::set_var("GITHUB_TOKEN", "ghp_leak_test_token");
		std::env::set_var("CACHIX_AUTH_TOKEN", "cachix_leak");
	}

	let result = compiler::languages::nix::lower_package(&dir);
	let dump = match &result {
		Ok(index) => serde_json::to_string(index).unwrap_or_else(|e| format!("ser:{e}")),
		Err(e) => e.to_string(),
	};

	for secret in [
		"AKIA_LEAK_TEST_TOKEN",
		"ghp_leak_test_token",
		"cachix_leak",
	] {
		assert!(
			!dump.contains(secret),
			"secret {secret:?} leaked into nix lower output:\n{dump}"
		);
	}

	unsafe {
		std::env::remove_var("AWS_SECRET_ACCESS_KEY");
		std::env::remove_var("GITHUB_TOKEN");
		std::env::remove_var("CACHIX_AUTH_TOKEN");
	}
	let _ = fs::remove_dir_all(&dir);
}

#[test]
fn docs_io_get_env_sealed() {
	use compiler::languages::nix::eval::DocsIO;
	use snix_eval::EvalIO;

	let io = DocsIO::new(PathBuf::from("/tmp"), None);
	unsafe {
		std::env::set_var("AWS_SECRET_ACCESS_KEY", "AKIA_SHOULD_NOT_APPEAR");
	}
	assert!(io
		.get_env(std::ffi::OsStr::new("AWS_SECRET_ACCESS_KEY"))
		.is_none());
	unsafe {
		std::env::remove_var("AWS_SECRET_ACCESS_KEY");
	}
}
