//! Witnessed read observations and exact dependency manifests.

use super::selector::{Read, ScopedRead};
use crate::canonical::canonical_scoped_manifest;
use crate::{
    ExactReadManifestVersion, FacetVersion, ReadManifestVersion, ScopedReadManifestVersion,
    SemanticError,
};
use backend_version::{AuthorizedCompleteCoverage, CoverageWitness, ObjectVersion};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, OnceLock};

type PolarityKey = (crate::FacetKind, Vec<u8>);
type CompactPolarityGroup = (bool, bool, BTreeSet<u64>, BTreeSet<u64>);
type ScopedPolarityGroup<'a> = Vec<(&'a ScopedRead, Vec<u8>, Option<Vec<u8>>, bool)>;

fn compact_polarity_conflict(reads: &[Read]) -> bool {
    let mut groups: BTreeMap<PolarityKey, CompactPolarityGroup> = BTreeMap::new();
    for read in reads {
        let entry = groups
            .entry((read.facet(), read.scope_root().as_bytes().to_vec()))
            .or_default();
        let (positive_exact, negative_exact, positive_ranges, negative_ranges) = entry;
        if read.is_negative() {
            if read.range_token() == 0 {
                if *positive_exact || !positive_ranges.is_empty() {
                    return true;
                }
                *negative_exact = true;
            } else {
                if *positive_exact || positive_ranges.contains(&read.range_token()) {
                    return true;
                }
                negative_ranges.insert(read.range_token());
            }
        } else if read.range_token() == 0 {
            if *negative_exact || !negative_ranges.is_empty() {
                return true;
            }
            *positive_exact = true;
        } else {
            if *negative_exact || negative_ranges.contains(&read.range_token()) {
                return true;
            }
            positive_ranges.insert(read.range_token());
        }
    }
    false
}

fn scoped_polarity_conflict(reads: &[ScopedRead]) -> bool {
    let mut groups: BTreeMap<PolarityKey, ScopedPolarityGroup<'_>> = BTreeMap::new();
    for read in reads {
        let (start, end, exact) = read.selector().interval();
        groups
            .entry((read.facet(), read.scope_root().as_bytes().to_vec()))
            .or_default()
            .push((read, start, end, exact));
    }
    groups.values_mut().any(|group| {
        group.sort_by(|left, right| {
            left.1
                .cmp(&right.1)
                .then_with(|| left.3.cmp(&right.3).reverse())
        });
        let mut positive_points = BTreeSet::<Vec<u8>>::new();
        let mut negative_points = BTreeSet::<Vec<u8>>::new();
        let mut positive_end: Option<Option<Vec<u8>>> = None;
        let mut negative_end: Option<Option<Vec<u8>>> = None;
        for (read, start, end, exact) in group.iter() {
            let (opposite_points, opposite_end) = if read.is_negative() {
                (&positive_points, &positive_end)
            } else {
                (&negative_points, &negative_end)
            };
            let overlaps = if *exact {
                opposite_points.contains(start)
                    || opposite_end.as_ref().is_some_and(|end| {
                        end.as_ref()
                            .is_none_or(|end| end.as_slice() > start.as_slice())
                    })
            } else {
                opposite_end.as_ref().is_some_and(|end| {
                    end.as_ref()
                        .is_none_or(|end| end.as_slice() > start.as_slice())
                }) || opposite_points
                    .range(start.clone()..)
                    .next()
                    .is_some_and(|point| {
                        end.as_ref()
                            .is_none_or(|end| point.as_slice() < end.as_slice())
                    })
            };
            if overlaps {
                return true;
            }
            if *exact {
                if read.is_negative() {
                    negative_points.insert(start.clone());
                } else {
                    positive_points.insert(start.clone());
                }
            } else {
                let slot = if read.is_negative() {
                    &mut negative_end
                } else {
                    &mut positive_end
                };
                *slot = Some(match (slot.take().flatten(), end.clone()) {
                    (None, end) => end,
                    (Some(previous), Some(end)) => Some(previous.max(end)),
                    (Some(_), None) => None,
                });
            }
        }
        false
    })
}

/// An exact scoped read paired with its admitted value version and witness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedReadObservation {
    read: ScopedRead,
    version: Option<FacetVersion>,
    coverage: CoverageWitness,
}

impl ScopedReadObservation {
    /// Admits any polarity-compatible witnessed read, including incomplete
    /// positive observations. A negative observation may omit its version but
    /// remains non-authoritative until its manifest supplies completeness.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] for a negative read
    /// paired with a value version, or [`SemanticError::ScopeMismatch`] when
    /// the witness names another scope.
    pub fn witnessed(
        read: ScopedRead,
        version: Option<FacetVersion>,
        coverage: CoverageWitness,
    ) -> Result<Self, SemanticError> {
        if read.is_negative() && version.is_some() {
            return Err(SemanticError::InvalidCoverageState);
        }
        if read.scope_root() != coverage.scope_root() {
            return Err(SemanticError::ScopeMismatch);
        }
        Ok(Self {
            read,
            version,
            coverage,
        })
    }

    /// Constructs an admitted positive observation.
    /// # Errors
    ///
    /// Returns [`SemanticError::PositiveReadRequired`] when the selector is
    /// negative, or [`SemanticError::ScopeMismatch`] when the witness names
    /// another authority scope.
    pub fn positive(
        read: ScopedRead,
        version: FacetVersion,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        if read.is_negative() {
            return Err(SemanticError::PositiveReadRequired);
        }
        if read.scope_root() != coverage.scope_root() {
            return Err(SemanticError::ScopeMismatch);
        }
        Self::witnessed(read, Some(version), CoverageWitness::Complete(coverage))
    }

    /// Constructs an admitted negative observation.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::NegativeReadRequired`] when `read` is not a
    /// negative selector.
    pub fn negative(
        read: ScopedRead,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        if !read.is_negative() {
            return Err(SemanticError::NegativeReadRequired);
        }
        if read.scope_root() != coverage.scope_root() {
            return Err(SemanticError::ScopeMismatch);
        }
        Self::witnessed(read, None, CoverageWitness::Complete(coverage))
    }

    /// Returns whether this observation can claim absence.
    #[must_use]
    pub fn authoritative_negative(&self) -> bool {
        self.read.is_negative() && matches!(self.coverage, CoverageWitness::Complete(_))
    }

    /// Returns the bound read selector.
    #[must_use]
    pub const fn read(&self) -> &ScopedRead {
        &self.read
    }

    /// Returns the observed value version, when present.
    #[must_use]
    pub const fn version(&self) -> Option<FacetVersion> {
        self.version
    }

    /// Returns the exact coverage witness.
    #[must_use]
    pub const fn coverage(&self) -> CoverageWitness {
        self.coverage
    }
}

/// Full-width exact dependency manifest, including negative and range reads.
#[derive(Debug)]
pub struct ScopedReadManifest {
    // Manifests are immutable content addressed values.  Keeping the sorted
    // rows behind an Arc makes the common "same reads, new result" path a
    // pointer copy; callers that need a mutable builder pay for a clone once
    // at admission rather than on every retained snapshot.
    observations: Arc<[ScopedReadObservation]>,
    canonical: Arc<[u8]>,
    version: Arc<OnceLock<ScopedReadManifestVersion>>,
}

impl Clone for ScopedReadManifest {
    fn clone(&self) -> Self {
        Self {
            observations: Arc::clone(&self.observations),
            canonical: Arc::clone(&self.canonical),
            version: Arc::clone(&self.version),
        }
    }
}

impl PartialEq for ScopedReadManifest {
    fn eq(&self, other: &Self) -> bool {
        self.observations == other.observations
    }
}

impl Eq for ScopedReadManifest {}

impl ScopedReadManifest {
    /// Sorts and admits unique observations with complete negative witnesses.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::DuplicateRead`] for duplicate selectors or
    /// [`SemanticError::IncompleteNegativeFact`] for an incomplete negative
    /// observation.
    pub fn new(mut observations: Vec<ScopedReadObservation>) -> Result<Self, SemanticError> {
        observations.sort_by(|left, right| left.read.cmp(&right.read));
        if observations
            .windows(2)
            .any(|window| window[0].read == window[1].read)
        {
            return Err(SemanticError::DuplicateRead);
        }
        let selectors = observations
            .iter()
            .map(|observation| observation.read().clone())
            .collect::<Vec<_>>();
        if scoped_polarity_conflict(&selectors) {
            return Err(SemanticError::InvalidSelector);
        }
        if observations.iter().any(|observation| {
            observation.read.is_negative() && !observation.authoritative_negative()
        }) {
            return Err(SemanticError::IncompleteNegativeFact);
        }
        let observations: Arc<[ScopedReadObservation]> = Arc::from(observations);
        // Canonicalize once at admission. Version checks are frequent in
        // planner/cache paths and must not repeatedly walk and allocate the
        // complete witnessed read set.
        let version = Arc::new(OnceLock::new());
        let shell = Self {
            observations,
            canonical: Arc::from([]),
            version: Arc::clone(&version),
        };
        let canonical: Arc<[u8]> = Arc::from(canonical_scoped_manifest(&shell));
        let encoded = Self {
            observations: Arc::clone(&shell.observations),
            canonical: Arc::clone(&canonical),
            version: Arc::clone(&version),
        };
        let _ = version.set(ObjectVersion::from_value(&encoded));
        Ok(Self {
            observations: encoded.observations,
            canonical,
            version,
        })
    }

    /// Returns the complete manifest encoding.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.canonical.to_vec()
    }

    /// Borrows the immutable canonical encoding without reallocation.
    #[must_use]
    pub(crate) fn canonical_bytes_ref(&self) -> &[u8] {
        &self.canonical
    }

    /// Returns the version of the exact dependency manifest.
    #[must_use]
    pub fn version(&self) -> ScopedReadManifestVersion {
        *self.version.get_or_init(|| ObjectVersion::from_value(self))
    }

    /// Returns whether any changed selector invalidates the manifest.
    #[must_use]
    pub fn invalidated_by(&self, changed: &[ScopedRead]) -> bool {
        self.observations.iter().any(|observation| {
            changed
                .iter()
                .any(|candidate| observation.read.intersects(candidate))
        })
    }

    /// Returns the shared immutable observation storage.  This is the
    /// zero-copy handoff used by planners and replication envelopes.
    #[must_use]
    pub fn shared_observations(&self) -> Arc<[ScopedReadObservation]> {
        Arc::clone(&self.observations)
    }

    /// Returns the canonically sorted witnessed reads.
    #[must_use]
    pub fn observations(&self) -> &[ScopedReadObservation] {
        &self.observations
    }

    /// Consumes the manifest and returns its witnessed reads.
    #[must_use]
    pub fn into_observations(self) -> Vec<ScopedReadObservation> {
        self.observations.to_vec()
    }
}

/// One exact read paired with the witness that admitted it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadObservation {
    read: Read,
    version: Option<FacetVersion>,
    coverage: CoverageWitness,
}

impl ReadObservation {
    /// Admits any polarity-compatible witnessed read, including incomplete
    /// positive observations.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::InvalidCoverageState`] for a negative read
    /// paired with a value version, or [`SemanticError::ScopeMismatch`] when
    /// the witness names another scope.
    pub fn witnessed(
        read: Read,
        version: Option<FacetVersion>,
        coverage: CoverageWitness,
    ) -> Result<Self, SemanticError> {
        if read.is_negative() && version.is_some() {
            return Err(SemanticError::InvalidCoverageState);
        }
        if read.scope_root() != coverage.scope_root() {
            return Err(SemanticError::ScopeMismatch);
        }
        Ok(Self {
            read,
            version,
            coverage,
        })
    }

    /// Constructs an admitted positive observation.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::PositiveReadRequired`] when the read is
    /// negative, or [`SemanticError::ScopeMismatch`] when the witness names
    /// another authority scope.
    pub fn positive(
        read: Read,
        version: FacetVersion,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        if read.is_negative() {
            return Err(SemanticError::PositiveReadRequired);
        }
        if read.scope_root() != coverage.scope_root() {
            return Err(SemanticError::ScopeMismatch);
        }
        Self::witnessed(read, Some(version), CoverageWitness::Complete(coverage))
    }

    /// Constructs an admitted negative observation. The read must be negative.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::NegativeReadRequired`] when `read` is not a
    /// negative selector.
    pub fn negative(
        read: Read,
        coverage: AuthorizedCompleteCoverage,
    ) -> Result<Self, SemanticError> {
        if !read.is_negative() {
            return Err(SemanticError::NegativeReadRequired);
        }
        if read.scope_root() != coverage.scope_root() {
            return Err(SemanticError::ScopeMismatch);
        }
        Self::witnessed(read, None, CoverageWitness::Complete(coverage))
    }

    /// Returns whether this observation may claim an exact absence.
    #[must_use]
    pub const fn authoritative_negative(self) -> bool {
        self.read.is_negative() && matches!(self.coverage, CoverageWitness::Complete(_))
    }

    /// Returns the bound read selector.
    #[must_use]
    pub const fn read(self) -> Read {
        self.read
    }

    /// Returns the observed value version, when present.
    #[must_use]
    pub const fn version(self) -> Option<FacetVersion> {
        self.version
    }

    /// Returns the exact coverage witness.
    #[must_use]
    pub const fn coverage(self) -> CoverageWitness {
        self.coverage
    }
}

/// Exact positive/negative/range dependency manifest.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ReadManifest {
    reads: Arc<[Read]>,
    canonical: Arc<[u8]>,
}

impl ReadManifest {
    /// Sorts and admits a manifest with no duplicate selectors.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::DuplicateRead`] when a selector appears more
    /// than once.
    pub fn new(mut reads: Vec<Read>) -> Result<Self, SemanticError> {
        reads.sort();
        if reads.windows(2).any(|window| window[0] == window[1]) {
            Err(SemanticError::DuplicateRead)
        } else if compact_polarity_conflict(&reads) {
            Err(SemanticError::InvalidSelector)
        } else {
            let reads: Arc<[Read]> = Arc::from(reads);
            let canonical: Arc<[u8]> =
                Arc::from(crate::canonical::canonical_read_manifest_reads(&reads));
            Ok(Self { reads, canonical })
        }
    }

    /// Returns the canonical manifest encoding.
    #[must_use]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        self.canonical.to_vec()
    }

    /// Borrows the cached canonical selector encoding for schema hashing.
    #[must_use]
    pub(crate) fn canonical_bytes_ref(&self) -> &[u8] {
        &self.canonical
    }

    /// Returns the typed manifest value version.
    #[must_use]
    pub fn version(&self) -> ReadManifestVersion {
        ObjectVersion::from_value(self)
    }

    /// Returns whether a changed read invalidates this manifest.
    #[must_use]
    pub fn invalidated_by(&self, changed: &[Read]) -> bool {
        self.reads
            .iter()
            .any(|read| changed.iter().any(|candidate| read.intersects(*candidate)))
    }

    /// Alias with wording used by dependency planners.
    #[must_use]
    pub fn is_invalidated_by(&self, changed: &[Read]) -> bool {
        self.invalidated_by(changed)
    }

    /// Returns the shared immutable selector storage for a zero-copy plan
    /// handoff.
    #[must_use]
    pub fn shared_reads(&self) -> Arc<[Read]> {
        Arc::clone(&self.reads)
    }

    /// Returns whether every negative selector has a complete witness.
    #[must_use]
    pub fn accepts_negative_observations(&self, observations: &[ReadObservation]) -> bool {
        self.reads
            .iter()
            .filter(|read| read.is_negative())
            .all(|read| {
                observations.iter().any(|observation| {
                    observation.read() == *read && observation.authoritative_negative()
                })
            })
    }

    /// Rejects an incomplete witness for a negative fact.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::NegativeReadRequired`] for a positive read or
    /// [`SemanticError::IncompleteNegativeFact`] for a non-complete witness.
    pub fn require_complete_negative(
        read: Read,
        coverage: CoverageWitness,
    ) -> Result<AuthorizedCompleteCoverage, SemanticError> {
        if !read.is_negative() {
            return Err(SemanticError::NegativeReadRequired);
        }
        match coverage {
            CoverageWitness::Complete(value) => Ok(value),
            CoverageWitness::UntrustedComplete(_)
            | CoverageWitness::Closed(_)
            | CoverageWitness::Partial(_)
            | CoverageWitness::Unavailable(_)
            | CoverageWitness::Unsupported(_) => Err(SemanticError::IncompleteNegativeFact),
        }
    }

    /// Returns the canonical sorted read selectors.
    #[must_use]
    pub fn reads(&self) -> &[Read] {
        &self.reads
    }

    /// Consumes the manifest and returns its sorted selectors.
    #[must_use]
    pub fn into_reads(self) -> Vec<Read> {
        self.reads.to_vec()
    }
}

/// A typed manifest retaining exact selectors and their witnesses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExactReadManifest {
    observations: Arc<[ReadObservation]>,
    canonical: Arc<[u8]>,
}

impl ExactReadManifest {
    /// Admits sorted unique witnessed reads.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::DuplicateRead`] for duplicate selectors or
    /// [`SemanticError::IncompleteNegativeFact`] for an incomplete negative
    /// observation.
    pub fn new(mut observations: Vec<ReadObservation>) -> Result<Self, SemanticError> {
        observations.sort_by_key(|observation| observation.read);
        if observations
            .windows(2)
            .any(|window| window[0].read == window[1].read)
        {
            return Err(SemanticError::DuplicateRead);
        }
        let reads = observations
            .iter()
            .map(|observation| observation.read())
            .collect::<Vec<_>>();
        if compact_polarity_conflict(&reads) {
            return Err(SemanticError::InvalidSelector);
        }
        if observations.iter().any(|observation| {
            observation.read().is_negative() && !observation.authoritative_negative()
        }) {
            return Err(SemanticError::IncompleteNegativeFact);
        }
        let observations: Arc<[ReadObservation]> = Arc::from(observations);
        let canonical: Arc<[u8]> =
            Arc::from(crate::canonical::canonical_exact_read_manifest_observations(&observations));
        Ok(Self {
            observations,
            canonical,
        })
    }

    /// Returns the compact manifest projection.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::DuplicateRead`] if the public observations
    /// contain duplicate selectors.
    pub fn manifest(&self) -> Result<ReadManifest, SemanticError> {
        ReadManifest::new(self.observations.iter().map(|row| row.read()).collect())
    }

    /// Returns the canonical typed version.
    #[must_use]
    pub fn version(&self) -> ExactReadManifestVersion {
        ObjectVersion::from_value(self)
    }

    /// Tests invalidation against changed selectors.
    #[must_use]
    pub fn invalidated_by(&self, changed: &[Read]) -> bool {
        self.observations.iter().any(|row| {
            changed
                .iter()
                .any(|candidate| row.read().intersects(*candidate))
        })
    }

    /// Returns the shared immutable witnessed rows for a zero-copy proof
    /// handoff.
    #[must_use]
    pub fn shared_observations(&self) -> Arc<[ReadObservation]> {
        Arc::clone(&self.observations)
    }

    /// Returns the canonically sorted witnessed reads.
    #[must_use]
    pub fn observations(&self) -> &[ReadObservation] {
        &self.observations
    }

    /// Borrows the cached canonical encoding for schema hashing.
    #[must_use]
    pub(crate) fn canonical_bytes_ref(&self) -> &[u8] {
        &self.canonical
    }

    /// Consumes the manifest and returns its witnessed reads.
    #[must_use]
    pub fn into_observations(self) -> Vec<ReadObservation> {
        self.observations.to_vec()
    }
}
