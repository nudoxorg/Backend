//! The `ForgeRuntime` — owned compile-plane capabilities (DAEMON-PLAN §2.5).
//!
//! Replaces every compile-plane process global with an owned, injected object:
//! the cage, the CAS, the toolchain set, the override table, the observer, and
//! the node identity. Assembly is a Cold→Ready typestate: a `Production` policy
//! refuses to yield a runtime without a production-grade cage.
//!
//! The server owns one `ForgeRuntime<Ready>` and passes it (`Arc`) into the
//! compile path, which reads nothing from process globals or the environment.

use std::collections::HashMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cas::{DynCas, Tiered};
use compiler::languages::producer::ForgeContext;
use registry::StoreCas;
use sandbox::{
	Cage, CageError, DevPassthrough, ForgeObserver, LinuxNamespaces, NodeId, NullObserver,
	OverrideTable, Policy, ToolchainSet, WorkerLang, WorkerPool, WorkerPoolConfig,
};

/// Cold: configured but not yet verified (no cage selected).
pub struct Cold;

/// Ready: a verified cage + resolved policy; implements [`ForgeContext`].
pub struct Ready;

/// Configuration inputs for a forge node (resolved from `ServerConfiguration`).
pub struct ForgeConfig {
	/// Explicit node id, or `None` to derive from the host.
	pub node: Option<String>,
	/// Per-profile / per-package limit overlays.
	pub overrides: HashMap<String, sandbox::LimitOverride>,
	/// Node-local CAS root (`{root}/cas/`), or `None` for memory-only.
	pub cas_root: Option<PathBuf>,
	/// Metrics observer.
	pub observer: Arc<dyn ForgeObserver>,
	/// L3 object-store backend for the tiered CAS.
	///
	/// When `Some`, a live [`registry::StoreCas`] is wired as the L3 tier so
	/// compile-plane cache misses fall through to the distributed object store.
	/// When `None` (development / tests), the forge runs L1+L2 only.
	pub l3: Option<StoreCas>,
}

impl Default for ForgeConfig {
	fn default() -> Self {
		Self {
			node: None,
			overrides: HashMap::new(),
			cas_root: None,
			observer: Arc::new(NullObserver),
			l3: None,
		}
	}
}

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

/// Owned compile-plane runtime, `S`-phased (`Cold` → `Ready`).
pub struct ForgeRuntime<S = Ready> {
	node: NodeId,
	policy: Policy,
	cage: Arc<dyn Cage>,
	/// The erased tiered CAS — `&dyn DynCas` is what `ForgeContext::cas()`
	/// returns, keeping `ForgeContext` object-safe regardless of L3 type.
	cas: Arc<dyn DynCas>,
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
	/// CAS, resolve toolchains, and warm interpreter worker pools.
	///
	/// A `tokio::runtime::Handle` must be provided so the sync compile path can
	/// drive the async CAS (kept explicit rather than a global).
	pub fn assemble(
		policy: Policy,
		cfg: ForgeConfig,
		handle: tokio::runtime::Handle,
	) -> Result<ForgeRuntime<Ready>, ForgeError> {
		let cage = select_cage(policy)?;

		// Build the tiered CAS, wiring StoreCas as L3 when the caller supplies
		// one. `DynCas` erases the concrete L3 type so `ForgeContext::cas()`
		// can return `&dyn DynCas` without making the trait generic over L3.
		let cas: Arc<dyn DynCas> = match (cfg.cas_root.as_deref(), cfg.l3) {
			(Some(root), Some(l3)) => {
				Arc::new(Tiered::with_l3(256, Some(cas::DiskCas::open(root)?), l3))
			},
			(Some(root), None) => Arc::new(Tiered::with_disk(256, cas::DiskCas::open(root)?)),
			(None, Some(l3)) => Arc::new(Tiered::with_l3(256, None, l3)),
			(None, None) => Arc::new(Tiered::memory_only(256)),
		};

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

	/// The shared CAS handle (erased to `dyn DynCas`).
	pub fn cas_handle(&self) -> &Arc<dyn DynCas> {
		&self.cas
	}
}

impl ForgeContext for ForgeRuntime<Ready> {
	fn node(&self) -> &NodeId {
		&self.node
	}
	fn cage(&self) -> &dyn Cage {
		self.cage.as_ref()
	}
	fn cas(&self) -> &dyn DynCas {
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

/// Select and verify a cage for `policy`.
///
/// `Production` demands a production-grade cage (LinuxNamespaces with bwrap);
/// `Development` yields a [`DevPassthrough`] (or LinuxNamespaces when present).
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

/// Resolve the producer-worker binary.
///
/// Order: `NUDOX_PRODUCER_WORKER` → same-dir `producer-worker` next to the
/// current exe → `producer-worker` on PATH. This is a server-plane env read
/// (allowed; the compiler never reads it).
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
