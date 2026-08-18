//! The frozen mapping between the registry's runtime vocabulary and catalog
//! rows (INDEX-PLAN §8). This is the only place the two vocabularies meet;
//! every read and write in [`crate::index`] routes through these functions.
//!
//! # The state law
//!
//! `heart::ResolutionState` ↔ `versions.{parse_state, parse_phase, failure}`
//! (+ a `generations` row for the `Stored` terminal):
//!
//! | ResolutionState        | parse_state | parse_phase       | failure |
//! |------------------------|-------------|-------------------|---------|
//! | `Unindexed{needed}`    | `pending`   | `"needed"` / none | none    |
//! | `Progressing(phase)`   | `in_progress` | phase token     | none    |
//! | `Stored{hash}`         | `parsed`    | none              | none    |
//! | `Failed(f)`            | `failed`    | none              | json(f) |
//! | `DeadLettered(f)`      | `failed`    | `"dead_lettered"` | json(f) |
//!
//! `Stored`'s content hash is **not** a column: it is the `gen_stamp` of the
//! version's newest `generations` row (the design law "Stored{hash} → a
//! generation"). Reading `parsed` therefore requires the generation lookup.
//!
//! # The facet law
//!
//! [`SearchFacets`] round-trips through the `facets` row as
//! `(keywords = joined slugs, quality_ppm, extras = full serde JSON)` — the
//! `extras` column is authoritative for reads; the split-out columns exist for
//! catalog-side query/ranking convenience.

use crate::enums::ParseState;
use crate::store::lifecycle::VersionLifecycle;
use heart::{Phase, ResolutionState, content::ContentHash};

use crate::metadata::SearchFacets;

/// Why a catalog row could not be mapped back into runtime vocabulary.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("failure column held malformed JSON: {0}")]
    FailureJson(#[source] serde_json::Error),
    #[error("facets extras column held malformed JSON: {0}")]
    FacetsJson(#[source] serde_json::Error),
    #[error("versions row is `parsed` but has no generation row (stored hash lost)")]
    ParsedWithoutGeneration,
    #[error("versions row is `failed` but the failure column is empty")]
    FailureMissing,
    #[error("unknown parse_phase token {token:?} for state {state:?}")]
    UnknownPhase { state: ParseState, token: String },
    #[error("state `{0:?}` is unreachable from catalog columns")]
    UnreachableState(ParseState),
}

/// Compatibility alias for callers that named the mapping error by its old
/// `CatalogMapError` spelling (e.g. `crate::error::IndexError`).
pub use self::Error as CatalogMapError;

/// The `parse_phase` marker that carries `Unindexed {{ needed: true }}`.
const PHASE_NEEDED: &str = "needed";
/// The `parse_phase` marker distinguishing `DeadLettered` from `Failed`.
const PHASE_DEAD_LETTERED: &str = "dead_lettered";

/// The lifecycle column values for a [`ResolutionState`] (write direction).
/// `Stored`'s hash is returned separately: the caller records it as a
/// generation row, never as a column.
pub struct StateColumns {
    pub parse_state: ParseState,
    pub parse_phase: Option<String>,
    pub failure_json: Option<String>,
    /// `Some` exactly for `Stored`: the content hash to record as `gen_stamp`.
    pub stored_hash: Option<ContentHash>,
}

/// Lower a [`ResolutionState`] onto lifecycle columns (the state law above).
pub fn state_to_columns(state: &ResolutionState) -> Result<StateColumns, Error> {
    let (parse_state, parse_phase, failure_json, stored_hash) = match state {
        ResolutionState::Unindexed { needed } => (
            ParseState::Pending,
            needed.then(|| PHASE_NEEDED.to_owned()),
            None,
            None,
        ),
        ResolutionState::Progressing(phase) => (
            ParseState::InProgress,
            Some(phase_token(*phase).to_owned()),
            None,
            None,
        ),
        ResolutionState::Stored { hash } => (ParseState::Parsed, None, None, Some(*hash)),
        ResolutionState::Failed(failure) => (
            ParseState::Failed,
            None,
            Some(serde_json::to_string(failure).map_err(Error::FailureJson)?),
            None,
        ),
        ResolutionState::DeadLettered(failure) => (
            ParseState::Failed,
            Some(PHASE_DEAD_LETTERED.to_owned()),
            Some(serde_json::to_string(failure).map_err(Error::FailureJson)?),
            None,
        ),
    };
    Ok(StateColumns {
        parse_state,
        parse_phase,
        failure_json,
        stored_hash,
    })
}

/// Raise lifecycle columns (+ the generation lookup result for `parsed` rows)
/// back into a [`ResolutionState`].
pub fn state_from_columns(
    lifecycle: &VersionLifecycle,
    stored_hash: Option<ContentHash>,
) -> Result<ResolutionState, Error> {
    match lifecycle.parse_state {
        ParseState::Pending => Ok(ResolutionState::Unindexed {
            needed: lifecycle.parse_phase.as_deref() == Some(PHASE_NEEDED),
        }),
        ParseState::InProgress => {
            let token = lifecycle.parse_phase.as_deref().unwrap_or_default();
            let phase = phase_from_token(token).ok_or_else(|| Error::UnknownPhase {
                state: ParseState::InProgress,
                token: token.to_owned(),
            })?;
            Ok(ResolutionState::Progressing(phase))
        }
        ParseState::Parsed => stored_hash
            .map(|hash| ResolutionState::Stored { hash })
            .ok_or(Error::ParsedWithoutGeneration),
        ParseState::Failed => {
            let failure = lifecycle
                .failure
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(Error::FailureJson)?
                .ok_or(Error::FailureMissing)?;
            if lifecycle.parse_phase.as_deref() == Some(PHASE_DEAD_LETTERED) {
                Ok(ResolutionState::DeadLettered(failure))
            } else {
                Ok(ResolutionState::Failed(failure))
            }
        }
        ParseState::Skipped => Err(Error::UnreachableState(ParseState::Skipped)),
    }
}

/// The stable token for a pipeline [`Phase`] in `parse_phase`.
pub fn phase_token(phase: Phase) -> &'static str {
    match phase {
        Phase::Acquiring => "acquiring",
        Phase::Extracting => "extracting",
        Phase::Compiling => "compiling",
        Phase::Emitting => "emitting",
    }
}

fn phase_from_token(token: &str) -> Option<Phase> {
    match token {
        "acquiring" => Some(Phase::Acquiring),
        "extracting" => Some(Phase::Extracting),
        "compiling" => Some(Phase::Compiling),
        "emitting" => Some(Phase::Emitting),
        _ => None,
    }
}

/// Lower [`SearchFacets`] onto the facet row triple (the facet law above).
pub fn facets_to_row(
    facets: &SearchFacets,
) -> Result<(Option<String>, Option<i64>, Option<String>), Error> {
    let keywords = (!facets.keywords.is_empty()).then(|| {
        facets
            .keywords
            .iter()
            .map(smol_str::SmolStr::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    });
    let extras = serde_json::to_string(facets).map_err(Error::FacetsJson)?;
    Ok((keywords, Some(i64::from(facets.quality_ppm)), Some(extras)))
}

/// Raise a facet row back into [`SearchFacets`] via the authoritative
/// `extras` JSON. `None` extras ⇒ no facets recorded.
pub fn facets_from_extras(extras_json: Option<&str>) -> Result<Option<SearchFacets>, Error> {
    extras_json
        .map(serde_json::from_str)
        .transpose()
        .map_err(Error::FacetsJson)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(state: ResolutionState) {
        let columns = state_to_columns(&state).expect("lower");
        let lifecycle = VersionLifecycle {
            parse_state: columns.parse_state,
            parse_phase: columns.parse_phase,
            attempts: 0,
            failure: columns.failure_json,
        };
        let raised = state_from_columns(&lifecycle, columns.stored_hash).expect("raise");
        assert_eq!(raised, state);
    }

    #[test]
    fn every_state_round_trips() {
        roundtrip(ResolutionState::Unindexed { needed: true });
        roundtrip(ResolutionState::Unindexed { needed: false });
        roundtrip(ResolutionState::Progressing(Phase::Acquiring));
        roundtrip(ResolutionState::Progressing(Phase::Emitting));
        roundtrip(ResolutionState::Stored {
            hash: ContentHash::from_bytes([7u8; 32]),
        });
    }

    #[test]
    fn parsed_without_generation_is_a_typed_error() {
        let lifecycle = VersionLifecycle {
            parse_state: ParseState::Parsed,
            parse_phase: None,
            attempts: 0,
            failure: None,
        };
        assert!(matches!(
            state_from_columns(&lifecycle, None),
            Err(Error::ParsedWithoutGeneration)
        ));
    }
}
