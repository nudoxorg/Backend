//! Explicit process environment and resource limits.

use crate::{ProcessError, UnsupportedLimit};
use std::time::Duration;

/// Process environment policy.  Only these sorted variables are passed to an
/// authority; ambient process environment is never inherited.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessEnvironment {
    variables: Vec<(String, String)>,
}

impl ProcessEnvironment {
    /// Builds a sorted, duplicate-free explicit environment.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::DuplicateEnvironment`] for duplicate keys or
    /// [`ProcessError::InvalidEnvironment`] for malformed names or values.
    pub fn new(mut variables: Vec<(String, String)>) -> Result<Self, ProcessError> {
        variables.sort();
        if variables
            .windows(2)
            .any(|window| window[0].0 == window[1].0)
        {
            return Err(ProcessError::DuplicateEnvironment);
        }
        if variables.iter().any(|(key, value)| {
            key.is_empty()
                || key.contains('=')
                || key.as_bytes().contains(&0)
                || value.as_bytes().contains(&0)
        }) {
            return Err(ProcessError::InvalidEnvironment);
        }
        Ok(Self { variables })
    }

    /// Returns explicit variables in canonical order.
    #[must_use]
    pub fn variables(&self) -> &[(String, String)] {
        &self.variables
    }

    pub(crate) fn as_envs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.variables
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }
}

/// Bounded process execution limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProcessLimits {
    stdout: usize,
    stderr: usize,
    wall_time: Duration,
    output_bytes: usize,
    input_bytes: usize,
    workspace_limit: Option<usize>,
    process_count_limit: Option<usize>,
    memory_bytes_limit: Option<usize>,
    cpu_time_limit: Option<Duration>,
}

#[derive(Clone, Copy)]
pub(crate) struct ProcessLimitIdentity {
    pub(super) stdout: usize,
    pub(super) stderr: usize,
    pub(super) wall_time: Duration,
    pub(super) output_bytes: usize,
    pub(super) input_bytes: usize,
    pub(super) workspace_limit: Option<usize>,
    pub(super) process_count_limit: Option<usize>,
    pub(super) memory_bytes_limit: Option<usize>,
    pub(super) cpu_time_limit: Option<Duration>,
}

impl ProcessLimits {
    /// Validates non-zero stdout, stderr, aggregate output, and wall-time bounds.
    ///
    /// The initial request-input bound equals `output_bytes` for compatibility;
    /// [`Self::with_input_bytes_limit`] can set it independently.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ZeroLimit`] when any required bound is zero.
    pub fn new(
        stdout: usize,
        stderr: usize,
        wall_time: Duration,
        output_bytes: usize,
    ) -> Result<Self, ProcessError> {
        if stdout == 0 || stderr == 0 || output_bytes == 0 || wall_time.is_zero() {
            return Err(ProcessError::ZeroLimit);
        }
        Ok(Self {
            stdout,
            stderr,
            wall_time,
            output_bytes,
            input_bytes: output_bytes,
            workspace_limit: None,
            process_count_limit: None,
            memory_bytes_limit: None,
            cpu_time_limit: None,
        })
    }

    /// Maximum stdout bytes retained and admitted.
    #[must_use]
    pub const fn stdout(&self) -> usize {
        self.stdout
    }

    /// Maximum stderr bytes retained and admitted.
    #[must_use]
    pub const fn stderr(&self) -> usize {
        self.stderr
    }

    /// Maximum wall time allowed for one process.
    #[must_use]
    pub const fn wall_time(&self) -> Duration {
        self.wall_time
    }

    /// Maximum aggregate stdout and stderr bytes retained by the supervisor.
    #[must_use]
    pub const fn output_bytes(&self) -> usize {
        self.output_bytes
    }

    /// Maximum request and standard-input bytes accepted by a command.
    #[must_use]
    pub const fn input_bytes(&self) -> usize {
        self.input_bytes
    }

    /// Compatibility accessor for the aggregate output bound.
    ///
    /// The command's workspace is the filesystem workspace path. This numeric
    /// bound is the same value returned by `output_bytes`. Request input has
    /// its independent `input_bytes` bound, and filesystem growth has the
    /// separate `workspace_limit` bound.
    #[must_use]
    pub const fn workspace_bytes(&self) -> usize {
        self.output_bytes
    }

    /// Adds an independent bound on request and standard-input bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ZeroLimit`] when `limit` is zero.
    pub fn with_input_bytes_limit(mut self, limit: usize) -> Result<Self, ProcessError> {
        if limit == 0 {
            return Err(ProcessError::ZeroLimit);
        }
        self.input_bytes = limit;
        Ok(self)
    }

    /// Adds a bound on bytes newly created or grown below the command's
    /// workspace while it runs.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ZeroLimit`] when `limit` is zero.
    pub fn with_workspace_limit(mut self, limit: usize) -> Result<Self, ProcessError> {
        if limit == 0 {
            return Err(ProcessError::ZeroLimit);
        }
        self.workspace_limit = Some(limit);
        Ok(self)
    }

    /// Adds a maximum process-tree count request.
    ///
    /// Unix supervisors apply a conservative per-user process ceiling through
    /// the child process's `RLIMIT_NPROC` resource.  This can reject earlier
    /// when the user already has unrelated processes.  Other platforms report
    /// [`ProcessError::UnsupportedLimit`] before execution.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ZeroLimit`] when `limit` is zero.
    pub fn with_process_count_limit(mut self, limit: usize) -> Result<Self, ProcessError> {
        if limit == 0 {
            return Err(ProcessError::ZeroLimit);
        }
        self.process_count_limit = Some(limit);
        Ok(self)
    }

    /// Adds a memory limit request.
    ///
    /// Linux supervisors apply this as a conservative virtual address-space
    /// ceiling through `RLIMIT_AS`. This is a hard per-process memory ceiling;
    /// platforms without a checked memory resource limit report
    /// [`ProcessError::UnsupportedLimit`] before execution.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ZeroLimit`] when `limit` is zero.
    pub fn with_memory_bytes_limit(mut self, limit: usize) -> Result<Self, ProcessError> {
        if limit == 0 {
            return Err(ProcessError::ZeroLimit);
        }
        self.memory_bytes_limit = Some(limit);
        Ok(self)
    }

    /// Adds a CPU-time limit request.
    ///
    /// Unix supervisors apply the request through `RLIMIT_CPU`.  Other
    /// platforms report [`ProcessError::UnsupportedLimit`] before execution.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessError::ZeroLimit`] when `limit` is zero.
    pub fn with_cpu_time_limit(mut self, limit: Duration) -> Result<Self, ProcessError> {
        if limit.is_zero() {
            return Err(ProcessError::ZeroLimit);
        }
        self.cpu_time_limit = Some(limit);
        Ok(self)
    }

    /// Returns the maximum allowed workspace growth, if requested.
    #[must_use]
    pub const fn workspace_limit(&self) -> Option<usize> {
        self.workspace_limit
    }

    /// Returns the requested process-count bound, if any.
    #[must_use]
    pub const fn process_count_limit(&self) -> Option<usize> {
        self.process_count_limit
    }

    /// Returns the requested memory bound, if any.
    #[must_use]
    pub const fn memory_bytes_limit(&self) -> Option<usize> {
        self.memory_bytes_limit
    }

    /// Returns the requested CPU-time bound, if any.
    #[must_use]
    pub const fn cpu_time_limit(&self) -> Option<Duration> {
        self.cpu_time_limit
    }

    pub(super) const fn identity_parts(&self) -> ProcessLimitIdentity {
        ProcessLimitIdentity {
            stdout: self.stdout,
            stderr: self.stderr,
            wall_time: self.wall_time,
            output_bytes: self.output_bytes,
            input_bytes: self.input_bytes,
            workspace_limit: self.workspace_limit,
            process_count_limit: self.process_count_limit,
            memory_bytes_limit: self.memory_bytes_limit,
            cpu_time_limit: self.cpu_time_limit,
        }
    }

    /// Returns the first optional bound that this supervisor cannot enforce.
    #[must_use]
    pub fn unsupported_limit(&self) -> Option<UnsupportedLimit> {
        #[cfg(not(unix))]
        if self.process_count_limit.is_some() {
            return Some(UnsupportedLimit::ProcessCount);
        }
        #[cfg(not(target_os = "linux"))]
        if self.memory_bytes_limit.is_some() {
            return Some(UnsupportedLimit::MemoryBytes);
        }
        #[cfg(not(unix))]
        if self.cpu_time_limit.is_some() {
            return Some(UnsupportedLimit::CpuTime);
        }
        None
    }

    pub(crate) fn uses_unix_resource_limits(&self) -> bool {
        #[cfg(unix)]
        {
            self.process_count_limit.is_some()
                || self.cpu_time_limit.is_some()
                || (cfg!(target_os = "linux") && self.memory_bytes_limit.is_some())
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    pub(crate) fn validate_supported(&self) -> Result<(), ProcessError> {
        self.unsupported_limit()
            .map_or(Ok(()), |limit| Err(ProcessError::UnsupportedLimit(limit)))
    }
}
