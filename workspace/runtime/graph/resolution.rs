//! Cross-version resolution: diffing a package's symbols across two versions so a
//! symbol present in both keeps one stable [`SymbolId`].

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use heart::{Symbol, SymbolId, Scored};

/// How an old symbol maps onto the next version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resolution {
	Stable(SymbolId),
	Moved { previous: SymbolId, next: Scored<SymbolId> },
	Removed(SymbolId),
	Added(SymbolId),
}

/// The pure diff of two package symbol sets — matching rules are separated
/// here so they are testable without a live graph.
///
/// Rules, in precedence order:
/// - same fully-qualified path in both versions → [`Resolution::Stable`],
///   carrying the *next* version's id (the identity going forward);
/// - otherwise, the best unclaimed next-version symbol with the same plain name
///   and kind → [`Resolution::Moved`], scored by path-segment similarity;
/// - otherwise the previous symbol is [`Resolution::Removed`];
/// - any next-version symbol left unclaimed is [`Resolution::Added`].
pub fn diff(previous: &[Symbol], next: &[Symbol]) -> Vec<Resolution> {
	let mut claimed: BTreeSet<SymbolId> = BTreeSet::new();
	let mut resolutions = Vec::with_capacity(previous.len() + next.len());

	for old in previous {
		// Exact path survival: the symbol simply persists.
		let survivor = next.iter().find(|candidate| {
			!claimed.contains(&candidate.id)
				&& candidate.name.fully_qualified == old.name.fully_qualified
		});
		if let Some(survivor) = survivor {
			claimed.insert(survivor.id);
			resolutions.push(Resolution::Stable(survivor.id));
			continue;
		}

		// A move/rename: same leaf name and kind, best-matching path.
		let moved = next
			.iter()
			.filter(|candidate| {
				!claimed.contains(&candidate.id)
					&& candidate.name.plain == old.name.plain
					&& candidate.kind == old.kind
			})
			.map(|candidate| {
				let similarity = path_similarity(
					&old.name.fully_qualified,
					&candidate.name.fully_qualified,
				);
				(candidate, similarity)
			})
			.max_by(|(_, a), (_, b)| a.total_cmp(b));
		match moved {
			Some((candidate, similarity)) => {
				claimed.insert(candidate.id);
				let score = heart::Score::try_new(similarity)
					.expect("segment jaccard over finite sets is finite");
				resolutions.push(Resolution::Moved {
					previous: old.id,
					next: Scored::new(candidate.id, score),
				});
			}
			None => resolutions.push(Resolution::Removed(old.id)),
		}
	}

	resolutions.extend(
		next.iter()
			.filter(|candidate| !claimed.contains(&candidate.id))
			.map(|candidate| Resolution::Added(candidate.id)),
	);
	resolutions
}

/// Jaccard similarity of two fully-qualified paths' segment sets — a cheap,
/// symmetric measure of how much of the surrounding module path survived a move.
fn path_similarity(previous: &str, next: &str) -> f32 {
	let segments = |path: &str| {
		path.split(['/', '.', ':'])
			.filter(|segment| !segment.is_empty())
			.map(str::to_owned)
			.collect::<BTreeSet<_>>()
	};
	let (previous, next) = (segments(previous), segments(next));
	let union = previous.union(&next).count();
	if union == 0 {
		return 0.0;
	}
	previous.intersection(&next).count() as f32 / union as f32
}

