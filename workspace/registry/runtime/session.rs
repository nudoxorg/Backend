//! Per-session graph state.
//!
//! As a user explores, the nodes/edges they accumulate are merged into a
//! session-scoped graph, so subsequent expansions build on what they have
//! already seen. The tests pin the algebra exactly: merge is **union**,
//! **idempotent**, **order-insensitive**, sessions are **isolated**, and a
//! configured session is **persistent**. That is precisely a *join-semilattice*
//! — accumulation is monotone and the merge order never matters.
//!
//! Structural sharing is via [`Arc`]: a [`snapshot`](SessionStore::snapshot) is
//! a cheap clone of the shared graph, not a deep copy.

use std::{
	collections::{BTreeSet, HashMap},
	io::ErrorKind,
	path::PathBuf,
	sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

use serde::{Deserialize, Serialize};

use heart::{SymbolId, Id};

use crate::runtime::error::SessionError;

/// The kind of edge two symbols share in an exploration graph.
///
/// A plain relationship taxonomy — formerly homed in the (now removed) Terminus
/// graph store, kept here because the session semilattice is its only remaining
/// user. The live symbol-relationship surface is the IR reverse-index /
/// `Target::Usages` query plane, not this enum.
#[derive(
	Debug,
	Clone,
	Copy,
	PartialEq,
	Eq,
	PartialOrd,
	Ord,
	Hash,
	Serialize,
	Deserialize,
	strum::Display,
	strum::EnumString,
)]
pub enum RelationKind {
	/// The target is a member of the source (a method of a type, a field of a
	/// record, an item of a module).
	Member,
	/// The source references the target (a call, a use, a mention).
	Reference,
	/// The target occurs within the source's declaration/signature.
	Occurrence,
	/// The source implements the target (a type implements a trait/interface).
	Implements,
	/// The source extends/subclasses the target.
	Extends,
	/// The source re-exports the target.
	ReExport,
}

/// A directed, kinded edge between two symbols in a session graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Edge {
	/// The source symbol.
	pub from: SymbolId,
	/// The kind of relationship.
	pub kind: RelationKind,
	/// The target symbol.
	pub to: SymbolId,
}

/// The stable identity of an exploration session.
pub type SessionId = Id<SessionGraph>;

/// The accumulated graph of one exploration session: the set of symbols seen and
/// the set of edges between them. Both are `BTreeSet`s, so union is set-union and
/// the whole structure is a join-semilattice under [`SessionGraph::merge`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGraph {
	/// The symbols encountered so far.
	pub nodes: BTreeSet<SymbolId>,
	/// The edges encountered so far.
	pub edges: BTreeSet<Edge>,
}

impl SessionGraph {
	/// An empty session graph (the semilattice's bottom element).
	pub fn empty() -> Self { Self::default() }

	/// Whether this session has accumulated nothing.
	pub fn is_empty(&self) -> bool { self.nodes.is_empty() && self.edges.is_empty() }

	/// Fold `other` into `self` (a least-upper-bound / union step): union the node
	/// and edge sets. Idempotent, commutative, and associative because set-union
	/// is.
	///
	/// LAWS (relied on by the session tests; callers may assume them):
	/// - **idempotent**: `x.merge(x.clone())` leaves `x` unchanged;
	/// - **commutative**: `a.merge(b)` and `b.merge(a)` reach the same value;
	/// - **associative**: `(a ∨ b) ∨ c == a ∨ (b ∨ c)`.
	///
	/// Together these make merge *order-insensitive* and safe to apply repeatedly
	/// — exactly the guarantees a concurrent, replayable session needs.
	pub fn merge(&mut self, other: Self) {
		self.nodes.extend(other.nodes);
		self.edges.extend(other.edges);
	}
}

/// Where a session store keeps its persisted snapshots.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Persistence {
	/// Sessions live only in memory; `persist`/`load` are no-ops.
	Ephemeral,
	/// Sessions are snapshotted under this directory, one file per session, so a
	/// merged session survives a reopen.
	Directory(std::path::PathBuf),
}

/// Manages the lifecycle of session graphs: opening, merging deltas in,
/// snapshotting cheaply, clearing, and (optionally) persisting.
///
/// The graph is shared via [`Arc`] so a snapshot is a cheap reference-count
/// bump; merges swap in a new `Arc` (copy-on-write) so a held snapshot is never
/// mutated under the caller.
#[diagnostic::on_unimplemented(
	message = "`{Self}` is not a `SessionStore`",
	note = "implement `SessionStore` to manage per-session exploration graphs"
)]
pub trait SessionStore: Send + Sync {
	/// Open (creating if absent) the session with the given id, returning its
	/// current graph. When persistence is a directory and a snapshot exists on
	/// disk, it is loaded.
	async fn open(&self, session: SessionId) -> Result<Arc<SessionGraph>, SessionError>;

	/// Merge `delta` into the session, returning the unioned graph. Idempotent
	/// and order-insensitive per [`SessionGraph::merge`]; isolated to this
	/// `session`.
	async fn merge_into(
		&self,
		session: SessionId,
		delta: SessionGraph,
	) -> Result<Arc<SessionGraph>, SessionError>;

	/// A cheap, shared snapshot of the session's current graph.
	async fn snapshot(&self, session: SessionId) -> Result<Arc<SessionGraph>, SessionError>;

	/// Empty the session (and remove any persisted file).
	async fn clear(&self, session: SessionId) -> Result<(), SessionError>;

	/// Flush the session's current graph to durable storage (no-op when
	/// [`Persistence::Ephemeral`]).
	async fn persist(&self, session: SessionId) -> Result<(), SessionError>;

	/// Load a session's graph from durable storage, if present.
	async fn load(&self, session: SessionId) -> Result<Option<Arc<SessionGraph>>, SessionError>;
}

/// The default, in-process session store: a concurrent map of session id to a
/// copy-on-write [`Arc<SessionGraph>`], with optional directory-backed
/// persistence.
pub struct MemorySessionStore {
	persistence: Persistence,
	// The concurrent map of SessionId -> Arc<SessionGraph>; kept private so the
	// concurrency primitive can change without touching the API. Guards are only
	// ever held across pure set operations — never across an `.await`.
	sessions: RwLock<HashMap<SessionId, Arc<SessionGraph>>>,
}

impl MemorySessionStore {
	/// Create a store with the given persistence policy.
	pub fn new(persistence: Persistence) -> Self {
		Self { persistence, sessions: RwLock::new(HashMap::new()) }
	}

	/// Where this session's snapshot lives on disk, if persistence is configured.
	fn snapshot_path(&self, session: SessionId) -> Option<PathBuf> {
		match &self.persistence {
			Persistence::Ephemeral => None,
			Persistence::Directory(directory) => {
				Some(directory.join(format!("{}.json", session.as_uuid())))
			}
		}
	}

	// The lock is only ever held over infallible set unions and map inserts, so
	// poisoning (a panic while held) is provably unreachable; the `expect`s
	// document that rather than smuggle an impossible error into the API.
	fn sessions_read(&self) -> RwLockReadGuard<'_, HashMap<SessionId, Arc<SessionGraph>>> {
		self.sessions.read().expect("session map lock is never poisoned: guarded sections cannot panic")
	}

	fn sessions_write(&self) -> RwLockWriteGuard<'_, HashMap<SessionId, Arc<SessionGraph>>> {
		self.sessions.write().expect("session map lock is never poisoned: guarded sections cannot panic")
	}

	/// Read the persisted snapshot for `session`, if persistence is configured
	/// and a file is present. Missing file is `Ok(None)`, not an error.
	fn read_snapshot(&self, session: SessionId) -> Result<Option<SessionGraph>, SessionError> {
		let Some(path) = self.snapshot_path(session) else { return Ok(None) };
		match std::fs::read(&path) {
			Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(SessionError::Codec),
			Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
			Err(error) => Err(SessionError::Io(error)),
		}
	}
}

impl SessionStore for MemorySessionStore {
	async fn open(&self, session: SessionId) -> Result<Arc<SessionGraph>, SessionError> {
		if let Some(graph) = self.sessions_read().get(&session).cloned() {
			return Ok(graph);
		}
		// Not yet in memory: hydrate from disk (or start empty), then race-safely
		// insert — a concurrent opener's graph wins so we never clobber state.
		let loaded = Arc::new(self.read_snapshot(session)?.unwrap_or_default());
		let mut sessions = self.sessions_write();
		let graph = Arc::clone(sessions.entry(session).or_insert(loaded));
		tracing::debug!(%session, nodes = graph.nodes.len(), edges = graph.edges.len(), "session opened");
		Ok(graph)
	}

	async fn merge_into(
		&self,
		session: SessionId,
		delta: SessionGraph,
	) -> Result<Arc<SessionGraph>, SessionError> {
		// Opening first means a persisted snapshot is always folded in before the
		// delta, keeping the merge monotone across restarts too.
		self.open(session).await?;
		let mut sessions = self.sessions_write();
		let current = sessions.get_mut(&session).ok_or(SessionError::NotFound)?;
		// Copy-on-write: held snapshots keep the old Arc; readers see either the
		// old or the merged graph, never a half-merged one.
		let mut merged = SessionGraph::clone(current);
		merged.merge(delta);
		let merged = Arc::new(merged);
		*current = Arc::clone(&merged);
		Ok(merged)
	}

	async fn snapshot(&self, session: SessionId) -> Result<Arc<SessionGraph>, SessionError> {
		self.sessions_read().get(&session).cloned().ok_or(SessionError::NotFound)
	}

	async fn clear(&self, session: SessionId) -> Result<(), SessionError> {
		self.sessions_write().insert(session, Arc::new(SessionGraph::empty()));
		if let Some(path) = self.snapshot_path(session) {
			match std::fs::remove_file(&path) {
				Ok(()) => {}
				Err(error) if error.kind() == ErrorKind::NotFound => {}
				Err(error) => return Err(SessionError::Io(error)),
			}
		}
		tracing::debug!(%session, "session cleared");
		Ok(())
	}

	async fn persist(&self, session: SessionId) -> Result<(), SessionError> {
		let Some(path) = self.snapshot_path(session) else { return Ok(()) };
		let graph = self.snapshot(session).await?;
		if let Some(parent) = path.parent() {
			std::fs::create_dir_all(parent).map_err(SessionError::Io)?;
		}
		let bytes = serde_json::to_vec(graph.as_ref()).map_err(SessionError::Codec)?;
		// Write-then-rename so a crash mid-flush never leaves a torn snapshot.
		let staging = path.with_extension("json.tmp");
		std::fs::write(&staging, &bytes).map_err(SessionError::Io)?;
		std::fs::rename(&staging, &path).map_err(SessionError::Io)?;
		tracing::debug!(%session, bytes = bytes.len(), "session persisted");
		Ok(())
	}

	async fn load(&self, session: SessionId) -> Result<Option<Arc<SessionGraph>>, SessionError> {
		Ok(self.read_snapshot(session)?.map(Arc::new))
	}
}

// ─────────────────────────────────────────────────────────────────────────────
// ScratchSessionStore — a scratch-backed SessionStore (INDEX-PLAN ID-2, ID-19).
//
// Exploration-graph sessions live in the ephemeral `scratch.sqlite` `sessions`
// table (`graph_state` column), not the versioned catalog: they are working
// state, delete-anytime, never replicated. This replaces the former
// postgres-backed store as part of the Postgres exit (INDEX-PLAN IP-4).
// `MemorySessionStore` remains the pure in-process option; this one is durable
// across a process's own restart via the on-disk scratch database.
// ─────────────────────────────────────────────────────────────────────────────

use std::sync::Mutex;

use index::scratch::{ScratchError, ScratchStore};

/// The placeholder writer host recorded for exploration-graph sessions.
///
/// The scratch `sessions` table's `writer_host` column exists for the
/// writer-sticky catalog-read guarantee (ID-19); exploration-graph sessions do
/// not participate in that routing, so they are all pinned to this single
/// sentinel. Keeping the column non-null lets one table serve both concerns.
const EXPLORATION_WRITER_HOST: &str = "exploration";

/// A [`SessionStore`] backed by the ephemeral [`ScratchStore`]: each session's
/// [`SessionGraph`] is one JSON `graph_state` row keyed by the session uuid.
///
/// The scratch store is a single `rusqlite` connection (`!Sync`), so it is held
/// behind a [`Mutex`]. Every method does its read → merge → write entirely under
/// the lock and never holds the guard across an `.await`, so concurrent
/// [`SessionStore::merge_into`] calls on the same session **serialize** and no
/// delta is lost. Because [`SessionGraph::merge`] is a join-semilattice union
/// (idempotent, commutative, associative) the serialized order is irrelevant —
/// the result is the least upper bound either way.
pub struct ScratchSessionStore {
	scratch: Mutex<ScratchStore>,
}

impl ScratchSessionStore {
	/// Wrap an already-opened [`ScratchStore`]. The scratch schema (including the
	/// `sessions` table) is created by [`ScratchStore::open`], so there is no
	/// separate migrate step.
	pub fn new(scratch: ScratchStore) -> Self {
		Self { scratch: Mutex::new(scratch) }
	}

	/// The lock is only ever held over infallible-to-acquire scratch calls and is
	/// never held across an `.await`, so poisoning (a panic while held) is
	/// unreachable; the `expect` documents that.
	fn locked(&self) -> std::sync::MutexGuard<'_, ScratchStore> {
		self.scratch
			.lock()
			.expect("scratch session mutex is never poisoned: guarded sections cannot panic")
	}

	/// Ensure a session row exists (idempotent), reading nothing back.
	fn ensure_row(store: &ScratchStore, session: SessionId) -> Result<(), SessionError> {
		let session_id = session.as_uuid().to_string();
		if store.get_session(&session_id).map_err(scratch_error)?.is_none() {
			// Create pins the sentinel writer host; a lost race (a concurrent
			// creator winning) surfaces as a UNIQUE violation which we treat as
			// "already exists".
			match store.create_session(&session_id, EXPLORATION_WRITER_HOST, now_unix()) {
				Ok(()) => {}
				Err(error) if error.is_unique_violation() => {}
				Err(other) => return Err(scratch_error(other)),
			}
		}
		Ok(())
	}

	/// Read a session's stored graph, if the row exists and carries a graph.
	fn read_graph(
		store: &ScratchStore,
		session: SessionId,
	) -> Result<Option<SessionGraph>, SessionError> {
		let session_id = session.as_uuid().to_string();
		match store.session_graph_state(&session_id).map_err(scratch_error)? {
			Some(json) => Ok(Some(serde_json::from_str(&json).map_err(SessionError::Codec)?)),
			None => Ok(None),
		}
	}

	/// Write a session's graph back to `graph_state`.
	fn write_graph(
		store: &ScratchStore,
		session: SessionId,
		graph: &SessionGraph,
	) -> Result<(), SessionError> {
		let session_id = session.as_uuid().to_string();
		let json = serde_json::to_string(graph).map_err(SessionError::Codec)?;
		store
			.set_session_graph_state(&session_id, Some(&json), now_unix())
			.map_err(scratch_error)
	}
}

impl SessionStore for ScratchSessionStore {
	async fn open(&self, session: SessionId) -> Result<Arc<SessionGraph>, SessionError> {
		let store = self.locked();
		Self::ensure_row(&store, session)?;
		let graph = Self::read_graph(&store, session)?.unwrap_or_default();
		Ok(Arc::new(graph))
	}

	async fn merge_into(
		&self,
		session: SessionId,
		delta: SessionGraph,
	) -> Result<Arc<SessionGraph>, SessionError> {
		// The whole read → merge → write runs under the scratch mutex, so two
		// tasks merging into the same session serialize; semilattice union makes
		// the interleaving irrelevant.
		let store = self.locked();
		Self::ensure_row(&store, session)?;
		let mut current = Self::read_graph(&store, session)?.unwrap_or_default();
		current.merge(delta);
		Self::write_graph(&store, session, &current)?;
		Ok(Arc::new(current))
	}

	async fn snapshot(&self, session: SessionId) -> Result<Arc<SessionGraph>, SessionError> {
		let store = self.locked();
		Self::read_graph(&store, session)?.map(Arc::new).ok_or(SessionError::NotFound)
	}

	async fn clear(&self, session: SessionId) -> Result<(), SessionError> {
		// Reset the graph to empty rather than deleting the row, mirroring
		// `MemorySessionStore::clear` (which keeps an empty session live).
		let store = self.locked();
		Self::ensure_row(&store, session)?;
		Self::write_graph(&store, session, &SessionGraph::empty())
	}

	async fn persist(&self, _session: SessionId) -> Result<(), SessionError> {
		// Every mutation already commits durably to the scratch sqlite file, so
		// there is no separate flush step — `persist` is a no-op here.
		Ok(())
	}

	async fn load(&self, session: SessionId) -> Result<Option<Arc<SessionGraph>>, SessionError> {
		let store = self.locked();
		Ok(Self::read_graph(&store, session)?.map(Arc::new))
	}
}

/// Wall-clock seconds since the Unix epoch, for the scratch `last_seen_at` /
/// `created_at` bookkeeping columns.
fn now_unix() -> i64 {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map(|d| d.as_secs() as i64)
		.unwrap_or(0)
}

/// Lift a scratch-store error into the session error surface.
fn scratch_error(error: ScratchError) -> SessionError {
	SessionError::Scratch(error)
}

#[cfg(test)]
mod scratch_session_tests {
	use super::*;

	fn store() -> ScratchSessionStore {
		ScratchSessionStore::new(
			ScratchStore::open_in_memory().expect("in-memory scratch open must not fail"),
		)
	}

	fn edge(from: SymbolId, to: SymbolId) -> Edge {
		Edge { from, kind: RelationKind::Reference, to }
	}

	#[tokio::test]
	async fn open_then_merge_round_trips_through_scratch() {
		let sessions = store();
		let id = SessionId::new_random();

		// A fresh session opens empty.
		let opened = sessions.open(id).await.expect("open must succeed");
		assert!(opened.is_empty());

		// Merge accumulates and is durable to a re-open (same store).
		let a = SymbolId::new_random();
		let b = SymbolId::new_random();
		let mut delta = SessionGraph::empty();
		delta.nodes.insert(a);
		delta.nodes.insert(b);
		delta.edges.insert(edge(a, b));
		let merged = sessions.merge_into(id, delta).await.expect("merge must succeed");
		assert_eq!(merged.nodes.len(), 2);
		assert_eq!(merged.edges.len(), 1);

		let reopened = sessions.open(id).await.expect("reopen must succeed");
		assert_eq!(reopened.nodes.len(), 2, "graph must survive re-open via scratch");
	}

	#[tokio::test]
	async fn merge_is_idempotent_and_isolated() {
		let sessions = store();
		let first = SessionId::new_random();
		let second = SessionId::new_random();

		let a = SymbolId::new_random();
		let mut delta = SessionGraph::empty();
		delta.nodes.insert(a);

		sessions.merge_into(first, delta.clone()).await.expect("merge must succeed");
		// Idempotent: re-merging the same delta does not grow the graph.
		let again = sessions.merge_into(first, delta).await.expect("merge must succeed");
		assert_eq!(again.nodes.len(), 1);

		// Isolated: the second session is untouched.
		let other = sessions.open(second).await.expect("open must succeed");
		assert!(other.is_empty());
	}

	#[tokio::test]
	async fn clear_empties_but_keeps_session_live() {
		let sessions = store();
		let id = SessionId::new_random();
		let mut delta = SessionGraph::empty();
		delta.nodes.insert(SymbolId::new_random());
		sessions.merge_into(id, delta).await.expect("merge must succeed");

		sessions.clear(id).await.expect("clear must succeed");
		let after = sessions.open(id).await.expect("open must succeed");
		assert!(after.is_empty(), "cleared session opens empty but still exists");
	}
}
