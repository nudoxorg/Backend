//! The `ForgeRuntime` for the compiler daemon — owned compile-plane capabilities.
//!
//! Ported from `server/forge.rs`; stripped of the server-only `registry::StoreCas`
//! L3 tier.  The daemon runs with a local L1+L2 tiered CAS (no distributed object
//! store at the compiler side of the wire boundary).
//!
//! Assembly follows the same Cold→Ready typestate pattern:
//! `ForgeRuntime<Cold>::assemble(...)` → `ForgeRuntime<Ready>`, which implements
//! `ForgeContext` and can be passed directly into `compiler::generate::generate_with`.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bytes::Bytes;
use cas::{Cas, CasError, ContentHash, DiskCas, Tiered};
use sandbox::{
    Cage, CageError, DevPassthrough, ForgeObserver, LinuxNamespaces, NodeId, NullObserver,
    OverrideTable, Policy, ToolchainSet, WorkerLang, WorkerPool, WorkerPoolConfig,
};

use crate::compile::producer::ForgeContext;

// ─────────────────────────────────────────────────────────────────────────────
// Daemon L3: always "none" — the daemon has no distributed object store.
// We mirror the NoL3 semantics inline so the CAS type is fully concrete.
// ─────────────────────────────────────────────────────────────────────────────

/// A placeholder L3 type that always returns `Unsupported`.
///
/// The daemon runs with L1 (memory) + optional L2 (disk) only; there is no
/// distributed object store at this side of the wire boundary.
pub enum DaemonL3 {
    /// No L3 configured (the only variant for the daemon).
    None,
}

impl Cas for DaemonL3 {
    async fn get(&self, _key: ContentHash) -> Result<Option<Bytes>, CasError> {
        Err(CasError::Unsupported("L3 not configured in compiler daemon"))
    }
    async fn put(&self, _bytes: Bytes) -> Result<ContentHash, CasError> {
        Err(CasError::Unsupported("L3 not configured in compiler daemon"))
    }
    async fn put_keyed(&self, _key: ContentHash, _bytes: Bytes) -> Result<bool, CasError> {
        Err(CasError::Unsupported("L3 not configured in compiler daemon"))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Typestates
// ─────────────────────────────────────────────────────────────────────────────

/// Cold: configured but not yet verified (no cage selected).
pub struct Cold;

/// Ready: a verified cage + resolved policy; implements [`ForgeContext`].
pub struct Ready;

// ─────────────────────────────────────────────────────────────────────────────
// ForgeConfig
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration inputs for the daemon's forge node.
pub struct ForgeConfig {
    /// Explicit node id, or `None` to derive from the host.
    pub node: Option<String>,
    /// Per-profile / per-package limit overlays.
    pub overrides: HashMap<String, sandbox::LimitOverride>,
    /// Node-local CAS root (`{root}/cas/`), or `None` for memory-only.
    pub cas_root: Option<PathBuf>,
    /// Metrics observer.
    pub observer: Arc<dyn ForgeObserver>,
}

impl Default for ForgeConfig {
    fn default() -> Self {
        Self {
            node: None,
            overrides: HashMap::new(),
            cas_root: None,
            observer: Arc::new(NullObserver),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ForgeError
// ─────────────────────────────────────────────────────────────────────────────

/// Why forge assembly failed.
#[derive(Debug, thiserror::Error)]
pub enum ForgeError {
    /// No production-grade cage on this host under `Policy::Production`.
    #[error("no production-grade cage available: {0}")]
    NoProductionCage(String),

    /// Cage construction failed.
    #[error("cage construction failed")]
    Cage(#[from] CageError),

    /// CAS open failed.
    #[error("cas open failed")]
    Cas(#[from] cas::CasError),
}

// ─────────────────────────────────────────────────────────────────────────────
// ForgeRuntime
// ─────────────────────────────────────────────────────────────────────────────

/// Owned compile-plane runtime for the compiler daemon.
///
/// `S` is the typestate: `Cold` (not yet assembled) or `Ready` (cage verified,
/// implements [`ForgeContext`]).
pub struct ForgeRuntime<S = Ready> {
    node: NodeId,
    policy: Policy,
    cage: Arc<dyn Cage>,
    cas: Arc<Tiered<DaemonL3>>,
    toolchains: ToolchainSet,
    overrides: OverrideTable,
    observer: Arc<dyn ForgeObserver>,
    handle: tokio::runtime::Handle,
    nix_pool: Option<WorkerPool>,
    parser_pool: Option<WorkerPool>,
    require_worker: bool,
    _state: PhantomData<S>,
}

impl ForgeRuntime<Cold> {
    /// Assemble a ready runtime: select and verify a cage for `policy`, open the
    /// CAS, resolve toolchains from env, and warm interpreter worker pools.
    pub fn assemble(
        policy: Policy,
        cfg: ForgeConfig,
        handle: tokio::runtime::Handle,
    ) -> Result<ForgeRuntime<Ready>, ForgeError> {
        let cage = select_cage(policy)?;

        let l3 = DaemonL3::None;
        let l2 = match cfg.cas_root.as_deref() {
            Some(root) => Some(cas::DiskCas::open(root)?),
            None => None,
        };
        let cas: Arc<Tiered<DaemonL3>> = Arc::new(Tiered::with_l3(256, l2, l3));

        let node = match cfg.node {
            Some(name) => NodeId::new(name),
            None => NodeId::from_host(),
        };

        let toolchains = ToolchainSet::from_env();
        let overrides = OverrideTable::from_config(cfg.overrides);

        let require_worker = sandbox::IsolationPolicy::require_worker();
        let worker_bin = producer_worker_bin();
        let nix_pool = worker_bin
            .clone()
            .and_then(|bin| WorkerPool::new(WorkerPoolConfig::nix(bin)).ok());
        let parser_pool = worker_bin
            .and_then(|bin| WorkerPool::new(WorkerPoolConfig::static_parser(bin)).ok());

        Ok(ForgeRuntime {
            node,
            policy,
            cage,
            cas,
            toolchains,
            overrides,
            observer: cfg.observer,
            handle,
            nix_pool,
            parser_pool,
            require_worker,
            _state: PhantomData,
        })
    }
}

impl ForgeRuntime<Ready> {
    /// The resolved policy.
    pub fn policy(&self) -> Policy {
        self.policy
    }
}

impl ForgeContext for ForgeRuntime<Ready> {
    type Cas = Tiered<DaemonL3>;

    fn node(&self) -> &NodeId {
        &self.node
    }
    fn cage(&self) -> &dyn Cage {
        self.cage.as_ref()
    }
    fn cas(&self) -> &Tiered<DaemonL3> {
        self.cas.as_ref()
    }
    fn toolchains(&self) -> &ToolchainSet {
        &self.toolchains
    }
    fn overrides(&self) -> &OverrideTable {
        &self.overrides
    }
    fn observer(&self) -> &dyn ForgeObserver {
        self.observer.as_ref()
    }
    fn handle(&self) -> &tokio::runtime::Handle {
        &self.handle
    }
    fn worker_pool(&self, lang: WorkerLang) -> Option<&WorkerPool> {
        match lang {
            WorkerLang::Nix => self.nix_pool.as_ref(),
            WorkerLang::Typescript | WorkerLang::Python => self.parser_pool.as_ref(),
        }
    }
    fn require_worker(&self) -> bool {
        self.require_worker
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cage selection
// ─────────────────────────────────────────────────────────────────────────────

/// Select and verify a cage for `policy`.
///
/// `Production` demands a production-grade cage (LinuxNamespaces + bwrap);
/// `Development` yields a [`DevPassthrough`] (or LinuxNamespaces when available).
pub fn select_cage(policy: Policy) -> Result<Arc<dyn Cage>, ForgeError> {
    match policy {
        Policy::Production => {
            let linux = LinuxNamespaces::new();
            if linux.probe_available() && Cage::capabilities(&linux).production_grade {
                Ok(Arc::new(linux))
            } else {
                Err(ForgeError::NoProductionCage(
                    "bwrap not available or backend not production-grade".into(),
                ))
            }
        }
        Policy::Development => {
            let linux = LinuxNamespaces::new();
            if linux.probe_available() {
                Ok(Arc::new(linux))
            } else {
                Ok(Arc::new(DevPassthrough::try_new(Policy::Development)?))
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Producer-worker binary resolution
// ─────────────────────────────────────────────────────────────────────────────

/// Resolve the producer-worker binary.
///
/// Order: `NUDOX_PRODUCER_WORKER` env var → same-dir `producer-worker` next to
/// the current exe → `producer-worker` on PATH.
pub fn producer_worker_bin() -> Option<PathBuf> {
    if let Some(raw) = std::env::var_os("NUDOX_PRODUCER_WORKER") {
        let path = PathBuf::from(raw);
        if path.exists() {
            return Some(path);
        }
        if let Ok(w) = which::which(&path) {
            return Some(w);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("producer-worker");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    which::which(Path::new("producer-worker")).ok()
}
