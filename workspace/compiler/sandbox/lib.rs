//! Hardware-virtualized isolation for untrusted producer execution.
//!
//! # One cage (SMOLVM-PLAN §0–§3)
//!
//! Isolation is the [`Cage`] trait: run a [`SealedCommand`] under a
//! [`CapabilityBudget`], with cooperative [`CancelToken`]. Production:
//! [`SmolvmCage`] — one smolvm microVM per compile, on desktop *and* fleet.
//! Dev: [`DevPassthrough`] (constructible only under [`Policy::Development`]).
//!
//! The budget layer is the contract; the VM is the mechanism (SV-2). The
//! cage *projects* a budget into a [`VmConfig`] (§3.1): RO roots become RO
//! virtiofs mounts, the scratch becomes an ephemeral qcow2 overlay,
//! `NetGrant::Off` becomes [`NetworkPolicy::None`], the env allowlist is the
//! entire guest environment, and limits become the VM memory ceiling plus
//! in-guest rlimits. The former layered OS sandbox (bwrap namespaces,
//! seccomp BPF, landlock, Seatbelt) is deleted; cgroups survive only as a
//! *scheduling-fairness* wrapper around the VMM / dev process, never as a
//! security layer.
//!
//! [`WorkerPool`] is **cage-internal** for library-form producers
//! (nix/ts/python) and runs *inside the guest*; real parallelism equals pool
//! size.
//!
//! # Invariants encoded in the type system
//!
//! - [`Env`] is *only* an allowlist — ambient host environment is never inherited.
//! - [`Network`] / [`NetGrant`] is explicit; the default is off.
//! - A sealed job's network field is the [`NetOff`] marker: "sealed with the
//!   network on" is unrepresentable, and the only VM policy [`NetOff`] can
//!   project to is [`NetworkPolicy::None`]. The machine is built without a
//!   NIC — the typestate states a physical fact, not a filter rule.
//! - [`Limits`] requires every ceiling (no silent "unlimited" field).
//! - [`LimitOverride`] is sparse — zeros are unrepresentable via `NonZero*`.
//! - [`DevPassthrough`] cannot be constructed under [`Policy::Production`].
//! - [`VmForge`] is obtainable only over a production-grade cage (SV-4):
//!   untrusted-source `Compile` impls demand it, so untrusted code cannot be
//!   compiled outside the VM boundary.

#![deny(missing_docs)]

pub mod backend;
pub mod budget;
pub mod cage;
pub mod cgroup;
pub mod error;
pub mod golden;
pub mod job;
pub mod node;
pub mod observer;
pub mod probe;
pub mod seal;
pub mod spec;
pub mod toolchains;
pub mod vm;

pub use backend::smolvm::{RootfsStore, SmolvmRuntime};
pub use budget::limits::{LimitOverride, Limits, Network};
pub use budget::overrides::{OverrideTable, SandboxKey};
pub use budget::profiles::ProducerProfile;
pub use budget::{CapabilityBudget, EgressAllowlist, FsGrant, NetGrant, ThreatTier};
pub use cage::smolvm::{
    GoldenPool, SmolvmCage, project_network, project_run_spec, project_vm_config,
    project_vm_config_with_stream,
};
pub use cage::{Cage, CageCaps, CageId, DevPassthrough, Policy, run_sealed};
pub use error::{CageError, KillReason, SandboxError, to_io_error};
pub use golden::{GoldenContentIo, MAX_GOLDEN_BYTES};
pub use job::cancel::CancelToken;
pub use job::worker::{JobRequest, JobResponse, WorkerLang, WorkerPool, WorkerPoolConfig};
pub use job::{Acquiring, Job, NetOff, Sealed, SealedBudget, VmForge};
pub use node::NodeId;
pub use observer::{CountingObserver, ForgeObserver, NullObserver};
pub use probe::{
    HostIsolation, IsolationPolicy, VirtSupport, VmExecutionCapability, VmUnavailableReason,
    probe_vm_execution, require as require_isolation,
};
pub use seal::{SealedCommand, SealedInput, Sealer};
pub use spec::{Env, Mounts, Output, ProcessEnd, Spec};
pub use toolchains::ToolchainSet;
pub use toolchains::images::{
    ImageDigest, OciImageRef, OciReference, ToolchainImage, ToolchainImageSet, ToolchainImageStore,
    ToolchainPlane,
};
pub use vm::{
    Cidr, DnsName, DnsPolicy, EgressPolicy, FakeVmRuntime, GoldenId, GuestRlimits, MountTag,
    NetworkPolicy, OverlayMode, RunSpec, ScratchOverlay, StreamPort, VirtiofsMount, VmConfig,
    VmError, VmHandle, VmRuntime, VsockPort,
};
/// Type alias kept for external crates; new code should use [`Output`] directly.
pub type Captured = Output;

/// Probe host isolation and enforce policy from the environment.
pub fn boot_check() -> Result<HostIsolation, SandboxError> {
    require_isolation(IsolationPolicy::from_env())
}
