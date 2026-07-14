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

use crate::runtime::{error::SessionError, graph::RelationKind};

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
// PgSessionStore — a postgres-backed SessionStore for horizontally-scaled
// gateways (DAEMON-PLAN §2.5: sessions → postgres so any replica serves any
// session). Additive; `MemorySessionStore` remains the single-node default.
// ─────────────────────────────────────────────────────────────────────────────

use sqlx::Row as _;

/// A postgres-backed [`SessionStore`]: each session's [`SessionGraph`] is one
/// `jsonb` row keyed by session uuid, so **any** gateway replica can open or
/// extend **any** session (replacing the node-local `MemorySessionStore`, whose
/// state is invisible to sibling replicas).
///
/// Concurrency is correct without a CRDT-on-the-wire: [`merge_into`] does its
/// read → merge → write inside one transaction under a `SELECT ... FOR UPDATE`
/// row lock, so two replicas merging deltas into the same session **serialize**
/// and neither delta is lost. Because [`SessionGraph::merge`] is a
/// join-semilattice union (idempotent, commutative, associative), the serialized
/// order is irrelevant — the result is the least upper bound either way. That is
/// what makes the persisted store safe under concurrent replicas.
///
/// The single `sessions` table is created (`IF NOT EXISTS`) by
/// [`PgSessionStore::migrate`]; it is intentionally self-contained so the runtime
/// crate need not depend on the registry schema module.
pub struct PgSessionStore {
	pool: sqlx::PgPool,
}

impl PgSessionStore {
	/// Wrap a postgres pool. Call [`migrate`](Self::migrate) once before use to
	/// ensure the backing table exists.
	pub fn new(pool: sqlx::PgPool) -> Self { Self { pool } }

	/// Create the `sessions` table if absent (`id uuid PK`, `graph jsonb`,
	/// `updated_at timestamptz`). Idempotent — safe to call on every boot.
	pub async fn migrate(&self) -> Result<(), SessionError> {
		sqlx::query(
			"CREATE TABLE IF NOT EXISTS sessions (\
			   id uuid PRIMARY KEY, \
			   graph jsonb NOT NULL, \
			   updated_at timestamptz NOT NULL DEFAULT now()\
			 )",
		)
		.execute(&self.pool)
		.await
		.map_err(SessionError::Database)?;
		Ok(())
	}

	/// Read a session's stored graph, if the row exists.
	async fn read_row(&self, session: SessionId) -> Result<Option<SessionGraph>, SessionError> {
		let row = sqlx::query("SELECT graph FROM sessions WHERE id = $1")
			.bind(session.as_uuid())
			.fetch_optional(&self.pool)
			.await
			.map_err(SessionError::Database)?;
		match row {
			Some(r) => {
				let json: serde_json::Value = r.try_get(0).map_err(SessionError::Database)?;
				let graph = serde_json::from_value(json).map_err(SessionError::Codec)?;
				Ok(Some(graph))
			}
			None => Ok(None),
		}
	}
}

impl SessionStore for PgSessionStore {
	async fn open(&self, session: SessionId) -> Result<Arc<SessionGraph>, SessionError> {
		// Ensure a row exists (empty graph) and return the current graph. The
		// upsert is `DO NOTHING` so a concurrent open never clobbers accumulated
		// state; we then read the authoritative row back.
		let empty = serde_json::to_value(SessionGraph::empty()).map_err(SessionError::Codec)?;
		sqlx::query("INSERT INTO sessions (id, graph) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
			.bind(session.as_uuid())
			.bind(&empty)
			.execute(&self.pool)
			.await
			.map_err(SessionError::Database)?;
		let graph = self.read_row(session).await?.unwrap_or_default();
		Ok(Arc::new(graph))
	}

	async fn merge_into(
		&self,
		session: SessionId,
		delta: SessionGraph,
	) -> Result<Arc<SessionGraph>, SessionError> {
		// Serialize concurrent merges on the same session with a row lock: read
		// the current graph FOR UPDATE, union the delta, write it back — all in
		// one transaction. Semilattice union makes the interleaving irrelevant;
		// the lock only guarantees no lost update.
		let mut tx = self.pool.begin().await.map_err(SessionError::Database)?;

		// Ensure the row exists so `FOR UPDATE` has something to lock.
		let empty = serde_json::to_value(SessionGraph::empty()).map_err(SessionError::Codec)?;
		sqlx::query("INSERT INTO sessions (id, graph) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING")
			.bind(session.as_uuid())
			.bind(&empty)
			.execute(&mut *tx)
			.await
			.map_err(SessionError::Database)?;

		let row = sqlx::query("SELECT graph FROM sessions WHERE id = $1 FOR UPDATE")
			.bind(session.as_uuid())
			.fetch_one(&mut *tx)
			.await
			.map_err(SessionError::Database)?;
		let json: serde_json::Value = row.try_get(0).map_err(SessionError::Database)?;
		let mut current: SessionGraph = serde_json::from_value(json).map_err(SessionError::Codec)?;

		current.merge(delta);
		let merged_json = serde_json::to_value(&current).map_err(SessionError::Codec)?;
		sqlx::query("UPDATE sessions SET graph = $2, updated_at = now() WHERE id = $1")
			.bind(session.as_uuid())
			.bind(&merged_json)
			.execute(&mut *tx)
			.await
			.map_err(SessionError::Database)?;

		tx.commit().await.map_err(SessionError::Database)?;
		Ok(Arc::new(current))
	}

	async fn snapshot(&self, session: SessionId) -> Result<Arc<SessionGraph>, SessionError> {
		self.read_row(session).await?.map(Arc::new).ok_or(SessionError::NotFound)
	}

	async fn clear(&self, session: SessionId) -> Result<(), SessionError> {
		// Reset the graph to empty rather than deleting the row, mirroring
		// `MemorySessionStore::clear` (which keeps an empty session live).
		let empty = serde_json::to_value(SessionGraph::empty()).map_err(SessionError::Codec)?;
		sqlx::query(
			"INSERT INTO sessions (id, graph) VALUES ($1, $2) \
			 ON CONFLICT (id) DO UPDATE SET graph = $2, updated_at = now()",
		)
		.bind(session.as_uuid())
		.bind(&empty)
		.execute(&self.pool)
		.await
		.map_err(SessionError::Database)?;
		Ok(())
	}

	async fn persist(&self, _session: SessionId) -> Result<(), SessionError> {
		// Every mutation already commits durably to postgres, so there is no
		// separate flush step — `persist` is a no-op for the pg-backed store.
		Ok(())
	}

	async fn load(&self, session: SessionId) -> Result<Option<Arc<SessionGraph>>, SessionError> {
		Ok(self.read_row(session).await?.map(Arc::new))
	}
}
