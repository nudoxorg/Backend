//! Process isolation for untrusted producer execution.
//!
//! # One cage (DAEMON-PLAN §2.2)
//!
//! Isolation is the [`Cage`] trait: run a [`SealedCommand`] under a
//! [`CapabilityBudget`], with cooperative [`CancelToken`]. Production:
//! [`LinuxNamespaces`] (bwrap + cgroup + seccomp). Dev: [`DevPassthrough`]
//! (constructible only under [`Policy::Development`]).
//!
//! [`WorkerPool`] is **cage-internal** for library-form producers (nix/ts/python),
//! not a peer of the Backend enum. Real parallelism equals pool size.
//!
//! # Layering (Linux production path)
//!
//! ```text
//! cgroup (memory.max + swap.max=0 + pids)  — real RAM / fork bomb
//!   └─ bwrap namespaces (net/pid/mount/…)  — cage
//!        ├─ rlimits (AS=4×mem VA, CPU, FSIZE, NOFILE)
//!        └─ bwrap --seccomp FD            — LPE denylist AFTER setup
//! ```
//!
//! **Never** install seccomp/Landlock on the bwrap process itself — that
//! blocks `unshare`/`mount` and breaks the sandbox. Direct spawn (no bwrap)
//! uses `pre_exec`: rlimit → Landlock → seccomp.
//!
//! # Invariants encoded in the type system
//!
//! - [`Env`] is *only* an allowlist — ambient host environment is never inherited.
//! - [`Network`] / [`NetGrant`] is explicit; the default is off.
//! - [`Limits`] requires every ceiling (no silent "unlimited" field).
//! - [`LimitOverride`] is sparse — zeros are unrepresentable via `NonZero*`.
//! - [`DevPassthrough`] cannot be constructed under [`Policy::Production`].

#![deny(missing_docs)]

pub mod backend;
pub mod budget;
pub mod cage;
pub mod cancel;
pub mod cgroup;
pub mod error;
pub mod job;
pub mod limits;
pub mod node;
pub mod observer;
pub mod overrides;
pub mod probe;
pub mod profiles;
pub mod seal;
pub mod spec;
pub mod toolchains;
pub mod worker;

/// Landlock LSM helpers (Linux only).
#[cfg(target_os = "linux")]
pub mod landlock;
/// Seccomp denylist + BPF export for `bwrap --seccomp` (Linux only).
#[cfg(target_os = "linux")]
pub mod seccomp;

pub use budget::{CapabilityBudget, FsGrant, NetGrant, ThreatTier};
pub use cage::{Cage, CageCaps, CageId, DevPassthrough, LinuxBwrap, LinuxNamespaces, Policy, run_sealed};
pub use job::{Acquiring, Job, NetOff, Sealed, SealedBudget};
pub use cancel::CancelToken;
pub use error::{CageError, KillReason, SandboxError, to_io_error};
pub use limits::{LimitOverride, Limits, Network};
pub use node::NodeId;
pub use observer::{CountingObserver, ForgeObserver, NullObserver};
pub use overrides::{OverrideTable, SandboxKey};
pub use probe::{HostIsolation, IsolationPolicy, LandlockAbi, require as require_isolation};
pub use profiles::ProducerProfile;
pub use seal::{SealedCommand, SealedInput, Sealer};
pub use toolchains::ToolchainSet;
pub use spec::{Env, Mounts, Output, ProcessEnd, Spec};
/// Type alias kept for external crates; new code should use [`Output`] directly.
pub type Captured = Output;
pub use worker::{JobRequest, JobResponse, WorkerLang, WorkerPool, WorkerPoolConfig};

/// Probe host isolation and enforce policy from the environment.
pub fn boot_check() -> Result<HostIsolation, SandboxError> {
	require_isolation(IsolationPolicy::from_env())
}
