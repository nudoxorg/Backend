//! Resource ceilings and network policy.
//!
//! Every field of [`Limits`] is required: there is no "forgot to cap memory"
//! state representable in the type system. Partial overrides are
//! [`LimitOverride`] — only `Some` fields replace the profile base.

use std::num::{NonZeroU32, NonZeroU64};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Whether the sandboxed process may observe a network namespace with routes.
///
/// Parse phases **must** use [`Network::Off`]. Fetch phases that need the
/// network still hash-pin inputs; prefer fixed-output derivations when possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Network {
    /// Empty netns / denied outbound (default for P-parse).
    #[default]
    Off,
    /// Network permitted (P-fetch only).
    On,
}

/// Hard resource ceilings for one sandboxed invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Address-space / cgroup memory max (bytes).
    pub mem_bytes: NonZeroU64,
    /// CPU seconds (`RLIMIT_CPU`).
    pub cpu_secs: NonZeroU64,
    /// Wall-clock budget enforced by the supervisor.
    pub wall: Duration,
    /// Maximum processes in the cgroup (`pids.max`).
    pub pids: NonZeroU32,
    /// Cap on stdout bytes read by the supervisor.
    pub max_stdout: usize,
    /// Cap on stderr bytes read by the supervisor.
    pub max_stderr: usize,
    /// `RLIMIT_FSIZE` — max size of a single file the guest may write.
    pub fsize_bytes: NonZeroU64,
    /// Soft cap on open file descriptors.
    pub nofile: NonZeroU64,
}

/// Sparse overlay on a profile's [`Limits`] (design §13 / P5).
///
/// All fields optional — only set values replace the base. Zero is rejected
/// via [`NonZeroU64`] / [`NonZeroU32`] at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LimitOverride {
    /// Replace memory ceiling (bytes).
    #[serde(default)]
    pub mem_bytes: Option<NonZeroU64>,
    /// Replace CPU seconds.
    #[serde(default)]
    pub cpu_secs: Option<NonZeroU64>,
    /// Replace wall-clock budget.
    #[serde(default, with = "humantime_serde_opt")]
    pub wall: Option<Duration>,
    /// Replace pid budget.
    #[serde(default)]
    pub pids: Option<NonZeroU32>,
    /// Replace stdout cap.
    #[serde(default)]
    pub max_stdout: Option<usize>,
    /// Replace stderr cap.
    #[serde(default)]
    pub max_stderr: Option<usize>,
    /// Replace file-size rlimit.
    #[serde(default)]
    pub fsize_bytes: Option<NonZeroU64>,
    /// Replace nofile rlimit.
    #[serde(default)]
    pub nofile: Option<NonZeroU64>,
}

impl LimitOverride {
    /// Empty overlay (identity).
    pub const fn none() -> Self {
        Self {
            mem_bytes: None,
            cpu_secs: None,
            wall: None,
            pids: None,
            max_stdout: None,
            max_stderr: None,
            fsize_bytes: None,
            nofile: None,
        }
    }

    /// Apply this overlay onto `base`.
    pub fn apply(self, mut base: Limits) -> Limits {
        if let Some(v) = self.mem_bytes {
            base.mem_bytes = v;
        }
        if let Some(v) = self.cpu_secs {
            base.cpu_secs = v;
        }
        if let Some(v) = self.wall {
            base.wall = v;
        }
        if let Some(v) = self.pids {
            base.pids = v;
        }
        if let Some(v) = self.max_stdout {
            base.max_stdout = v;
        }
        if let Some(v) = self.max_stderr {
            base.max_stderr = v;
        }
        if let Some(v) = self.fsize_bytes {
            base.fsize_bytes = v;
        }
        if let Some(v) = self.nofile {
            base.nofile = v;
        }
        base
    }
}

/// Optional duration as seconds in config (avoid pulling humantime).
mod humantime_serde_opt {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(v: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(d) => s.serialize_some(&d.as_secs()),
            None => s.serialize_none(),
        }
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Duration>, D::Error> {
        let opt = Option::<u64>::deserialize(d)?;
        Ok(opt.map(Duration::from_secs))
    }
}

impl Limits {
    /// Build limits from raw numbers, rejecting zeros.
    pub fn try_new(
        mem_bytes: u64,
        cpu_secs: u64,
        wall: Duration,
        pids: u32,
        max_stdout: usize,
        max_stderr: usize,
        fsize_bytes: u64,
        nofile: u64,
    ) -> Result<Self, &'static str> {
        Ok(Self {
            mem_bytes: NonZeroU64::new(mem_bytes).ok_or("mem_bytes must be non-zero")?,
            cpu_secs: NonZeroU64::new(cpu_secs).ok_or("cpu_secs must be non-zero")?,
            wall,
            pids: NonZeroU32::new(pids).ok_or("pids must be non-zero")?,
            max_stdout,
            max_stderr,
            fsize_bytes: NonZeroU64::new(fsize_bytes).ok_or("fsize_bytes must be non-zero")?,
            nofile: NonZeroU64::new(nofile).ok_or("nofile must be non-zero")?,
        })
    }

    /// Fallible constructor used by [`crate::profiles`]; panics only on
    /// programmer error (zero constants in profile tables).
    pub(crate) const fn from_const(
        mem_bytes: u64,
        cpu_secs: u64,
        wall_secs: u64,
        pids: u32,
        max_stdout: usize,
        max_stderr: usize,
        fsize_bytes: u64,
        nofile: u64,
    ) -> Self {
        // const-panicking NonZero constructors
        Self {
            mem_bytes: match NonZeroU64::new(mem_bytes) {
                Some(v) => v,
                None => panic!("mem_bytes"),
            },
            cpu_secs: match NonZeroU64::new(cpu_secs) {
                Some(v) => v,
                None => panic!("cpu_secs"),
            },
            wall: Duration::from_secs(wall_secs),
            pids: match NonZeroU32::new(pids) {
                Some(v) => v,
                None => panic!("pids"),
            },
            max_stdout,
            max_stderr,
            fsize_bytes: match NonZeroU64::new(fsize_bytes) {
                Some(v) => v,
                None => panic!("fsize_bytes"),
            },
            nofile: match NonZeroU64::new(nofile) {
                Some(v) => v,
                None => panic!("nofile"),
            },
        }
    }
}
