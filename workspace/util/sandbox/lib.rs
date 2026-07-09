//! Process isolation for untrusted producer execution.
//!
//! Two isolation problems, two mechanisms (see design §3–6):
//!
//! 1. **Compile-heavy, code-executing producers** (Rust, Java, Go) run as
//!    external toolchains under a namespace cage ([`LinuxBwrap`] or
//!    [`NixDerivation`]).
//! 2. **In-process interpreters/parsers** (snix, deno_doc, pyrefly) move out
//!    of the indexer address space into sandboxed worker subprocesses
//!    ([`worker`]).
//!
//! Network discipline is uniform: resolve/fetch may use the network (hash-pinned);
//! parse always runs with [`Network::Off`].
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
//! - [`Network`] is explicit; the default is off.
//! - [`Limits`] requires every ceiling (no silent "unlimited" field).
//! - [`LimitOverride`] is sparse — zeros are unrepresentable via `NonZero*`.
//! - [`Backend`] selection is probe-based; [`Passthrough`] refuses production.
//!
//! # Backends
//!
//! | Backend | Platform | Role |
//! |---------|----------|------|
//! | [`LinuxBwrap`] | Linux | Primary: bwrap + `--seccomp` + cgroups |
//! | [`NixDerivation`] | any w/ nix | Daemon sandbox + store cache (`NUDOX_SANDBOX=nix`) |
//! | [`MacSeatbelt`] | macOS | Opt-in (`NUDOX_SANDBOX=seatbelt`); not boundary of record |
//! | [`Passthrough`] | any | Dev default on macOS; gated out of production |

#![deny(missing_docs)]

pub mod backend;
pub mod cache;
pub mod cgroup;
pub mod error;
pub mod limits;
pub mod observer;
pub mod overrides;
pub mod probe;
pub mod profiles;
pub mod spec;
pub mod worker;

/// Landlock LSM helpers (Linux only).
#[cfg(target_os = "linux")]
pub mod landlock;
/// Seccomp denylist + BPF export for `bwrap --seccomp` (Linux only).
#[cfg(target_os = "linux")]
pub mod seccomp;

pub use backend::{
	select, Backend, Capabilities, LinuxBwrap, MacSeatbelt, NixDerivation, Passthrough, Selected,
};
pub use cache::{CacheKey, ParseCache};
pub use error::{to_io_error, KillReason, SandboxError};
pub use limits::{LimitOverride, Limits, Network};
pub use observer::{CountingObserver, NullObserver, SandboxObserver};
pub use probe::{require as require_isolation, HostIsolation, IsolationPolicy, LandlockAbi};
pub use overrides::install as install_limit_overrides;
pub use profiles::ProducerProfile;
pub use spec::{Captured, Env, Mounts, Output, ProcessEnd, Spec};
pub use worker::{JobRequest, JobResponse, WorkerLang, WorkerPool, WorkerPoolConfig};

use std::sync::OnceLock;

/// Process-wide default backend, selected once via [`select`].
fn global_backend() -> &'static dyn Backend {
	static BACKEND: OnceLock<Selected> = OnceLock::new();
	BACKEND.get_or_init(select).as_ref()
}

/// Run `spec` on the process-wide default backend.
pub fn run(spec: Spec) -> Result<Output, SandboxError> {
	global_backend().run(spec)
}

/// Run `spec` on an explicitly chosen backend.
pub fn run_with(backend: &dyn Backend, spec: Spec) -> Result<Output, SandboxError> {
	backend.run(spec)
}

/// Probe host isolation and enforce policy from the environment.
pub fn boot_check() -> Result<HostIsolation, SandboxError> {
	require_isolation(IsolationPolicy::from_env())
}
