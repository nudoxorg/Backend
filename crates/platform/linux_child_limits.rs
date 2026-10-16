//! Linux child resource ceilings applied before exec, without a host shell.
#![allow(
    unsafe_code,
    reason = "narrow async-signal-safe child setrlimit boundary"
)]

use rustix::process::{Resource, Rlimit, setrlimit};
use std::{io, os::unix::process::CommandExt, process::Command};

/// Exact OS units for the existing supervisor policy. Omitted ceilings retain
/// the inherited limit; supplied ceilings set both the soft and hard limits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LinuxChildResourceLimits {
    /// CPU seconds, rounded by the caller to the OS's whole-second unit.
    pub cpu_seconds: Option<u64>,
    /// Maximum virtual address space in bytes.
    pub address_space_bytes: Option<u64>,
    /// Per-user process count, with Linux's normal privileged-user semantics.
    pub processes: Option<u64>,
}

/// Applies limits only to the child. A refused syscall causes `Command::spawn`
/// to fail before the requested executable can run.
pub fn configure_child_resource_limits(command: &mut Command, limits: LinuxChildResourceLimits) {
    // SAFETY: This hook captures only Copy integer policy. Between fork and
    // exec it performs only setrlimit syscalls and errno-to-io conversion;
    // it allocates nothing, takes no locks, and touches no parent process state.
    unsafe {
        command.pre_exec(move || {
            for (resource, value) in [
                (Resource::Cpu, limits.cpu_seconds),
                (Resource::As, limits.address_space_bytes),
                (Resource::Nproc, limits.processes),
            ] {
                if let Some(value) = value {
                    setrlimit(
                        resource,
                        Rlimit {
                            current: Some(value),
                            maximum: Some(value),
                        },
                    )
                    .map_err(io::Error::from)?;
                }
            }
            Ok(())
        });
    }
}
