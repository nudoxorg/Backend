//! Conservative candidate discovery for semantic declaration continuity.
//!
//! This module does not mint, preserve, or select declaration identity. It
//! compares exact deleted and introduced rows from one *direct parent/child*
//! transition and returns candidate evidence for a later, root-bound history
//! change record. Every result remains unverified: a consumer must bind the
//! endpoints to the admitted parent and child generations and validate their
//! row membership before recording or acting on the evidence.
//!
//! Candidate discovery is deliberately narrower than the retired `ir-vcs`
//! continuity engine. It uses exact package/profile/kind gates, then surfaces
//! pairs through same-family, same-name, or same-core-shape anchors. Exact
//! stable-reference overlap may corroborate a surfaced pair, but can never
//! create a candidate by itself. Ordinal chunk identity, source order, and
//! proximity are never evidence.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use crate::ir::{
    CorePayloadHash, DeclarationFamilyId, DeclarationIdentity, DeclarationKey, EntityKind,
    StableRef,
};
use crate::vocabulary::LanguageProfile;

/// One exact endpoint row's producer-visible declaration key and optional
/// structural/body evidence.
///
/// `identity` and `key` must come from the same row admitted by the caller's
/// semantic reader. This type intentionally does not claim to prove that
/// association. The matcher emits candidates only; durable history must
/// independently bind each endpoint to its exact generation row.
#[derive(Clone, Copy, Debug)]
pub struct LineageObservation<'bytes> {
    /// Exact current-generation declaration endpoint.
    pub identity: DeclarationIdentity,
    /// Validated key cells: package lineage, source path, kind, and exact name.
    pub key: DeclarationKey<'bytes>,
    /// Closed compiler profile. Different profiles never match.
    pub profile: LanguageProfile,
    /// Exact current-generation parent identity, when represented locally.
    pub parent: Option<DeclarationIdentity>,
    /// Current core payload digest. Equality is a candidate anchor only.
    pub core_shape: Option<CorePayloadHash>,
    /// Optional canonical set of exact stable references from the declaration
    /// body. It is corroborating evidence, never an anchor.
    pub body_references: CanonicalBodyReferences<'bytes>,
}

impl<'bytes> LineageObservation<'bytes> {
    /// Builds a borrowed observation from row facts already admitted by the
    /// caller. This constructor does not validate that `identity` was minted
    /// from `key`; the reader/history boundary remains responsible for that.
    #[must_use]
    pub const fn new(
        identity: DeclarationIdentity,
        key: DeclarationKey<'bytes>,
        profile: LanguageProfile,
        parent: Option<DeclarationIdentity>,
        core_shape: Option<CorePayloadHash>,
        body_references: CanonicalBodyReferences<'bytes>,
    ) -> Self {
        Self {
            identity,
            key,
            profile,
            parent,
            core_shape,
            body_references,
        }
    }
}

/// Borrowed body-reference set in strict canonical order.
///
/// `StableRef` identity changes across a callee rename, so this exact overlap
/// signal can undercount co-renames. That is an intentional false-negative
/// direction: it can corroborate a surfaced candidate but cannot create or
/// authorize one.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalBodyReferences<'refs>(&'refs [StableRef]);

/// Body-reference input was not strictly sorted and unique.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BodyReferenceOrderError {
    /// First index whose value is not greater than its predecessor.
    pub index: usize,
}

impl<'refs> CanonicalBodyReferences<'refs> {
    /// Validates a borrowed, strictly sorted, duplicate-free reference set.
    pub fn new(references: &'refs [StableRef]) -> Result<Self, BodyReferenceOrderError> {
        for (index, pair) in references.windows(2).enumerate() {
            if pair[0] >= pair[1] {
                return Err(BodyReferenceOrderError { index: index + 1 });
            }
        }
        Ok(Self(references))
    }

    /// Empty canonical set, used when body evidence is unavailable.
    #[must_use]
    pub const fn empty() -> Self {
        Self(&[])
    }

    /// Number of distinct stable references.
    #[must_use]
    pub const fn len(self) -> usize {
        self.0.len()
    }

    /// Whether no body references were admitted.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0.is_empty()
    }
}

/// The candidate describes a name change, a relocation, or both.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineageEditKindV1 {
    /// Exact declaration name changed while path and parent stayed equal.
    Rename,
    /// Path or parent changed while the exact declaration name stayed equal.
    Relocation,
    /// Name and at least one of path or parent changed.
    RenameAndRelocation,
}

/// Why the pair was surfaced by the bounded candidate index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineageCandidateAnchorsV1 {
    /// The 128-bit declaration-family identities are equal.
    pub same_family: bool,
    /// The exact core payload hashes are equal.
    pub same_core_shape: bool,
    /// Exact declaration name bytes are equal.
    pub same_name: bool,
}

/// Exact reference-set overlap, when both bodies have enough references to
/// make Jaccard similarity informative.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BodyReferenceOverlapV1 {
    /// Number of exact stable references present on both sides.
    pub intersection: usize,
    /// Number of exact stable references present on either side.
    pub union: usize,
    /// Integer-only threshold bucket used by the retired matcher.
    pub band: BodyReferenceOverlapBandV1,
}

/// Coarse overlap band. These are evidence labels, not probabilities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BodyReferenceOverlapBandV1 {
    /// Jaccard overlap is at least 80 percent.
    Near,
    /// Jaccard overlap is at least 50 percent and below 80 percent.
    Mid,
}

/// Candidate-pair multiplicity in the complete surfaced candidate graph.
///
/// `NoCompetingSurfacePair` means only that the bounded, exact-anchor search
/// found no competing pair. It does not mean continuity is confirmed, nor
/// prove that an omitted signal could not have found another declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineageCandidateAmbiguityV1 {
    /// No other surfaced pair shares either endpoint.
    NoCompetingSurfacePair,
    /// One or both endpoints have more than one surfaced partner. Counts
    /// include this candidate.
    CompetingSurfacePairs {
        /// Number of introduced candidates for this deleted endpoint.
        introduced_for_deleted: u32,
        /// Number of deleted candidates for this introduced endpoint.
        deleted_for_introduced: u32,
    },
}

/// Unverified candidate evidence for one removed and one introduced row.
///
/// This type has no `Verified`, `Confirmed`, or identity-substitution state.
/// Even a non-ambiguous candidate remains a hypothesis for a future durable
/// change record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineageCandidateEvidenceV1 {
    /// Exact endpoint from the direct parent generation.
    pub deleted: DeclarationIdentity,
    /// Exact endpoint from the direct child generation.
    pub introduced: DeclarationIdentity,
    /// Descriptive edit shape; does not select a durable identity.
    pub edit: LineageEditKindV1,
    /// Locator changes observed between the two admitted feature records.
    pub path_changed: bool,
    /// Parent endpoint changed between the two admitted feature records.
    pub parent_changed: bool,
    /// Exact indexing anchors that surfaced this pair.
    pub anchors: LineageCandidateAnchorsV1,
    /// Body overlap corroboration, if both body sets clear the two-reference
    /// floor. It does not affect ambiguity or candidate selection.
    pub body_overlap: Option<BodyReferenceOverlapV1>,
    /// Ambiguity among all surfaced pairs in this complete search.
    pub ambiguity: LineageCandidateAmbiguityV1,
}

/// Bounded work allowance for one candidate-discovery pass.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineageMatcherLimitsV1 {
    /// Maximum total endpoint observations scanned.
    pub max_observations: usize,
    /// Maximum surfaced pairs scored. Exceeding this fails the whole pass;
    /// no truncated result is returned as if it were complete.
    pub max_candidate_pairs: usize,
    /// Maximum stable-reference comparisons used for body overlap.
    pub max_body_reference_steps: usize,
}

impl Default for LineageMatcherLimitsV1 {
    fn default() -> Self {
        Self {
            max_observations: 1_000_000,
            max_candidate_pairs: 250_000,
            max_body_reference_steps: 4_000_000,
        }
    }
}

/// Observable algorithm work for one complete match pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LineageMatcherCostV1 {
    /// Number of deleted endpoint records read.
    pub deleted_observations: usize,
    /// Number of introduced endpoint records read.
    pub introduced_observations: usize,
    /// Number of B-tree anchor entries built for deleted endpoints.
    pub index_entries: usize,
    /// Number of anchor lookups performed for introduced endpoints.
    pub anchor_probes: usize,
    /// Number of distinct candidate pairs scored after exact context gates.
    pub candidate_pairs_scored: usize,
    /// Name bytes hashed for candidate index keys.
    pub name_bytes_hashed: usize,
    /// Two-pointer stable-reference comparisons performed for corroboration.
    pub body_reference_steps: usize,
}

/// Complete candidate-only result and its work counters.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineageCandidateSetV1 {
    /// Every surfaced pair, in `(deleted identity, introduced identity)` order.
    pub candidates: Vec<LineageCandidateEvidenceV1>,
    /// Work performed for this complete pass.
    pub cost: LineageMatcherCostV1,
}

/// Which endpoint list violated strict canonical identity order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineageEndpointSideV1 {
    /// The deleted endpoint list.
    Deleted,
    /// The introduced endpoint list.
    Introduced,
}

/// Why no complete candidate set could be produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineageMatcherFailureKindV1 {
    /// The caller supplied more rows than the configured scan bound.
    ObservationLimitExceeded {
        /// Combined row count observed before any index allocation.
        observed: usize,
        /// Configured maximum combined row count.
        limit: usize,
    },
    /// An endpoint list is not strictly sorted by exact declaration identity.
    EndpointsNotStrictlySorted {
        /// Which endpoint list is malformed.
        side: LineageEndpointSideV1,
        /// First offending index.
        index: usize,
    },
    /// The same endpoint identity appeared as both deleted and introduced.
    EndpointAlsoOnOppositeSide {
        /// Exact identity that cannot be both removed and introduced.
        identity: DeclarationIdentity,
    },
    /// The output graph exceeded its configured pair bound. No partial graph
    /// is returned, so ambiguity is never hidden by truncation.
    CandidatePairLimitExceeded {
        /// Configured maximum pair count.
        limit: usize,
    },
    /// Body corroboration exceeded its reference-step budget. No partial graph
    /// is returned.
    BodyReferenceLimitExceeded {
        /// Configured maximum reference-comparison count.
        limit: usize,
    },
}

/// Failed complete pass with counters retained for diagnosis.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineageMatcherFailureV1 {
    /// Exact reason the pass stopped.
    pub kind: LineageMatcherFailureKindV1,
    /// Work performed before it stopped.
    pub cost: LineageMatcherCostV1,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CandidateIndexKey {
    Family(DeclarationFamilyId),
    Shape(EntityKind, CorePayloadHash),
    Name(EntityKind, [u8; 32]),
}

/// Discovers conservative candidate evidence for deleted and introduced rows.
///
/// Both input slices must be the complete deleted/introduced sets for one
/// direct parent/child comparison, sorted strictly by exact identity. The
/// function validates ordering and endpoint disjointness, fails closed on
/// budget exhaustion, and never picks one edge from an ambiguous group.
pub fn match_lineage_candidates_v1(
    deleted: &[LineageObservation<'_>],
    introduced: &[LineageObservation<'_>],
    limits: LineageMatcherLimitsV1,
) -> Result<LineageCandidateSetV1, LineageMatcherFailureV1> {
    let mut cost = LineageMatcherCostV1 {
        deleted_observations: deleted.len(),
        introduced_observations: introduced.len(),
        ..LineageMatcherCostV1::default()
    };
    let observed = deleted.len().saturating_add(introduced.len());
    if observed > limits.max_observations {
        return Err(failure(
            LineageMatcherFailureKindV1::ObservationLimitExceeded {
                observed,
                limit: limits.max_observations,
            },
            cost,
        ));
    }
    validate_order(deleted, LineageEndpointSideV1::Deleted, cost)?;
    validate_order(introduced, LineageEndpointSideV1::Introduced, cost)?;
    validate_disjoint(deleted, introduced, cost)?;

    let mut by_anchor: BTreeMap<CandidateIndexKey, Vec<usize>> = BTreeMap::new();
    for (index, observation) in deleted.iter().enumerate() {
        insert_anchor(
            &mut by_anchor,
            CandidateIndexKey::Family(observation.identity.family),
            index,
            &mut cost,
        );
        if let Some(shape) = observation.core_shape {
            insert_anchor(
                &mut by_anchor,
                CandidateIndexKey::Shape(observation.key.kind, shape),
                index,
                &mut cost,
            );
        }
        let name_key = name_anchor(observation.key.kind, observation.key.name);
        cost.name_bytes_hashed = cost
            .name_bytes_hashed
            .saturating_add(observation.key.name.len());
        insert_anchor(&mut by_anchor, name_key, index, &mut cost);
    }

    let mut pairs = BTreeSet::new();
    for (introduced_index, after) in introduced.iter().enumerate() {
        let mut possible_deleted = BTreeSet::new();
        probe_anchor(
            &by_anchor,
            CandidateIndexKey::Family(after.identity.family),
            &mut possible_deleted,
            &mut cost,
        );
        if let Some(shape) = after.core_shape {
            probe_anchor(
                &by_anchor,
                CandidateIndexKey::Shape(after.key.kind, shape),
                &mut possible_deleted,
                &mut cost,
            );
        }
        let name_key = name_anchor(after.key.kind, after.key.name);
        cost.name_bytes_hashed = cost.name_bytes_hashed.saturating_add(after.key.name.len());
        probe_anchor(&by_anchor, name_key, &mut possible_deleted, &mut cost);

        for deleted_index in possible_deleted {
            let before = &deleted[deleted_index];
            if !same_candidate_scope(before, after) {
                continue;
            }
            let same_family = before.identity.family == after.identity.family;
            let same_core_shape = matches!(
                (before.core_shape, after.core_shape),
                (Some(left), Some(right)) if left == right
            );
            let same_name = before.key.name == after.key.name;
            // The family anchor is useful for future structural-evolution
            // records, but this API emits only a rename or relocation claim.
            let path_changed = before.key.path != after.key.path;
            let name_changed = !same_name;
            let parent_changed = before.parent != after.parent;
            if !path_changed && !name_changed && !parent_changed {
                continue;
            }
            debug_assert!(same_family || same_core_shape || same_name);
            if pairs.len() >= limits.max_candidate_pairs {
                return Err(failure(
                    LineageMatcherFailureKindV1::CandidatePairLimitExceeded {
                        limit: limits.max_candidate_pairs,
                    },
                    cost,
                ));
            }
            pairs.insert((deleted_index, introduced_index));
        }
    }

    let mut deleted_degree = alloc::vec![0usize; deleted.len()];
    let mut introduced_degree = alloc::vec![0usize; introduced.len()];
    for &(deleted_index, introduced_index) in &pairs {
        deleted_degree[deleted_index] += 1;
        introduced_degree[introduced_index] += 1;
    }
    cost.candidate_pairs_scored = pairs.len();

    let mut candidates = Vec::new();
    candidates.try_reserve_exact(pairs.len()).map_err(|_| {
        failure(
            LineageMatcherFailureKindV1::CandidatePairLimitExceeded {
                limit: limits.max_candidate_pairs,
            },
            cost,
        )
    })?;
    for (deleted_index, introduced_index) in pairs {
        let before = &deleted[deleted_index];
        let after = &introduced[introduced_index];
        let same_name = before.key.name == after.key.name;
        let path_changed = before.key.path != after.key.path;
        let parent_changed = before.parent != after.parent;
        let has_relocation = path_changed || parent_changed;
        let edit = match (same_name, has_relocation) {
            (false, false) => LineageEditKindV1::Rename,
            (true, true) => LineageEditKindV1::Relocation,
            (false, true) => LineageEditKindV1::RenameAndRelocation,
            (true, false) => continue,
        };
        let body_overlap = reference_overlap(
            before.body_references,
            after.body_references,
            &mut cost,
            limits.max_body_reference_steps,
        )
        .map_err(|()| {
            failure(
                LineageMatcherFailureKindV1::BodyReferenceLimitExceeded {
                    limit: limits.max_body_reference_steps,
                },
                cost,
            )
        })?;
        let deleted_count = deleted_degree[deleted_index];
        let introduced_count = introduced_degree[introduced_index];
        let ambiguity = if deleted_count == 1 && introduced_count == 1 {
            LineageCandidateAmbiguityV1::NoCompetingSurfacePair
        } else {
            LineageCandidateAmbiguityV1::CompetingSurfacePairs {
                introduced_for_deleted: u32::try_from(deleted_count).unwrap_or(u32::MAX),
                deleted_for_introduced: u32::try_from(introduced_count).unwrap_or(u32::MAX),
            }
        };
        candidates.push(LineageCandidateEvidenceV1 {
            deleted: before.identity,
            introduced: after.identity,
            edit,
            path_changed,
            parent_changed,
            anchors: LineageCandidateAnchorsV1 {
                same_family: before.identity.family == after.identity.family,
                same_core_shape: matches!(
                    (before.core_shape, after.core_shape),
                    (Some(left), Some(right)) if left == right
                ),
                same_name,
            },
            body_overlap,
            ambiguity,
        });
    }
    cost.candidate_pairs_scored = candidates.len();
    Ok(LineageCandidateSetV1 { candidates, cost })
}

fn validate_order(
    observations: &[LineageObservation<'_>],
    side: LineageEndpointSideV1,
    cost: LineageMatcherCostV1,
) -> Result<(), LineageMatcherFailureV1> {
    for (index, pair) in observations.windows(2).enumerate() {
        if pair[0].identity >= pair[1].identity {
            return Err(failure(
                LineageMatcherFailureKindV1::EndpointsNotStrictlySorted {
                    side,
                    index: index + 1,
                },
                cost,
            ));
        }
    }
    Ok(())
}

fn validate_disjoint(
    deleted: &[LineageObservation<'_>],
    introduced: &[LineageObservation<'_>],
    cost: LineageMatcherCostV1,
) -> Result<(), LineageMatcherFailureV1> {
    let (mut left, mut right) = (0, 0);
    while left < deleted.len() && right < introduced.len() {
        match deleted[left].identity.cmp(&introduced[right].identity) {
            core::cmp::Ordering::Less => left += 1,
            core::cmp::Ordering::Greater => right += 1,
            core::cmp::Ordering::Equal => {
                return Err(failure(
                    LineageMatcherFailureKindV1::EndpointAlsoOnOppositeSide {
                        identity: deleted[left].identity,
                    },
                    cost,
                ));
            }
        }
    }
    Ok(())
}

fn insert_anchor(
    index: &mut BTreeMap<CandidateIndexKey, Vec<usize>>,
    anchor: CandidateIndexKey,
    row: usize,
    cost: &mut LineageMatcherCostV1,
) {
    index.entry(anchor).or_default().push(row);
    cost.index_entries = cost.index_entries.saturating_add(1);
}

fn probe_anchor(
    index: &BTreeMap<CandidateIndexKey, Vec<usize>>,
    anchor: CandidateIndexKey,
    matches: &mut BTreeSet<usize>,
    cost: &mut LineageMatcherCostV1,
) {
    cost.anchor_probes = cost.anchor_probes.saturating_add(1);
    if let Some(rows) = index.get(&anchor) {
        matches.extend(rows.iter().copied());
    }
}

fn name_anchor(kind: EntityKind, name: &[u8]) -> CandidateIndexKey {
    let mut hasher = blake3::Hasher::new_derive_key("backend.semantic.lineage-name-anchor.v1");
    hasher.update(name);
    CandidateIndexKey::Name(kind, *hasher.finalize().as_bytes())
}

fn same_candidate_scope(before: &LineageObservation<'_>, after: &LineageObservation<'_>) -> bool {
    before.profile == after.profile
        && before.key.kind == after.key.kind
        && before.key.lineage.ecosystem == after.key.lineage.ecosystem
        && before.key.lineage.name == after.key.lineage.name
}

fn reference_overlap(
    before: CanonicalBodyReferences<'_>,
    after: CanonicalBodyReferences<'_>,
    cost: &mut LineageMatcherCostV1,
    step_limit: usize,
) -> Result<Option<BodyReferenceOverlapV1>, ()> {
    const MIN_BODY_REFERENCES: usize = 2;
    if before.len() < MIN_BODY_REFERENCES || after.len() < MIN_BODY_REFERENCES {
        return Ok(None);
    }
    let (mut left, mut right, mut intersection) = (0, 0, 0usize);
    while left < before.0.len() && right < after.0.len() {
        if cost.body_reference_steps >= step_limit {
            return Err(());
        }
        cost.body_reference_steps += 1;
        match before.0[left].cmp(&after.0[right]) {
            core::cmp::Ordering::Less => left += 1,
            core::cmp::Ordering::Greater => right += 1,
            core::cmp::Ordering::Equal => {
                intersection += 1;
                left += 1;
                right += 1;
            }
        }
    }
    let union = before.len() + after.len() - intersection;
    if intersection.saturating_mul(100) >= union.saturating_mul(80) {
        Ok(Some(BodyReferenceOverlapV1 {
            intersection,
            union,
            band: BodyReferenceOverlapBandV1::Near,
        }))
    } else if intersection.saturating_mul(100) >= union.saturating_mul(50) {
        Ok(Some(BodyReferenceOverlapV1 {
            intersection,
            union,
            band: BodyReferenceOverlapBandV1::Mid,
        }))
    } else {
        Ok(None)
    }
}

fn failure(
    kind: LineageMatcherFailureKindV1,
    cost: LineageMatcherCostV1,
) -> LineageMatcherFailureV1 {
    LineageMatcherFailureV1 { kind, cost }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{
        DeclarationFamilyId, DeclarationIdentity, DeclarationKey, EntityKind, PackageLineage,
        StableRef, VariantFingerprint,
    };
    use crate::ir_vocabulary::ExternalFragmentId;
    use crate::vocabulary::{LanguageProfile, PythonVersion, RustEdition};

    const RUST: LanguageProfile = LanguageProfile::Rust(RustEdition::Rust2024);
    const PYTHON: LanguageProfile = LanguageProfile::Python(PythonVersion::Python314);

    fn identity(family: u8, variant: u8) -> DeclarationIdentity {
        DeclarationIdentity {
            family: DeclarationFamilyId::from_raw([family; 16]),
            variant: VariantFingerprint::from_raw([variant; 16]),
        }
    }

    fn observation<'a>(
        family: u8,
        variant: u8,
        path: &'a str,
        name: &'a [u8],
        profile: LanguageProfile,
        parent: Option<DeclarationIdentity>,
        shape: Option<u8>,
        refs: &'a [StableRef],
    ) -> LineageObservation<'a> {
        let lineage = PackageLineage::new("cargo", "demo").expect("valid package lineage");
        let key = DeclarationKey::new(lineage, path, EntityKind::Function, name)
            .expect("valid declaration key");
        LineageObservation::new(
            identity(family, variant),
            key,
            profile,
            parent,
            shape.map(|seed| CorePayloadHash::from_raw([seed; 16])),
            CanonicalBodyReferences::new(refs).expect("sorted unique body refs"),
        )
    }

    fn ref_id(seed: u8) -> StableRef {
        StableRef {
            fragment: ExternalFragmentId::from_canonical_bytes(&[seed]),
            declaration: identity(seed, seed.wrapping_add(1)),
        }
    }

    fn sorted_refs<const N: usize>(seeds: [u8; N]) -> [StableRef; N] {
        let mut references = seeds.map(ref_id);
        references.sort_unstable();
        references
    }

    fn limits() -> LineageMatcherLimitsV1 {
        LineageMatcherLimitsV1 {
            max_observations: 64,
            max_candidate_pairs: 64,
            max_body_reference_steps: 4096,
        }
    }

    fn sort_by_identity(observations: &mut [LineageObservation<'_>]) {
        observations.sort_by_key(|observation| observation.identity);
    }

    #[test]
    fn exact_shape_surfaces_rename_without_selecting_identity() {
        let deleted = [observation(
            1,
            1,
            "src/lib.rs",
            b"decode",
            RUST,
            None,
            Some(7),
            &[],
        )];
        let introduced = [observation(
            2,
            2,
            "src/lib.rs",
            b"parse",
            RUST,
            None,
            Some(7),
            &[],
        )];
        let result = match_lineage_candidates_v1(&deleted, &introduced, limits()).unwrap();

        assert_eq!(result.candidates.len(), 1);
        let candidate = result.candidates[0];
        assert_eq!(candidate.deleted, identity(1, 1));
        assert_eq!(candidate.introduced, identity(2, 2));
        assert_eq!(candidate.edit, LineageEditKindV1::Rename);
        assert!(candidate.anchors.same_core_shape);
        assert!(!candidate.anchors.same_name);
        assert_eq!(
            candidate.ambiguity,
            LineageCandidateAmbiguityV1::NoCompetingSurfacePair
        );
        assert_eq!(result.cost.candidate_pairs_scored, 1);
    }

    #[test]
    fn exact_name_surfaces_path_and_parent_relocation() {
        let parent_before = identity(20, 1);
        let parent_after = identity(21, 1);
        let deleted = [observation(
            1,
            1,
            "src/old.rs",
            b"decode",
            RUST,
            Some(parent_before),
            Some(7),
            &[],
        )];
        let introduced = [observation(
            2,
            2,
            "src/new.rs",
            b"decode",
            RUST,
            Some(parent_after),
            Some(8),
            &[],
        )];
        let result = match_lineage_candidates_v1(&deleted, &introduced, limits()).unwrap();

        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].edit, LineageEditKindV1::Relocation);
        assert!(result.candidates[0].path_changed);
        assert!(result.candidates[0].parent_changed);
        assert!(result.candidates[0].anchors.same_name);
        assert!(!result.candidates[0].anchors.same_core_shape);
    }

    #[test]
    fn split_is_reported_as_competing_candidates_for_both_children() {
        let deleted = [observation(
            1,
            1,
            "src/lib.rs",
            b"decode",
            RUST,
            None,
            Some(7),
            &[],
        )];
        let mut introduced = [
            observation(
                2,
                2,
                "src/lib.rs",
                b"decode_header",
                RUST,
                None,
                Some(7),
                &[],
            ),
            observation(3, 3, "src/lib.rs", b"decode_body", RUST, None, Some(7), &[]),
        ];
        sort_by_identity(&mut introduced);
        let result = match_lineage_candidates_v1(&deleted, &introduced, limits()).unwrap();

        assert_eq!(result.candidates.len(), 2);
        for candidate in result.candidates {
            assert_eq!(candidate.edit, LineageEditKindV1::Rename);
            assert_eq!(
                candidate.ambiguity,
                LineageCandidateAmbiguityV1::CompetingSurfacePairs {
                    introduced_for_deleted: 2,
                    deleted_for_introduced: 1,
                }
            );
        }
    }

    #[test]
    fn duplicate_shapes_and_near_duplicate_bodies_remain_ambiguous() {
        let common = sorted_refs([1, 2, 3]);
        let before_a = sorted_refs([1, 2, 4]);
        let before_b = sorted_refs([1, 2, 5]);
        let mut deleted = [
            observation(
                1,
                1,
                "src/lib.rs",
                b"decode_a",
                RUST,
                None,
                Some(7),
                &before_a,
            ),
            observation(
                2,
                2,
                "src/lib.rs",
                b"decode_b",
                RUST,
                None,
                Some(7),
                &before_b,
            ),
        ];
        sort_by_identity(&mut deleted);
        let introduced = [observation(
            3,
            3,
            "src/lib.rs",
            b"parse",
            RUST,
            None,
            Some(7),
            &common,
        )];
        let result = match_lineage_candidates_v1(&deleted, &introduced, limits()).unwrap();

        assert_eq!(result.candidates.len(), 2);
        assert!(result.candidates.iter().all(|candidate| matches!(
            candidate.ambiguity,
            LineageCandidateAmbiguityV1::CompetingSurfacePairs {
                introduced_for_deleted: 1,
                deleted_for_introduced: 2,
            }
        )));
        assert!(result.candidates.iter().all(|candidate| matches!(
            candidate.body_overlap,
            Some(BodyReferenceOverlapV1 {
                intersection: 2,
                union: 4,
                band: BodyReferenceOverlapBandV1::Mid,
            })
        )));
    }

    #[test]
    fn immediate_readd_after_deletion_has_no_old_endpoint_to_match() {
        let introduced = [observation(
            2,
            2,
            "src/lib.rs",
            b"decode",
            RUST,
            None,
            Some(7),
            &[],
        )];
        // This is the direct transition from an empty parent generation to a
        // re-added child. The deleted endpoint from two generations ago is not
        // passed in, so the matcher cannot invent resurrection ancestry.
        let result = match_lineage_candidates_v1(&[], &introduced, limits()).unwrap();
        assert!(result.candidates.is_empty());
    }

    #[test]
    fn cross_language_equal_bytes_do_not_cross_the_profile_gate() {
        let deleted = [observation(
            1,
            1,
            "src/lib.rs",
            b"decode",
            RUST,
            None,
            Some(7),
            &[],
        )];
        let introduced = [observation(
            2,
            2,
            "src/lib.rs",
            b"decode",
            PYTHON,
            None,
            Some(7),
            &[],
        )];
        let result = match_lineage_candidates_v1(&deleted, &introduced, limits()).unwrap();
        assert!(result.candidates.is_empty());
    }

    #[test]
    fn rename_and_move_is_explicit_and_bounded_work_is_reported() {
        let deleted = [observation(
            1,
            1,
            "src/old.rs",
            b"decode",
            RUST,
            None,
            Some(7),
            &[],
        )];
        let introduced = [observation(
            2,
            2,
            "src/new.rs",
            b"parse",
            RUST,
            None,
            Some(7),
            &[],
        )];
        let result = match_lineage_candidates_v1(&deleted, &introduced, limits()).unwrap();
        assert_eq!(
            result.candidates[0].edit,
            LineageEditKindV1::RenameAndRelocation
        );
        assert_eq!(result.cost.deleted_observations, 1);
        assert_eq!(result.cost.introduced_observations, 1);
        assert_eq!(result.cost.index_entries, 3);
        assert_eq!(result.cost.anchor_probes, 3);
        assert_eq!(
            result.cost.name_bytes_hashed,
            b"decode".len() + b"parse".len()
        );
        assert_eq!(result.cost.candidate_pairs_scored, 1);
    }

    #[test]
    fn body_overlap_cannot_create_a_candidate_without_an_exact_anchor() {
        let refs = sorted_refs([1, 2, 3]);
        let deleted = [observation(
            1,
            1,
            "src/lib.rs",
            b"decode",
            RUST,
            None,
            Some(7),
            &refs,
        )];
        let introduced = [observation(
            2,
            2,
            "src/lib.rs",
            b"parse",
            RUST,
            None,
            Some(8),
            &refs,
        )];
        let result = match_lineage_candidates_v1(&deleted, &introduced, limits()).unwrap();
        assert!(result.candidates.is_empty());
        assert_eq!(result.cost.candidate_pairs_scored, 0);
        assert_eq!(result.cost.body_reference_steps, 0);
    }

    #[test]
    fn malformed_endpoint_order_and_pair_truncation_fail_closed() {
        let mut deleted = [
            observation(2, 2, "src/lib.rs", b"decode", RUST, None, Some(7), &[]),
            observation(1, 1, "src/lib.rs", b"decode", RUST, None, Some(7), &[]),
        ];
        let error = match_lineage_candidates_v1(&deleted, &[], limits()).unwrap_err();
        assert!(matches!(
            error.kind,
            LineageMatcherFailureKindV1::EndpointsNotStrictlySorted {
                side: LineageEndpointSideV1::Deleted,
                index: 1,
            }
        ));

        sort_by_identity(&mut deleted);
        let introduced = [observation(
            3,
            3,
            "src/lib.rs",
            b"parse",
            RUST,
            None,
            Some(7),
            &[],
        )];
        let tight = LineageMatcherLimitsV1 {
            max_candidate_pairs: 0,
            ..limits()
        };
        let error = match_lineage_candidates_v1(&deleted, &introduced, tight).unwrap_err();
        assert!(matches!(
            error.kind,
            LineageMatcherFailureKindV1::CandidatePairLimitExceeded { limit: 0 }
        ));
    }
}
