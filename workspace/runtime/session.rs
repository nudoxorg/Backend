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
	collections::BTreeSet,
	sync::Arc,
};

use serde::{Deserialize, Serialize};

use heart::{SymbolId, Id};

use crate::{error::SessionError, graph::RelationKind};

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
	// sessions: dashmap/parking_lot map of SessionId -> Arc<SessionGraph>; kept
	// opaque so the concurrency primitive can change without touching the API.
}

impl MemorySessionStore {
	/// Create a store with the given persistence policy.
	pub fn new(persistence: Persistence) -> Self {
		let _ = persistence;
		todo!("initialize the concurrent session map with the given persistence policy")
	}
}

impl SessionStore for MemorySessionStore {
	async fn open(&self, session: SessionId) -> Result<Arc<SessionGraph>, SessionError> {
		let _ = (&self.persistence, session);
		todo!("load from disk if configured+present, else insert an empty graph, return the Arc")
	}

	async fn merge_into(
		&self,
		session: SessionId,
		delta: SessionGraph,
	) -> Result<Arc<SessionGraph>, SessionError> {
		let _ = (session, delta);
		todo!("copy-on-write: clone current, SessionGraph::merge(delta), swap the Arc, return it")
	}

	async fn snapshot(&self, session: SessionId) -> Result<Arc<SessionGraph>, SessionError> {
		let _ = session;
		todo!("clone the current Arc for this session")
	}

	async fn clear(&self, session: SessionId) -> Result<(), SessionError> {
		let _ = session;
		todo!("drop in-memory state and remove the persisted file if any")
	}

	async fn persist(&self, session: SessionId) -> Result<(), SessionError> {
		let _ = session;
		todo!("serialize the snapshot to the session's file (no-op if Ephemeral)")
	}

	async fn load(&self, session: SessionId) -> Result<Option<Arc<SessionGraph>>, SessionError> {
		let _ = session;
		todo!("read + deserialize the session's file if present")
	}
}
