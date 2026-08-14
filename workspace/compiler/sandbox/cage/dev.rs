//! Dev-only passthrough cage: rlimits + fairness cgroup, no isolation.

use crate::backend::supervisor::{self, apply_rlimits};
use crate::cage::{Cage, CageCaps, CageId, Policy};
use crate::cgroup::Cgroup;
use crate::error::CageError;
use crate::job::cancel::CancelToken;
use crate::seal::SealedCommand;
use crate::spec::Output;

/// Dev-only cage: rlimits (+ best-effort cgroup for scheduling fairness),
/// no isolation boundary of any kind.
///
/// Constructible only from [`Policy::Development`] — the type cannot be built
/// under a production policy, replacing thrice-read env gates at the boundary.
#[derive(Debug)]
pub struct DevPassthrough {
    _policy: DevOnly,
}

/// Zero-sized token proving construction went through [`Policy::Development`].
#[derive(Debug, Clone, Copy)]
struct DevOnly;

impl DevPassthrough {
    /// Construct when `policy` is [`Policy::Development`].
    pub fn try_new(policy: Policy) -> Result<Self, CageError> {
        match policy {
            Policy::Development => Ok(Self { _policy: DevOnly }),
            Policy::Production => Err(CageError::Denied {
                reason: "DevPassthrough is not constructible under Policy::Production".into(),
            }),
        }
    }

    /// Convenience: build from the process env policy (dev only).
    pub fn from_env() -> Result<Self, CageError> {
        Self::try_new(Policy::from_env())
    }
}

impl Cage for DevPassthrough {
    fn id(&self) -> CageId {
        CageId("dev-passthrough")
    }

    fn capabilities(&self) -> CageCaps {
        CageCaps {
            isolation: false,
            network_off: false,
            fs_scope: false,
            resource_limits: false,
            production_grade: false,
        }
    }

    fn run(&self, cmd: SealedCommand, cancel: &CancelToken) -> Result<Output, CageError> {
        if cancel.is_cancelled() {
            return Err(CageError::Cancelled);
        }

        if which::which(&cmd.command).is_err() && !cmd.command.exists() {
            return Err(CageError::ToolchainMissing {
                program: cmd.command_display(),
            });
        }

        let limits = cmd.budget.resources;
        let cgroup = Cgroup::try_create(&limits).map_err(CageError::from)?;

        use std::process::Stdio;
        let mut proc = supervisor::base_command(&cmd.command);
        proc.args(&cmd.args);
        for (k, v) in cmd.budget.env.iter() {
            proc.env(k, v);
        }
        if let Some(cwd) = &cmd.cwd {
            proc.current_dir(cwd);
        }
        proc.stdin(Stdio::null());

        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            #[allow(clippy::redundant_locals)]
            let limits = limits;
            unsafe {
                proc.pre_exec(move || {
                    apply_rlimits(&limits).map_err(crate::error::to_io_error)?;
                    Ok(())
                });
            }
        }

        if cancel.is_cancelled() {
            return Err(CageError::Cancelled);
        }

        let child = proc.spawn().map_err(CageError::Spawn)?;
        supervisor::supervise(child, &limits, cgroup, cancel).map_err(CageError::from)
    }
}
