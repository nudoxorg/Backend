//! The search planner — the *only* place a [`SemanticGate`] is minted.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use runtime::vector::SemanticGate;

use crate::search::query::{AbstractQuery, Query, Search};

pub enum Plan {
	Precise,
	Semantic(SemanticGate),
}

/// How many semantic queries the default planner admits per window. Semantic
/// search is the expensive path; the budget keeps a chatty client from turning
/// every keystroke into an embedding round-trip.
const DEFAULT_SEMANTIC_BUDGET: u32 = 64;
const DEFAULT_SEMANTIC_WINDOW: Duration = Duration::from_secs(60);

pub struct SearchPlanner {
	/// The policy half: whether the expensive path is warranted *right now*.
	semantic_quota: Quota,
}

impl SearchPlanner {
	pub fn new() -> Self {
		Self::with_quota(DEFAULT_SEMANTIC_BUDGET, DEFAULT_SEMANTIC_WINDOW)
	}

	/// A planner with an explicit semantic budget — `capacity` gates per
	/// `window`. `capacity: 0` yields a planner that never plans semantically.
	pub fn with_quota(capacity: u32, window: Duration) -> Self {
		Self { semantic_quota: Quota::new(capacity, window) }
	}

	pub fn plan(&self, request: &Search<'_>) -> Plan {
		match &request.query {
			Query::Literal(_) => Plan::Precise,
			Query::Abstract(query) => {
				if self.semantic_quota.admit() {
					Plan::Semantic(SemanticGate::issue(match query {
						AbstractQuery::NaturalLanguage(_) => {
							"planner: natural-language query warrants the semantic surface"
						}
						AbstractQuery::CodeSnippet { .. } => {
							"planner: code-snippet query warrants the semantic surface"
						}
					}))
				} else {
					// Quota exhausted: degrade to the cheap precise surface. The
					// inverse — text silently escalating to semantic — never happens.
					tracing::debug!("semantic quota exhausted; degrading to precise search");
					Plan::Precise
				}
			}
		}
	}

	/// Authorize one explicit similar-items (sidebar) query, or `None` when the
	/// semantic budget is spent. The sidebar is a deliberate qdrant call, so it
	/// draws from the same budget as planned semantic searches.
	pub fn authorize_similar(&self) -> Option<SemanticGate> {
		self.semantic_quota
			.admit()
			.then(|| SemanticGate::issue("similar-items sidebar"))
	}

	/// Extend a planner-issued authorization across a federation fan-out: one
	/// user-visible semantic query touches every source, and each per-source
	/// query needs its own single-use token. Takes the *reason of an already
	/// issued gate*, and minting stays inside this module, so the "only the
	/// planner issues gates" invariant keeps one home.
	pub(crate) fn extend_across_federation(reason: &'static str) -> SemanticGate {
		SemanticGate::issue(reason)
	}
}

impl Default for SearchPlanner {
	fn default() -> Self { Self::new() }
}

/// A fixed-window budget: `capacity` admissions per `window`, then denial until
/// the window rolls. Coarse on purpose — this bounds blast radius, it does not
/// bill anyone.
struct Quota {
	capacity: u32,
	window: Duration,
	state: Mutex<QuotaWindow>,
}

struct QuotaWindow {
	opened: Instant,
	admitted: u32,
}

impl Quota {
	fn new(capacity: u32, window: Duration) -> Self {
		Self {
			capacity,
			window,
			state: Mutex::new(QuotaWindow { opened: Instant::now(), admitted: 0 }),
		}
	}

	fn admit(&self) -> bool {
		let mut window = match self.state.lock() {
			Ok(window) => window,
			// A poisoned window means a panic mid-admit; deny the expensive path
			// rather than run unmetered.
			Err(_) => return false,
		};
		if window.opened.elapsed() >= self.window {
			window.opened = Instant::now();
			window.admitted = 0;
		}
		if window.admitted < self.capacity {
			window.admitted += 1;
			true
		} else {
			false
		}
	}
}
