//! Escape-test suite, retargeted at the VM boundary (SMOLVM-PLAN §8 P0).
//!
//! Under `DevPassthrough` these assert best-effort behaviour (env scrub,
//! output caps, wall kills). The former bwrap-internals assertions are now
//! projection assertions: the security claims live in what the machine is
//! *built* with (no NIC when sealed, only granted roots mounted, scratch an
//! ephemeral overlay). The ignored tests below exercise the real backend when
//! the strict VM runner is invoked.

use std::path::PathBuf;
use std::time::Duration;

use sandbox::{
    CancelToken, CapabilityBudget, Env, FsGrant, KillReason, Limits, Mounts, NetGrant, Network,
    Output, ProducerProfile, SandboxError, SealedCommand, Spec, run_sealed,
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

/// Build a `SealedCommand` from a `Spec` for test purposes.
///
/// Uses the first writable mount as scratch (or temp dir as fallback).
fn spec_to_sealed(spec: Spec) -> SealedCommand {
    let scratch = spec
        .mounts
        .writable
        .first()
        .cloned()
        .unwrap_or_else(std::env::temp_dir);
    let mut fs = FsGrant::scratch(&scratch);
    for p in &spec.mounts.read_only {
        fs = fs.ro(p);
    }
    for p in spec.mounts.writable.iter().skip(1) {
        fs = fs.rw(p);
    }
    let budget = CapabilityBudget::new(fs, NetGrant::from(spec.network), spec.env, spec.limits);
    let mut cmd = SealedCommand::new(spec.command, spec.args, budget);
    if let Some(cwd) = spec.cwd {
        cmd = cmd.cwd(cwd);
    }
    cmd
}

/// Run a `Spec` via the platform cage (thin test helper).
fn run(spec: Spec) -> Result<Output, SandboxError> {
    run_sealed(spec_to_sealed(spec), &CancelToken::never()).map_err(Into::into)
}

fn echo_spec(arg: &str) -> Spec {
    Spec::new(echo_bin(), tiny_limits())
        .arg(arg)
        .env(Env::empty().set("PATH", "/usr/bin:/bin:/nix/var/nix/profiles/default/bin"))
        .mounts(Mounts::new())
}

#[test]
#[ignore = "requires NUDOX_GUEST_ROOTFS + smolvm binary on PATH (real VM)"]
fn trivial_echo_runs() {
    let out = run(echo_spec("hello-sandbox")).expect("echo should run");
    assert!(
        out.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("hello-sandbox"));
}

#[test]
#[ignore = "requires NUDOX_GUEST_ROOTFS + smolvm binary on PATH (real VM)"]
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
    let o = out.expect("real VM env probe must execute in the guest");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        !combined.contains("should-not-leak"),
        "secret leaked into guest env: {combined}"
    );
    unsafe {
        std::env::remove_var("NUDOX_SECRET_TOKEN");
    }
}

#[test]
#[ignore = "requires NUDOX_GUEST_ROOTFS + smolvm binary on PATH (real VM)"]
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

    // The strict VM rootfs must provide the POSIX utility used by this test.
    let program = "yes";

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
#[ignore = "requires NUDOX_GUEST_ROOTFS + smolvm binary on PATH (real VM)"]
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
fn dev_passthrough_refuses_production_policy() {
    // DevPassthrough cannot be constructed under Policy::Production.
    use sandbox::{CageError, DevPassthrough, Policy};
    let result = DevPassthrough::try_new(Policy::Production);
    assert!(
        matches!(result, Err(CageError::Denied { .. })),
        "expected Denied, got {result:?}"
    );
}

/// Retargeted from the bwrap FS-escape test: under the VM cage the guest can
/// only see what the projection *mounts*. `/etc/shadow` is unreadable not
/// because a bind was skipped, but because no virtiofs mount for it exists in
/// the machine config — and the only writable surface is a self-destroying
/// overlay, never a host path.
#[test]
fn vm_fs_visibility_is_exactly_the_grant() {
    use sandbox::{CapabilityBudget, OverlayMode, project_vm_config};

    let budget = CapabilityBudget::new(
        FsGrant::scratch("/tmp/job-scratch").ro("/pkg/src"),
        NetGrant::Off,
        Env::empty(),
        tiny_limits(),
    );
    let cfg = project_vm_config(&budget).expect("project");

    assert_eq!(cfg.mounts.len(), 1, "only granted roots are mounted");
    assert!(cfg.mounts[0].read_only);
    assert_eq!(cfg.mounts[0].host_path, PathBuf::from("/pkg/src"));
    assert_eq!(
        cfg.scratch.mode,
        OverlayMode::Ephemeral,
        "the writable surface is a disposable overlay, not a host bind"
    );
}

/// Retargeted from the bwrap empty-netns test: a sealed run's machine is
/// *constructed* with `NetworkPolicy::None` — there is no NIC for an
/// exfiltration attempt to use, on every platform, verified end-to-end
/// through the `Cage` trait against the recording runtime.
#[test]
fn vm_sealed_network_is_absent_from_the_machine() {
    use sandbox::{Cage, CapabilityBudget, FakeVmRuntime, NetworkPolicy, SmolvmCage};

    let rt = FakeVmRuntime::new();
    let cage = SmolvmCage::with_runtime(rt.clone());
    let budget = CapabilityBudget::new(
        FsGrant::scratch("/tmp/job-scratch"),
        NetGrant::Off,
        Env::empty().set("PATH", "/usr/bin:/bin"),
        tiny_limits(),
    );
    let cmd = SealedCommand::new("/usr/bin/curl", ["http://1.1.1.1/"], budget);

    cage.run(cmd, &CancelToken::never()).expect("fake run");
    let launched = rt.launched();
    assert_eq!(launched.len(), 1);
    assert_eq!(
        launched[0].network,
        NetworkPolicy::None,
        "sealed ⇒ the machine has no network device at all"
    );
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
