//! Host virtualization probe and production policy (SMOLVM-PLAN §3.3, §8 P0).
//!
//! Call once at process start. [`IsolationPolicy::RequireProduction`] fails
//! closed when the host cannot host a hardware-virtualized cage: Linux needs
//! a readable+writable `/dev/kvm`; macOS needs `kern.hv_support == 1`
//! (Hypervisor.framework).

use crate::cage::CageCaps;
use crate::error::SandboxError;

/// Which hypervisor the host can offer to the microVM cage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VirtSupport {
    /// Linux KVM: `/dev/kvm` exists and is read/write accessible.
    Kvm,
    /// macOS Hypervisor.framework: `kern.hv_support` is 1.
    Hvf,
    /// No hardware virtualization available to this process.
    Unavailable,
}

impl VirtSupport {
    /// Whether a microVM can be booted on this host.
    pub const fn available(self) -> bool {
        !matches!(self, Self::Unavailable)
    }
}

/// Probe host virtualization support.
///
/// Linux: access(2) on `/dev/kvm` for read+write. macOS: `sysctlbyname`
/// (`kern.hv_support`), with a `sysctl -n` subprocess fallback. Everything
/// else reports [`VirtSupport::Unavailable`].
pub fn probe_virtualization() -> VirtSupport {
    #[cfg(target_os = "linux")]
    {
        if kvm_accessible() {
            return VirtSupport::Kvm;
        }
        VirtSupport::Unavailable
    }
    #[cfg(target_os = "macos")]
    {
        if hv_support() {
            return VirtSupport::Hvf;
        }
        VirtSupport::Unavailable
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        VirtSupport::Unavailable
    }
}

#[cfg(target_os = "linux")]
fn kvm_accessible() -> bool {
    // access(2): existence + rw permission in one call, no fd churn.
    // SAFETY: constant NUL-terminated path; access has no memory effects.
    unsafe { libc::access(c"/dev/kvm".as_ptr(), libc::R_OK | libc::W_OK) == 0 }
}

#[cfg(target_os = "macos")]
fn hv_support() -> bool {
    let mut val: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: sysctlbyname writes at most `len` bytes into `val`.
    let rc = unsafe {
        libc::sysctlbyname(
            c"kern.hv_support".as_ptr(),
            (&raw mut val).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc == 0 {
        return val == 1;
    }
    // Fallback: subprocess read (sandboxed test environments occasionally
    // deny the syscall but allow the binary).
    std::process::Command::new("sysctl")
        .arg("-n")
        .arg("kern.hv_support")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "1")
}

/// Snapshot of what this host can enforce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostIsolation {
    /// Capabilities of the cage this host can carry.
    pub capabilities: CageCaps,
    /// Selected cage name.
    pub backend: &'static str,
    /// Hardware virtualization support (KVM / HVF).
    pub virtualization: VirtSupport,
    /// cgroup v2 writable parent discovered (scheduling fairness only).
    pub cgroup: bool,
    /// Kernel major.minor when readable from `uname`.
    pub kernel: Option<(u32, u32)>,
}

/// How strictly isolation is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IsolationPolicy {
    /// Degrade gracefully (dev default).
    #[default]
    BestEffort,
    /// Refuse to run without a production-grade (virtualization-capable) host.
    RequireProduction,
}

impl IsolationPolicy {
    /// Single resolution of the production isolation gate from env.
    ///
    /// True when `NUDOX_SANDBOX_REQUIRE=1|true` or `NUDOX_ENV=prod|production`.
    /// Every call site that needs the prod gate must use this (or [`Self::from_env`])
    /// rather than re-reading the environment.
    pub fn env_requires_production() -> bool {
        matches!(
            std::env::var("NUDOX_SANDBOX_REQUIRE").as_deref(),
            Ok("1" | "true")
        ) || matches!(
            std::env::var("NUDOX_ENV").as_deref(),
            Ok("prod" | "production")
        )
    }

    /// From env: production gate → require; otherwise best-effort.
    pub fn from_env() -> Self {
        if Self::env_requires_production() {
            Self::RequireProduction
        } else {
            Self::BestEffort
        }
    }

    /// Whether interpreter workers are mandatory (no in-process fallback).
    ///
    /// True under the production gate, or when `NUDOX_PRODUCER_WORKER` is set
    /// (explicit worker path implies the worker route).
    pub fn require_worker() -> bool {
        Self::env_requires_production() || std::env::var_os("NUDOX_PRODUCER_WORKER").is_some()
    }
}

/// Probe the host once.
///
/// Reports the capabilities of the cage this host *can carry*: a
/// virtualization-capable host reports the smolvm microVM cage's
/// (production-grade) capability set even before the `smolvm-backend`
/// feature links the real runtime — wiring the runtime is the P0 gate, the
/// hardware capability is what the probe answers.
pub fn probe() -> HostIsolation {
    let virtualization = probe_virtualization();
    let (capabilities, backend) = if virtualization.available() {
        (
            CageCaps {
                isolation: true,
                network_off: true,
                fs_scope: true,
                resource_limits: true,
                production_grade: true,
            },
            "smolvm-microvm",
        )
    } else {
        (CageCaps::default(), "dev-passthrough")
    };

    HostIsolation {
        capabilities,
        backend,
        virtualization,
        cgroup: crate::cgroup::has_writable_parent(),
        kernel: read_kernel_version(),
    }
}

/// Assert the host meets `policy`. Returns the probe result on success.
pub fn require(policy: IsolationPolicy) -> Result<HostIsolation, SandboxError> {
    let host = probe();
    match policy {
        IsolationPolicy::BestEffort => {
            if !host.capabilities.production_grade {
                tracing::warn!(
                    backend = host.backend,
                    "isolation is best-effort (no hardware virtualization)"
                );
            }
            Ok(host)
        }
        IsolationPolicy::RequireProduction => {
            if !host.virtualization.available() {
                return Err(SandboxError::Denied {
                    reason: format!(
                        "production isolation required but host has no hardware \
						 virtualization (backend `{}`)",
                        host.backend
                    ),
                });
            }
            Ok(host)
        }
    }
}

fn read_kernel_version() -> Option<(u32, u32)> {
    #[cfg(unix)]
    {
        let uts = std::process::Command::new("uname")
            .arg("-r")
            .output()
            .ok()?;
        if !uts.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&uts.stdout);
        let mut parts = s.trim().split(|c: char| !c.is_ascii_digit());
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        Some((major, minor))
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_consistent_capability_and_backend() {
        let host = probe();
        if host.virtualization.available() {
            assert_eq!(host.backend, "smolvm-microvm");
            assert!(host.capabilities.production_grade);
        } else {
            assert_eq!(host.backend, "dev-passthrough");
            assert!(!host.capabilities.production_grade);
        }
    }

    #[test]
    fn best_effort_always_passes() {
        assert!(require(IsolationPolicy::BestEffort).is_ok());
    }

    #[test]
    fn virt_support_availability() {
        assert!(VirtSupport::Kvm.available());
        assert!(VirtSupport::Hvf.available());
        assert!(!VirtSupport::Unavailable.available());
    }
}
