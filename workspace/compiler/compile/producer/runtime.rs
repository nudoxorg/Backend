//! The injected compile-plane context (DAEMON-PLAN §2.5).
//!
//! [`ForgeContext`] replaces every compile-plane process global: the cage, the
//! CAS, the toolchain set, the override table, the observer, and the node
//! identity are all borrowed from an owned runtime instead of read from
//! `OnceLock`s or the environment.
//!
//! [`run_producer`] is the sole cache client: `cas.get` → hit (decode) or miss
//! (run under the cage / in-process, then `cas.put`), with observer events on
//! the boundary.

use bytes::Bytes;
use heart::cache::{Cas, EvictableCas, Tiered};
use heart::ContentHash;
use sandbox::{
	Cage, ForgeObserver, NodeId, OverrideTable, SealedInput, ToolchainSet, WorkerLang,
	WorkerPool,
};
use serde::{Deserialize, Serialize};

use super::{Producer, ProducerError, ProducerOutput};

/// Borrowed compile-plane capabilities, injected by the server's `ForgeRuntime`.
///
/// The `compiler` crate reads nothing from process globals or the environment;
/// everything policy-relevant arrives through this trait.
pub trait ForgeContext: Send + Sync {
	/// The concrete content-addressed store this context owns.
	///
	/// A fully-generic associated type (not a boxed `dyn`): the CAS is the sole
	/// erasure point in the design, and it is erased *statically*. The
	/// [`EvictableCas`] bound lets the poison-repair path invalidate a wrong
	/// entry without any object-safety gymnastics.
	type Cas: Cas + EvictableCas;

	/// Stable node identity (scratch / cgroup naming).
	fn node(&self) -> &NodeId;

	/// The isolation cage sealed commands run under.
	fn cage(&self) -> &dyn Cage;

	/// The content-addressed store (sole cache).
	///
	/// Returns the concrete `&Self::Cas`. Because this names an associated type,
	/// `ForgeContext` is consumed *generically* (`fn f<C: ForgeContext>(ctx: &C)`)
	/// rather than through a `&dyn ForgeContext` trait object — static erasure is
	/// the single, uniform strategy across the whole design.
	fn cas(&self) -> &Self::Cas;

	/// Resolved toolchain store paths (hashed into every job key).
	fn toolchains(&self) -> &ToolchainSet;

	/// Typed per-profile / per-package limit overlays.
	fn overrides(&self) -> &OverrideTable;

	/// Metrics boundary (cache hit/miss, producer finished/killed).
	fn observer(&self) -> &dyn ForgeObserver;

	/// A tokio handle to drive async CAS calls from the sync compile path.
	fn handle(&self) -> &tokio::runtime::Handle;

	/// The interpreter worker pool for `lang`, when one is configured.
	///
	/// Library-form producers (nix/ts/python) run here under the cage.
	/// `None` in a dev context with no worker binary → in-process fallback.
	fn worker_pool(&self, _lang: WorkerLang) -> Option<&WorkerPool> {
		None
	}

	/// Whether a worker is mandatory (production): no in-process fallback.
	fn require_worker(&self) -> bool {
		false
	}
}

/// Drive a CAS future from the sync compile path using the context's handle.
pub fn block_on<C: ForgeContext, F: std::future::Future>(ctx: &C, fut: F) -> F::Output {
	// Prefer the injected handle; when this thread is already on a multi-thread
	// runtime, park it with block_in_place so we do not panic on a nested block.
	if tokio::runtime::Handle::try_current().is_ok() {
		tokio::task::block_in_place(|| ctx.handle().block_on(fut))
	} else {
		ctx.handle().block_on(fut)
	}
}

/// Look opaque bytes up in the context CAS, recording the observer boundary.
pub fn cas_get<C: ForgeContext>(ctx: &C, key: ContentHash) -> Option<Bytes> {
	match block_on(ctx, ctx.cas().get(key)) {
		Ok(Some(bytes)) => {
			ctx.observer().cache_hit();
			Some(bytes)
		},
		Ok(None) => {
			ctx.observer().cache_miss();
			None
		},
		Err(e) => {
			tracing::warn!(error = %e, "cas get failed");
			ctx.observer().cache_miss();
			None
		},
	}
}

/// Store opaque bytes under `key` (best-effort, first-write-wins).
pub fn cas_put<C: ForgeContext>(ctx: &C, key: ContentHash, bytes: Bytes) {
	if let Err(e) = block_on(ctx, ctx.cas().put_keyed(key, bytes)) {
		tracing::warn!(error = %e, "cas put failed");
	}
}

/// Drop a poison / wrong-version entry so a subsequent put can land.
///
/// Requires `C::Cas: EvictableCas`, which the [`ForgeContext::Cas`] bound already
/// guarantees — object-store L3 tiers cannot evict, but the tiered composition
/// evicts L1/L2 (never the immutable L3).
pub fn cas_invalidate<C: ForgeContext>(ctx: &C, key: ContentHash) {
	if let Err(e) = block_on(ctx, ctx.cas().invalidate(key)) {
		tracing::warn!(error = %e, "cas invalidate failed");
	}
}

/// Postcard get-or-compute against the context CAS — the sole cache client.
pub fn cache_get_or_build<C, T, F, E>(
	ctx: &C,
	key: ContentHash,
	build: F,
) -> Result<T, E>
where
	C: ForgeContext,
	T: Serialize + for<'de> Deserialize<'de>,
	F: FnOnce() -> Result<T, E>,
{
	if let Some(bytes) = cas_get(ctx, key) {
		match postcard::from_bytes::<T>(&bytes) {
			Ok(value) => {
				tracing::debug!(key = %key, "cas hit");
				return Ok(value);
			},
			Err(e) => {
				tracing::warn!(key = %key, error = %e, "cas value decode failed; invalidating");
				cas_invalidate(ctx, key);
			},
		}
	}

	let value = build()?;
	if let Ok(bytes) = postcard::to_allocvec(&value) {
		cas_put(ctx, key, Bytes::from(bytes));
	}
	Ok(value)
}

/// The sole cache client for a single producer run (DAEMON-PLAN §2.4).
///
/// `input.key` is the CAS key. On a hit the postcard-encoded [`ProducerOutput`]
/// is decoded; on a miss the plan executes under the cage (or in-process for
/// adaptive producers), the output is stored, and observer events fire.
pub fn run_producer<C: ForgeContext, P: Producer + ?Sized>(
	ctx: &C,
	p: &P,
	input: &SealedInput,
) -> Result<ProducerOutput, ProducerError> {
	let key = input.key.as_hash();

	if let Some(bytes) = cas_get(ctx, key) {
		match postcard::from_bytes::<ProducerOutput>(&bytes) {
			Ok(out) => {
				tracing::debug!(key = %key, "producer cache hit");
				return Ok(out);
			},
			Err(e) => {
				tracing::warn!(key = %key, error = %e, "producer cache decode failed; invalidating");
				cas_invalidate(ctx, key);
			},
		}
	}

	// Miss: run the producer. Adaptive producers (Rust RA, Java) own their
	// orchestration in `produce`; the default `produce` is plan → cage → decode.
	let out = p.produce(ctx, input)?;

	if let Ok(bytes) = postcard::to_allocvec(&out) {
		cas_put(ctx, key, Bytes::from(bytes));
	}
	Ok(out)
}

/// Execute a producer's plan under the injected cage / worker context.
pub fn execute_plan<C: ForgeContext, P: Producer + ?Sized>(
	ctx: &C,
	p: &P,
	input: &SealedInput,
) -> Result<ProducerOutput, ProducerError> {
	super::execute(ctx, p, input)
}

// ─── Local default context ─────────────────────────────────────────────────

/// A self-contained, owned [`ForgeContext`] for the compiler's default path
/// (bare `generate()` calls and unit tests).
///
/// It is *not* a process global: each instance owns its own memory CAS, dev
/// cage, and observer. The server injects its own `ForgeRuntime` instead. The
/// toolchain env read happens here at the sealer boundary (allowed), never in
/// the wider compiler.
pub struct LocalForgeContext {
	node: NodeId,
	cage: sandbox::DevPassthrough,
	cas: Tiered<heart::cache::NoL3>,
	toolchains: ToolchainSet,
	overrides: OverrideTable,
	observer: sandbox::NullObserver,
	runtime: Option<tokio::runtime::Runtime>,
	handle: tokio::runtime::Handle,
}

impl LocalForgeContext {
	/// Build a local context with a memory CAS and toolchains from env.
	///
	/// Reuses the ambient tokio handle when present (tests / server), otherwise
	/// owns a small current-thread runtime for the CAS bridge.
	pub fn new() -> Self {
		let (runtime, handle) = match tokio::runtime::Handle::try_current() {
			Ok(h) => (None, h),
			Err(_) => {
				let rt = tokio::runtime::Builder::new_current_thread()
					.enable_all()
					.build()
					.expect("local forge runtime");
				let h = rt.handle().clone();
				(Some(rt), h)
			},
		};
		Self {
			node: NodeId::from_host(),
			// Dev passthrough is fine for the default in-process path; production
			// runs come through the server's injected LinuxNamespaces cage.
			cage: sandbox::DevPassthrough::try_new(sandbox::Policy::Development)
				.expect("Development policy yields a dev cage"),
			cas: Tiered::memory_only(256),
			toolchains: ToolchainSet::from_env(),
			overrides: OverrideTable::empty(),
			observer: sandbox::NullObserver,
			runtime,
			handle,
		}
	}
}

impl Default for LocalForgeContext {
	fn default() -> Self {
		Self::new()
	}
}

impl Drop for LocalForgeContext {
	fn drop(&mut self) {
		// Ensure the owned runtime shuts down without blocking on stray tasks.
		if let Some(rt) = self.runtime.take() {
			rt.shutdown_background();
		}
	}
}

impl ForgeContext for LocalForgeContext {
	type Cas = Tiered<heart::cache::NoL3>;

	fn node(&self) -> &NodeId {
		&self.node
	}
	fn cage(&self) -> &dyn Cage {
		&self.cage
	}
	fn cas(&self) -> &Tiered<heart::cache::NoL3> {
		&self.cas
	}
	fn toolchains(&self) -> &ToolchainSet {
		&self.toolchains
	}
	fn overrides(&self) -> &OverrideTable {
		&self.overrides
	}
	fn observer(&self) -> &dyn ForgeObserver {
		&self.observer
	}
	fn handle(&self) -> &tokio::runtime::Handle {
		&self.handle
	}
}
