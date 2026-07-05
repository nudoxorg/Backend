//! The search planner — the *only* place a [`SemanticGate`] is minted.

use runtime::vector::SemanticGate;

use crate::search::query::{Query, Search};

pub enum Plan {
	Precise,
	Semantic(SemanticGate),
}

pub struct SearchPlanner {}

impl SearchPlanner {
	pub fn new() -> Self { todo!("wire policy + quota handles") }

	pub fn plan(&self, request: &Search<'_>) -> Plan {
		match &request.query {
			Query::Literal(_) => Plan::Precise,
			Query::Abstract(_) => {
				todo!("check policy+quota; if allowed -> Plan::Semantic(SemanticGate::issue(...))")
			}
		}
	}
}

impl Default for SearchPlanner {
	fn default() -> Self { Self::new() }
}
