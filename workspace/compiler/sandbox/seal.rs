//! Sealed command / input construction.
//!
//! [`Sealer`] is the only intentional place that may touch ambient host state
//! (PATH, toolchains, scratch roots) when projecting into a budget. Callers
//! that already hold a fully resolved budget can build [`SealedCommand`]
//! directly via [`SealedCommand::new`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use heart::JobKey;

use crate::budget::{CapabilityBudget, FsGrant, NetGrant};
use crate::limits::Limits;
use crate::spec::{Env, Mounts};

/// Hash-pinned inputs already on disk, with a fully resolved budget.
///
/// The [`key`](Self::key) is the content-addressed job identity computed from
/// `ToolchainSet::digest()` + source / lock hashes; `run_producer` uses it as
/// the CAS key.
#[derive(Debug, Clone)]
pub struct SealedInput {
    /// Content-addressed job identity (CAS key).
    pub key: JobKey,
    /// Package / tree root (already local, hash-pinned by the caller).
    pub root: PathBuf,
    /// Resolved capability budget — no re-resolution downstream.
    pub budget: CapabilityBudget,
}

impl SealedInput {
    /// Construct a sealed input with an explicit job key.
    pub fn new(key: JobKey, root: impl Into<PathBuf>, budget: CapabilityBudget) -> Self {
        Self {
            key,
            root: root.into(),
            budget,
        }
    }

    /// Construct with a placeholder key derived from the root path (dev / tests
    /// that do not go through the CAS). Prefer [`Self::new`] with a real key.
    pub fn unkeyed(root: impl Into<PathBuf>, budget: CapabilityBudget) -> Self {
        let root = root.into();
        let key = JobKey::derive(b"unkeyed", b"", root.as_os_str().as_encoded_bytes(), b"");
        Self { key, root, budget }
    }
}

/// Fully specified command under a capability budget.
///
/// Replaces ad-hoc [`Spec`] + external isolated builders as the cage-facing
/// unit of work.
#[derive(Debug, Clone)]
pub struct SealedCommand {
    /// Program to exec.
    pub command: PathBuf,
    /// Arguments (not including argv0).
    pub args: Vec<OsString>,
    /// Working directory inside the guest (must be visible via the grant).
    pub cwd: Option<PathBuf>,
    /// Fully resolved capability budget.
    pub budget: CapabilityBudget,
}

impl SealedCommand {
    /// Build from resolved parts (no ambient discovery).
    pub fn new(
        command: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
        budget: CapabilityBudget,
    ) -> Self {
        Self {
            command: command.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
            budget,
        }
    }

    /// Set guest cwd.
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Command path for diagnostics.
    pub fn command_path(&self) -> &Path {
        &self.command
    }

    /// Display label for error messages.
    pub fn command_display(&self) -> String {
        self.command.display().to_string()
    }
}

/// Projects ambient host facts into sealed budgets / commands.
///
/// Producers assemble mounts/env themselves and call [`SealedCommand::new`].
/// The type marks the ambient-access boundary.
#[derive(Debug, Default, Clone)]
pub struct Sealer {
    _private: (),
}

impl Sealer {
    /// Construct a sealer.
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Seal a command under an already-resolved budget (no ambient reads).
    pub fn seal_command(
        &self,
        command: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<OsString>>,
        budget: CapabilityBudget,
    ) -> SealedCommand {
        SealedCommand::new(command, args, budget)
    }

    /// Build a budget from explicit pieces (no ambient reads).
    pub fn budget(
        &self,
        fs: FsGrant,
        net: NetGrant,
        env: Env,
        resources: Limits,
    ) -> CapabilityBudget {
        CapabilityBudget::new(fs, net, env, resources)
    }

    /// Lift mounts + env + limits into a budget (scratch required).
    pub fn budget_from_mounts(
        &self,
        mounts: Mounts,
        scratch: impl Into<PathBuf>,
        net: NetGrant,
        env: Env,
        resources: Limits,
    ) -> CapabilityBudget {
        CapabilityBudget::new(FsGrant::from_mounts(mounts, scratch), net, env, resources)
    }
}
