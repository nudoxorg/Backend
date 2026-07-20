//! The two deployment shapes (INDEX-PLAN §3) — frozen early.
//!
//! Same crates, different feature sets and process topology. `Remote` runs
//! `server` + `ingestor` + N forge workers; `Embedded` hosts everything in
//! one GUI process with exactly one compile cage.

use std::num::NonZeroUsize;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which of the two shapes this process is running as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeploymentKind {
    /// Multi-compiler, multi-host, HA, enterprise remotes.
    Remote,
    /// One process, one compiler, local catalog clone, offline branches.
    Embedded,
}

/// A remote endpoint an enrolled device may exchange bulk data with
/// (INDEX-PLAN ID-18: local uploads go only to trusted remotes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedRemote {
    /// Human-chosen handle, unique within the profile.
    pub name: String,
    /// iroh endpoint identifier (z-base-32 public key string).
    pub endpoint: String,
    /// This device may `provide` ObjectPacks / IR changes to the remote.
    pub can_provide: bool,
    /// This device may fetch from the remote.
    pub can_fetch: bool,
}

/// Everything topology-shaped a process needs at startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeploymentProfile {
    pub kind: DeploymentKind,
    /// Remote: N; Embedded: always 1.
    pub max_concurrent_cages: NonZeroUsize,
    pub trusted_remotes: Vec<TrustedRemote>,
    /// Directory holding `catalog.dolt`.
    pub catalog_path: PathBuf,
    /// Root of the local libpijul `IrRepository`.
    pub ir_repo_root: PathBuf,
    /// Root of the local ObjectPack store.
    pub object_pack_root: PathBuf,
}

impl DeploymentProfile {
    /// The embedded (GUI) shape: one cage, app-data-relative stores.
    pub fn embedded(data_root: PathBuf) -> Self {
        Self {
            kind: DeploymentKind::Embedded,
            max_concurrent_cages: NonZeroUsize::MIN,
            trusted_remotes: Vec::new(),
            catalog_path: data_root.join("catalog.dolt"),
            ir_repo_root: data_root.join("ir"),
            object_pack_root: data_root.join("objects"),
        }
    }
}
