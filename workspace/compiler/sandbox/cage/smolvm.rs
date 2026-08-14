//! `SmolvmCage` — the one production cage: a microVM per compile.
//!
//! Implements [`Cage`] by *projecting* a [`CapabilityBudget`] into a
//! [`VmConfig`] + [`RunSpec`] per the normative SMOLVM-PLAN §3.1 table:
//!
//! | Budget field | VM projection |
//! |---|---|
//! | `FsGrant.read_only` roots | RO virtiofs mounts, one tag per root |
//! | `FsGrant.scratch` | ephemeral qcow2 overlay (auto-destroyed) |
//! | `FsGrant.writable` extras | RW virtiofs mounts (discouraged — SV-13) |
//! | `NetGrant::Off` | [`NetworkPolicy::None`] — no NIC, vsock only |
//! | `NetGrant::On(allowlist)` | [`NetworkPolicy::Egress`] + DNS filter |
//! | `Env` allowlist | [`RunSpec::env`] verbatim (never ambient) |
//! | `Limits.mem_bytes` | `memory_mib` (balloon reclaims idle) + rlimit |
//! | `Limits.wall` | [`RunSpec::timeout`]; cancel → [`VmHandle::kill`] |
//! | `Limits.{cpu,pids,nofile,fsize}` | in-guest rlimits |
//!
//! The budget layer is the contract; the VM is the mechanism (SV-2) — no
//! caller above [`Cage`] changes.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::Mutex;

use crate::budget::{CapabilityBudget, NetGrant};
use crate::cage::{Cage, CageCaps, CageId};
use crate::error::CageError;
use crate::job::cancel::CancelToken;
use crate::job::NetOff;
use crate::seal::SealedCommand;
use crate::spec::Output;
use crate::vm::{
    DnsPolicy, EgressPolicy, GoldenId, GuestRlimits, MountTag, NetworkPolicy, OverlayMode, RunSpec,
    ScratchOverlay, StreamPort, VirtiofsMount, VmConfig, VmHandle, VmRuntime, VsockPort,
};

use crate::toolchains::images::ImageDigest;

/// Golden checkpoints keyed by toolchain-image digest (SV-11).
///
/// A new image digest ⇒ a new golden; stale goldens are simply never hit
/// again and can be GC'd by the runtime layer.
#[derive(Debug, Default)]
pub struct GoldenPool {
    inner: Mutex<HashMap<ImageDigest, GoldenId>>,
}

impl GoldenPool {
    /// Empty pool.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up the golden for an image digest.
    pub fn lookup(&self, image: &ImageDigest) -> Option<GoldenId> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(image)
            .cloned()
    }

    /// Install (or replace) the golden for an image digest.
    pub fn install(&self, image: ImageDigest, golden: GoldenId) {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(image, golden);
    }

    /// Number of parked goldens.
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

// ─── Projection (normative §3.1) ────────────────────────────────────────────

/// Project a network grant into the VM network posture.
///
/// `Off` ⇒ [`NetworkPolicy::None`]: the machine is constructed without a
/// NIC — the network is absent, not filtered. `On` ⇒ egress under the
/// allowlist, with the DNS filter fed from the same allowlist (empty name
/// list ⇒ unrestricted DNS, the permissive acquiring default).
pub fn project_network(net: &NetGrant) -> NetworkPolicy {
    match net {
        NetGrant::Off => NetworkPolicy::None,
        NetGrant::On(allow) => NetworkPolicy::Egress(EgressPolicy {
            allowed_cidrs: allow.cidrs.clone(),
            dns: if allow.dns_names.is_empty() {
                DnsPolicy::AllowAll
            } else {
                DnsPolicy::Allowlist(allow.dns_names.clone())
            },
        }),
    }
}

/// The sealed network projection as a type-level fact: the only network
/// state a [`crate::job::SealedBudget`] can name is [`NetOff`], and the only
/// policy [`NetOff`] can become is [`NetworkPolicy::None`]. There is no code
/// path from a sealed budget to a machine with a NIC.
impl From<NetOff> for NetworkPolicy {
    fn from(_: NetOff) -> Self {
        NetworkPolicy::None
    }
}

/// Project a capability budget into a machine configuration, optionally
/// appending an IR-stream socket bridge for live symbol streaming (SMOLVM-PLAN §6).
///
/// - RO roots become RO virtiofs mounts (one tag per root, DAX on for fast
///   repeated reads of big source/toolchain trees).
/// - The scratch is an **ephemeral overlay**, not a host bind: nothing the
///   guest writes survives the VM (SV-13). Extra `writable` binds become RW
///   virtiofs mounts — supported but discouraged; artifacts should leave via
///   streams.
/// - `Limits.mem_bytes` rounds up to `memory_mib`; the virtio balloon
///   reclaims idle guest pages, so the ceiling is a cap, not a reservation.
/// - vCPU count is a scheduling knob, not a budget surface; it defaults to 1.
/// - When `stream` is `Some`, one [`VsockPort`] (host-listens, `listen: true`)
///   is appended to the config. The real backend translates this to a `Mount`
///   direction `PublishedSocketConfig`; the guest producer connects to
///   [`crate::vm::IR_STREAM_GUEST_SOCKET`] inside the VM.
pub fn project_vm_config_with_stream(
    budget: &CapabilityBudget,
    stream: Option<&crate::vm::StreamPort>,
) -> Result<VmConfig, CageError> {
    let mut builder = VmConfig::builder()
        .memory_mib(mem_mib(budget.resources.mem_bytes))
        .network(project_network(&budget.net))
        .scratch(ScratchOverlay {
            mode: OverlayMode::Ephemeral,
        });

    for (i, root) in budget.fs.read_only.iter().enumerate() {
        builder = builder.mount(VirtiofsMount {
            tag: MountTag::new(format!("ro{i}")).map_err(CageError::from)?,
            host_path: root.clone(),
            read_only: true,
            dax: true,
        });
    }
    for (i, path) in budget.fs.writable.iter().enumerate() {
        builder = builder.mount(VirtiofsMount {
            tag: MountTag::new(format!("rw{i}")).map_err(CageError::from)?,
            host_path: path.clone(),
            read_only: false,
            dax: false,
        });
    }

    if let Some(sp) = stream {
        builder = builder.vsock_port(VsockPort {
            port: sp.port,
            socket_path: sp.socket_path.clone(),
            listen: true,
        });
    }

    Ok(builder.build())
}

/// Project a capability budget into a machine configuration.
///
/// Delegates to [`project_vm_config_with_stream`] with `stream = None`.
pub fn project_vm_config(budget: &CapabilityBudget) -> Result<VmConfig, CageError> {
    project_vm_config_with_stream(budget, None)
}

/// Project a sealed command into the in-guest exec.
///
/// The env vector is the budget's allowlist **verbatim** — the guest never
/// sees the host environment. Wall clock becomes the exec timeout;
/// cpu/pids/nofile/fsize become in-guest rlimits applied by the agent, and
/// the memory rlimit rides along as belt-and-braces under the VM ceiling.
pub fn project_run_spec(cmd: &SealedCommand) -> RunSpec {
    let limits = cmd.budget.resources;
    RunSpec {
        command: cmd.command.clone(),
        args: cmd.args.clone(),
        env: cmd
            .budget
            .env
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.to_string_lossy().into_owned(),
                )
            })
            .collect(),
        cwd: cmd.cwd.clone(),
        timeout: limits.wall,
        rlimits: GuestRlimits {
            cpu_secs: limits.cpu_secs,
            pids: limits.pids,
            nofile: limits.nofile,
            fsize_bytes: limits.fsize_bytes,
            mem_bytes: limits.mem_bytes,
        },
    }
}

/// Round a byte ceiling up to MiB, never below 1.
fn mem_mib(bytes: NonZeroU64) -> NonZeroU64 {
    NonZeroU64::new(bytes.get().div_ceil(1024 * 1024)).unwrap_or(NonZeroU64::MIN)
}

// ─── The cage ───────────────────────────────────────────────────────────────

/// The production cage: one smolvm microVM per sealed run (SV-1).
///
/// Generic over [`VmRuntime`] so projection is testable against
/// [`crate::vm::FakeVmRuntime`]; production injects the embedded smolvm
/// runtime via [`SmolvmCage::with_runtime`] once the `smolvm-backend`
/// feature carries a real implementation.
#[derive(Debug)]
pub struct SmolvmCage<R: VmRuntime> {
    runtime: R,
    pool: GoldenPool,
}

impl<R: VmRuntime> SmolvmCage<R> {
    /// Build a cage over an injected VM runtime.
    pub fn with_runtime(runtime: R) -> Self {
        Self {
            runtime,
            pool: GoldenPool::new(),
        }
    }

    /// The golden checkpoint pool (SV-11).
    pub fn golden_pool(&self) -> &GoldenPool {
        &self.pool
    }

    /// The underlying runtime.
    pub fn runtime(&self) -> &R {
        &self.runtime
    }

    fn run_inner(
        &self,
        cmd: &SealedCommand,
        cancel: &CancelToken,
        stream: Option<&StreamPort>,
    ) -> Result<Output, CageError> {
        if cancel.is_cancelled() {
            return Err(CageError::Cancelled);
        }

        let cfg = project_vm_config_with_stream(&cmd.budget, stream)?;
        let spec = project_run_spec(cmd);

        let mut handle = self.runtime.launch(&cfg).map_err(CageError::from)?;
        if cancel.is_cancelled() {
            handle.kill();
            return Err(CageError::Cancelled);
        }

        // NOTE: mid-exec cancellation (CancelToken observed while the guest
        // runs) is wired by the real backend, which polls the token beside the
        // vsock exec channel and calls `kill` — the same contract as the exec
        // timeout. The projection layer enforces it at the boundaries.
        let result = handle.exec(&spec);
        if cancel.is_cancelled() {
            handle.kill();
            return Err(CageError::Cancelled);
        }

        // One VM per job: tear down; the ephemeral overlay dies with it.
        handle.kill();
        result.map_err(CageError::from)
    }

    /// Run a sealed command with an optional IR-stream port attached
    /// (SMOLVM-PLAN §6 / SV-8).
    ///
    /// When `stream` is `Some`, a `VsockPort { listen: true }` is appended to the
    /// VM config (via `project_vm_config_with_stream`). The real backend
    /// ([`crate::backend::smolvm::SmolvmRuntime`]) translates this into a
    /// `PublishedSocketConfig { direction: Mount, guest_path: IR_STREAM_GUEST_SOCKET,
    /// host_path: Some(stream.socket_path) }` threaded through
    /// `AgentManager::start_with_full_config` via `LaunchFeatures.published_sockets`.
    /// smolvm's guest agent creates a Unix listener at `IR_STREAM_GUEST_SOCKET`
    /// (`/run/nudox/ir-stream.sock`); the producer connects there and writes
    /// ir-stream frames; the agent relays each connection to the host socket.
    ///
    /// The host must already be listening on `stream.socket_path` before this is
    /// called (the host creates the listener; smolvm dials it on the guest's behalf).
    pub fn run_with_stream(
        &self,
        cmd: SealedCommand,
        stream: Option<&StreamPort>,
        cancel: &CancelToken,
    ) -> Result<Output, CageError> {
        self.run_inner(&cmd, cancel, stream)
    }
}

impl<R: VmRuntime> Cage for SmolvmCage<R> {
    fn id(&self) -> CageId {
        CageId("smolvm-microvm")
    }

    fn capabilities(&self) -> CageCaps {
        CageCaps {
            isolation: true,
            network_off: true,
            fs_scope: true,
            resource_limits: true,
            production_grade: true,
        }
    }

    fn run(&self, cmd: SealedCommand, cancel: &CancelToken) -> Result<Output, CageError> {
        self.run_inner(&cmd, cancel, None)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use heart::JobKey;

    use super::*;
    use crate::budget::{EgressAllowlist, FsGrant, ThreatTier};
    use crate::budget::profiles::ProducerProfile;
    use crate::job::{Job, VmForge};
    use crate::spec::Env;
    use crate::vm::{Cidr, DnsName, FakeVmRuntime, VmError};

    fn budget(fs: FsGrant, net: NetGrant) -> CapabilityBudget {
        CapabilityBudget::new(fs, net, Env::empty(), ProducerProfile::Tiny.limits())
    }

    fn sealed(fs: FsGrant, net: NetGrant) -> SealedCommand {
        SealedCommand::new("/usr/bin/producer", ["--lower"], budget(fs, net))
    }

    // §3.1 row: FsGrant RO roots → RO virtiofs mounts, one tag per root.
    #[test]
    fn ro_roots_project_to_ro_mounts_one_tag_each() {
        let fs = FsGrant::scratch("/tmp/scratch")
            .ro("/nix/store")
            .ro("/pkg/src");
        let cfg = project_vm_config(&budget(fs, NetGrant::Off)).expect("project");

        let ro: Vec<_> = cfg.mounts.iter().filter(|m| m.read_only).collect();
        assert_eq!(ro.len(), 2);
        assert_eq!(ro[0].host_path, PathBuf::from("/nix/store"));
        assert_eq!(ro[1].host_path, PathBuf::from("/pkg/src"));
        assert_ne!(ro[0].tag, ro[1].tag, "one tag per root");
        assert!(ro.iter().all(|m| m.dax), "RO trees get DAX windows");
    }

    // §3.1 row: scratch → ephemeral overlay, never a host bind.
    #[test]
    fn scratch_projects_to_ephemeral_overlay_not_a_mount() {
        let fs = FsGrant::scratch("/tmp/scratch");
        let cfg = project_vm_config(&budget(fs, NetGrant::Off)).expect("project");
        assert_eq!(cfg.scratch.mode, OverlayMode::Ephemeral);
        assert!(
            cfg.mounts
                .iter()
                .all(|m| m.host_path != std::path::Path::new("/tmp/scratch")),
            "scratch is the overlay, not a virtiofs bind"
        );
    }

    // §3.1 row: extra writable binds → RW virtiofs (supported, discouraged).
    #[test]
    fn extra_writable_binds_project_to_rw_mounts() {
        let fs = FsGrant::scratch("/tmp/scratch").rw("/tmp/out");
        let cfg = project_vm_config(&budget(fs, NetGrant::Off)).expect("project");
        let rw: Vec<_> = cfg.mounts.iter().filter(|m| !m.read_only).collect();
        assert_eq!(rw.len(), 1);
        assert_eq!(rw[0].host_path, PathBuf::from("/tmp/out"));
    }

    // §3.1 row: NetGrant::Off → NetworkPolicy::None.
    #[test]
    fn net_off_projects_to_no_network() {
        let cfg =
            project_vm_config(&budget(FsGrant::scratch("/tmp"), NetGrant::Off)).expect("project");
        assert_eq!(cfg.network, NetworkPolicy::None);
    }

    // §3.1 row: NetGrant::On(allowlist) → Egress projection + DNS filter.
    #[test]
    fn net_on_projects_allowlist_to_egress() {
        let cidr: Cidr = "140.82.112.0/20".parse().expect("cidr");
        let dns = DnsName::new("crates.io").expect("dns");
        let net = NetGrant::On(EgressAllowlist::new(vec![cidr], vec![dns.clone()]));
        match project_network(&net) {
            NetworkPolicy::Egress(egress) => {
                assert_eq!(egress.allowed_cidrs, vec![cidr]);
                assert_eq!(egress.dns, DnsPolicy::Allowlist(vec![dns]));
            }
            other => panic!("expected egress, got {other:?}"),
        }
    }

    #[test]
    fn permissive_grant_projects_wide_egress_with_open_dns() {
        match project_network(&NetGrant::permissive()) {
            NetworkPolicy::Egress(egress) => {
                assert_eq!(egress.allowed_cidrs, vec![Cidr::ANY_V4]);
                assert_eq!(egress.dns, DnsPolicy::AllowAll);
            }
            other => panic!("expected egress, got {other:?}"),
        }
    }

    // The sealed invariant: a budget that went through Job::seal can NEVER
    // project to anything but NetworkPolicy::None — even when acquisition ran
    // with the permissive grant. Type-level: NetOff → NetworkPolicy::None is
    // the only From impl a sealed budget's net field has.
    #[test]
    fn sealed_budget_can_only_project_network_none() {
        let key = JobKey::derive(b"smolvm-test", b"", b"root", b"");
        let job = Job::acquiring(
            key,
            "/pkg",
            FsGrant::scratch(std::env::temp_dir()),
            Env::empty(),
            ProducerProfile::Rust.limits(),
            NetGrant::permissive(), // acquiring had the network wide open
        )
        .seal(ThreatTier::Hostile);

        // Type-level: the sealed budget's net field is NetOff.
        let policy: NetworkPolicy = job.budget().net.into();
        assert_eq!(policy, NetworkPolicy::None);

        // Dynamic path: lowering through CapabilityBudget agrees.
        let input = job.into_input();
        let cfg = project_vm_config(&input.budget).expect("project");
        assert_eq!(cfg.network, NetworkPolicy::None);
    }

    // §3.1 row: Env allowlist → RunSpec env verbatim, never ambient.
    #[test]
    fn env_allowlist_is_the_entire_guest_env() {
        let env = Env::empty().set("PATH", "/usr/bin").set("HOME", "/tmp");
        let budget = CapabilityBudget::new(
            FsGrant::scratch("/tmp"),
            NetGrant::Off,
            env,
            ProducerProfile::Tiny.limits(),
        );
        let cmd = SealedCommand::new("/bin/true", Vec::<String>::new(), budget);
        let spec = project_run_spec(&cmd);
        assert_eq!(
            spec.env,
            vec![
                ("PATH".to_string(), "/usr/bin".to_string()),
                ("HOME".to_string(), "/tmp".to_string()),
            ],
            "exactly the allowlist, in order, nothing ambient"
        );
    }

    // §3.1 rows: memory → memory_mib; wall → timeout; cpu/pids/nofile/fsize
    // → in-guest rlimits.
    #[test]
    fn limits_project_to_memory_timeout_and_rlimits() {
        let limits = ProducerProfile::Rust.limits(); // 6 GiB / 15 min / 900 cpu-s
        let budget = CapabilityBudget::new(
            FsGrant::scratch("/tmp"),
            NetGrant::Off,
            Env::empty(),
            limits,
        );
        let cfg = project_vm_config(&budget).expect("project");
        assert_eq!(
            cfg.memory_mib.get(),
            6 * 1024,
            "bytes → MiB (balloon-capped)"
        );

        let cmd = SealedCommand::new("/bin/true", Vec::<String>::new(), budget);
        let spec = project_run_spec(&cmd);
        assert_eq!(spec.timeout, Duration::from_secs(15 * 60));
        assert_eq!(spec.rlimits.cpu_secs, limits.cpu_secs);
        assert_eq!(spec.rlimits.pids, limits.pids);
        assert_eq!(spec.rlimits.nofile, limits.nofile);
        assert_eq!(spec.rlimits.fsize_bytes, limits.fsize_bytes);
        assert_eq!(spec.rlimits.mem_bytes, limits.mem_bytes);
    }

    #[test]
    fn memory_mib_rounds_up_and_never_hits_zero() {
        assert_eq!(mem_mib(NonZeroU64::MIN).get(), 1);
        assert_eq!(mem_mib(NonZeroU64::new(1024 * 1024 + 1).unwrap()).get(), 2);
    }

    // End-to-end through the Cage trait against the fake runtime.
    #[test]
    fn cage_run_launches_projected_config_and_execs_projected_spec() {
        let rt = FakeVmRuntime::new();
        let cage = SmolvmCage::with_runtime(rt.clone());
        let cmd = sealed(FsGrant::scratch("/tmp").ro("/pkg"), NetGrant::Off);

        let out = cage
            .run(cmd.clone(), &CancelToken::never())
            .expect("fake run");
        assert!(out.success());

        let launched = rt.launched();
        assert_eq!(launched.len(), 1);
        assert_eq!(
            launched[0],
            project_vm_config(&cmd.budget).expect("project")
        );

        let execs = rt.execs();
        assert_eq!(execs.len(), 1);
        assert_eq!(execs[0], project_run_spec(&cmd));
    }

    #[test]
    fn cage_run_honors_pre_cancel_without_launching() {
        let rt = FakeVmRuntime::new();
        let cage = SmolvmCage::with_runtime(rt.clone());
        let cancel = CancelToken::new();
        cancel.cancel();

        let err = cage
            .run(sealed(FsGrant::scratch("/tmp"), NetGrant::Off), &cancel)
            .expect_err("cancelled");
        assert!(matches!(err, CageError::Cancelled));
        assert!(rt.launched().is_empty(), "no VM boots after cancel");
    }

    #[test]
    fn cage_folds_vm_errors_into_cage_errors() {
        let rt = FakeVmRuntime::new();
        rt.script_exec(Err(VmError::Exec {
            reason: "agent went away".into(),
        }));
        let cage = SmolvmCage::with_runtime(rt);
        let err = cage
            .run(
                sealed(FsGrant::scratch("/tmp"), NetGrant::Off),
                &CancelToken::never(),
            )
            .expect_err("scripted failure");
        assert!(matches!(err, CageError::Vm(VmError::Exec { .. })));
    }

    #[test]
    fn cage_identity_and_caps_are_production_grade() {
        let cage = SmolvmCage::with_runtime(FakeVmRuntime::new());
        assert_eq!(cage.id().as_str(), "smolvm-microvm");
        let caps = cage.capabilities();
        assert!(caps.isolation);
        assert!(caps.network_off);
        assert!(caps.fs_scope);
        assert!(caps.resource_limits);
        assert!(caps.production_grade);
    }

    #[test]
    fn vm_forge_is_obtainable_over_the_smolvm_cage() {
        let cage = SmolvmCage::with_runtime(FakeVmRuntime::new());
        let forge = VmForge::over(&cage).expect("production-grade cage");
        assert_eq!(forge.cage_id().as_str(), "smolvm-microvm");
    }

    #[test]
    fn golden_pool_is_keyed_by_image_digest() {
        let pool = GoldenPool::new();
        let image = ImageDigest::from_bytes([0xAB; 32]);
        assert!(pool.lookup(&image).is_none());
        pool.install(image, GoldenId::new("g1"));
        assert_eq!(pool.lookup(&image), Some(GoldenId::new("g1")));
        // New digest ⇒ new golden slot (staleness rule V9).
        pool.install(ImageDigest::from_bytes([0xCD; 32]), GoldenId::new("g2"));
        assert_eq!(pool.len(), 2);
    }

    // SMOLVM-PLAN §6 / SV-8: the IR-stream port is appended to the projection.
    #[test]
    fn stream_port_projects_into_vsock_port() {
        let b = budget(FsGrant::scratch("/tmp/scratch"), NetGrant::Off);
        let sp = StreamPort {
            port: 7777,
            socket_path: PathBuf::from("/run/ir-stream.sock"),
        };
        let cfg = project_vm_config_with_stream(&b, Some(&sp)).expect("project");
        assert_eq!(cfg.vsock_ports.len(), 1);
        assert_eq!(cfg.vsock_ports[0].port, 7777);
        assert_eq!(
            cfg.vsock_ports[0].socket_path,
            PathBuf::from("/run/ir-stream.sock")
        );
        assert!(cfg.vsock_ports[0].listen, "host listens for the guest");

        // Without a stream, the projection carries no vsock ports.
        let cfg = project_vm_config(&b).expect("project");
        assert!(cfg.vsock_ports.is_empty());
    }
}
