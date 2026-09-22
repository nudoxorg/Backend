//! Allocation-free admission and reconciliation of observed index versions.

use crate::ir::DeclarationIdentity;
use crate::index_vocabulary::{
    IndexLocatorFacts, PackageCoordinate, SemanticImageLocator, VerifiedCanonicalEntityLocator,
    VerifiedSemanticPublication,
};

/// Maximum number of versions admitted from either side of one reconciliation page/batch.
///
/// This is a measured page budget, not a product wall: real multi-package
/// ingestion pages exceed the old 64-row bound, so it is raised to a bounded
/// 256 rows (matching the per-segment row scale) while keeping the
/// `O(rows log rows)` index ordering and duplicate check plus fixed `[usize; ..]`
/// scratch bounded. A page beyond this still returns the typed
/// [`ReconciliationFault::InputTooLong`] rejection.
pub const MAX_RECONCILIATION_ROWS: usize = 256;

/// Declarative source of an observed version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngestionOrigin {
    /// Registry update observation.
    RegistryUpdate,
    /// Remote pull observation.
    RemotePull,
    /// Scrape observation.
    Scrape,
}

/// One admitted version retaining only typed compiler/index locators.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IngestedVersion<'coordinate, 'entities> {
    origin: IngestionOrigin,
    coordinate: PackageCoordinate<'coordinate>,
    publication: VerifiedSemanticPublication,
    entities: &'entities [VerifiedCanonicalEntityLocator],
}

/// Exact admission failures for an observed version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngestedVersionFault {
    /// The caller omitted or added canonical declarations relative to the reopened image.
    EntityCountMismatch {
        /// Count retained in the semantic publication proof.
        expected: usize,
        /// Count supplied to this admission boundary.
        observed: usize,
    },
    /// An entity locator names another semantic image.
    EntityImageMismatch,
    /// Two entity locators repeat one declaration identity.
    DuplicateDeclaration(DeclarationIdentity),
    /// Entity locators were not supplied in canonical declaration order.
    EntityLocatorOutOfOrder {
        /// The preceding declaration identity in the supplied slice.
        previous: DeclarationIdentity,
        /// The declaration identity that violated strictly increasing order.
        observed: DeclarationIdentity,
    },
}

impl<'coordinate, 'entities> IngestedVersion<'coordinate, 'entities> {
    /// Admits a version after checking image containment and declaration uniqueness.
    /// Entity inputs must be strictly increasing by declaration identity; each locator
    /// remains independently verified against its canonical ordinal and image.
    pub fn new(
        origin: IngestionOrigin,
        coordinate: PackageCoordinate<'coordinate>,
        publication: VerifiedSemanticPublication,
        entities: &'entities [VerifiedCanonicalEntityLocator],
    ) -> Result<Self, IngestedVersionFault> {
        if entities.len() != publication.entity_count() {
            return Err(IngestedVersionFault::EntityCountMismatch {
                expected: publication.entity_count(),
                observed: entities.len(),
            });
        }
        let mut left = 0;
        while left < entities.len() {
            let current = entities[left].as_locator();
            if current.image != publication.image() {
                return Err(IngestedVersionFault::EntityImageMismatch);
            }
            if left != 0 {
                let previous = entities[left - 1].as_locator().declaration;
                if previous == current.declaration {
                    return Err(IngestedVersionFault::DuplicateDeclaration(previous));
                }
                if previous > current.declaration {
                    return Err(IngestedVersionFault::EntityLocatorOutOfOrder {
                        previous,
                        observed: current.declaration,
                    });
                }
            }
            left += 1;
        }
        Ok(Self {
            origin,
            coordinate,
            publication,
            entities,
        })
    }

    /// Returns the declarative acquisition origin.
    pub const fn origin(self) -> IngestionOrigin {
        self.origin
    }
    /// Returns the package coordinate.
    pub const fn coordinate(self) -> PackageCoordinate<'coordinate> {
        self.coordinate
    }
    /// Returns immutable generation, snapshot, and publication authorities.
    pub const fn locator_facts(self) -> IndexLocatorFacts {
        self.publication.authority()
    }
    /// Returns the canonical image locator.
    pub const fn image(self) -> SemanticImageLocator {
        self.publication.image()
    }
    /// Returns verified declaration locators.
    pub const fn entities(self) -> &'entities [VerifiedCanonicalEntityLocator] {
        self.entities
    }
}

/// One closed local-to-desired catalog operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationOperation<'record, 'coordinate, 'entities> {
    /// Desired version has no local coordinate.
    Insert(&'record IngestedVersion<'coordinate, 'entities>),
    /// Desired version supersedes the local image at the same coordinate.
    Replace {
        /// The current local version.
        local: &'record IngestedVersion<'coordinate, 'entities>,
        /// The desired version with a new image.
        desired: &'record IngestedVersion<'coordinate, 'entities>,
    },
    /// Desired and local records are equal in every retained field.
    Unchanged(&'record IngestedVersion<'coordinate, 'entities>),
    /// Desired record changes retained metadata while preserving its image.
    Rebind {
        /// The current local version.
        local: &'record IngestedVersion<'coordinate, 'entities>,
        /// The desired version with changed facts, origin, or entity locators.
        desired: &'record IngestedVersion<'coordinate, 'entities>,
    },
    /// Local coordinate is absent from the desired set.
    Remove(&'record IngestedVersion<'coordinate, 'entities>),
}

/// Exact preflight failures for bounded reconciliation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationFault<'coordinate> {
    /// One input contains more than [`MAX_RECONCILIATION_ROWS`] rows.
    InputTooLong,
    /// An input repeats one package coordinate.
    DuplicateCoordinate(PackageCoordinate<'coordinate>),
    /// Caller scratch cannot hold every operation.
    OutputTooShort {
        /// The minimum number of operation slots required by the inputs.
        required: usize,
    },
}

/// Monotonic durable-apply checkpoint for one ingestion page.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    /// Monotonic page sequence.
    pub sequence: u64,
    /// Cursor within the page.
    pub page: u64,
}
/// Exact checkpoint admission failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointFault {
    /// Candidate is not strictly newer.
    NotHigher,
}
/// A checkpoint that may advance only after durable application succeeds.
pub struct PendingCheckpoint {
    current: Checkpoint,
    next: Checkpoint,
}
/// Error returned when durable application fails; the current checkpoint is retained.
#[derive(Debug, Eq, PartialEq)]
pub struct DurableApplyFault<E> {
    /// The checkpoint to retry.
    pub current: Checkpoint,
    /// The durable operation error.
    pub error: E,
}
impl Checkpoint {
    /// Prepares a strictly higher checkpoint without persisting it.
    pub const fn prepare_next(
        self,
        next: Checkpoint,
    ) -> Result<PendingCheckpoint, CheckpointFault> {
        if next.sequence > self.sequence
            || (next.sequence == self.sequence && next.page > self.page)
        {
            Ok(PendingCheckpoint {
                current: self,
                next,
            })
        } else {
            Err(CheckpointFault::NotHigher)
        }
    }
}
impl PendingCheckpoint {
    /// Runs the caller's durable operation; failures retain the old checkpoint.
    pub fn advance_after_durable_apply<E, F: FnOnce() -> Result<(), E>>(
        self,
        apply: F,
    ) -> Result<Checkpoint, DurableApplyFault<E>> {
        apply()
            .map(|()| self.next)
            .map_err(|error| DurableApplyFault {
                current: self.current,
                error,
            })
    }
}

/// Reconciles two arbitrarily ordered version sets into deterministic caller scratch.
pub fn reconcile_into<'record, 'coordinate, 'entities>(
    local: &'record [IngestedVersion<'coordinate, 'entities>],
    desired: &'record [IngestedVersion<'coordinate, 'entities>],
    out: &mut [ReconciliationOperation<'record, 'coordinate, 'entities>],
) -> Result<usize, ReconciliationFault<'coordinate>> {
    if local.len() > MAX_RECONCILIATION_ROWS || desired.len() > MAX_RECONCILIATION_ROWS {
        return Err(ReconciliationFault::InputTooLong);
    }
    // Keep sorting scratch on the stack.  The old implementation repeatedly
    // scanned both unsorted inputs for every row, making a full bounded page
    // quadratic in duplicate checks, matches, and ordering.  Sorting indexes
    // preserves borrowed rows and gives us one canonical order for all three
    // passes without allocating or copying package records.
    let mut local_order = [0_usize; MAX_RECONCILIATION_ROWS];
    let mut desired_order = [0_usize; MAX_RECONCILIATION_ROWS];
    for (index, slot) in local_order.iter_mut().take(local.len()).enumerate() {
        *slot = index;
    }
    for (index, slot) in desired_order.iter_mut().take(desired.len()).enumerate() {
        *slot = index;
    }
    local_order[..local.len()].sort_unstable_by(|left, right| {
        coordinate_cmp(local[*left].coordinate(), local[*right].coordinate())
    });
    desired_order[..desired.len()].sort_unstable_by(|left, right| {
        coordinate_cmp(desired[*left].coordinate(), desired[*right].coordinate())
    });
    check_sorted_duplicates(local, &local_order[..local.len()])?;
    check_sorted_duplicates(desired, &desired_order[..desired.len()])?;
    let removals = local_order[..local.len()]
        .iter()
        .filter(|index| {
            desired_order[..desired.len()]
                .binary_search_by(|desired_index| {
                    coordinate_cmp(
                        desired[*desired_index].coordinate(),
                        local[**index].coordinate(),
                    )
                })
                .is_err()
        })
        .count();
    let required = desired.len() + removals;
    if out.len() < required {
        return Err(ReconciliationFault::OutputTooShort { required });
    }
    let mut written = 0;
    for desired_index in desired_order[..desired.len()].iter().copied() {
        let desired_row = &desired[desired_index];
        match local_order[..local.len()].binary_search_by(|local_index| {
            coordinate_cmp(local[*local_index].coordinate(), desired_row.coordinate())
        }) {
            Ok(sorted_position) => {
                // `binary_search_by` returns the position in the sorted
                // scratch slice.  Convert it back to the caller's row index
                // before borrowing the local record; unsorted inputs are a
                // supported contract, not an incidental case.
                let local_index = local_order[sorted_position];
                out[written] = if local[local_index] == *desired_row {
                    ReconciliationOperation::Unchanged(desired_row)
                } else if local[local_index].image() == desired_row.image() {
                    ReconciliationOperation::Rebind {
                        local: &local[local_index],
                        desired: desired_row,
                    }
                } else {
                    ReconciliationOperation::Replace {
                        local: &local[local_index],
                        desired: desired_row,
                    }
                };
            }
            Err(_) => out[written] = ReconciliationOperation::Insert(desired_row),
        }
        written += 1;
    }
    for local_index in local_order[..local.len()].iter().copied() {
        let row = &local[local_index];
        if desired_order[..desired.len()]
            .binary_search_by(|desired_index| {
                coordinate_cmp(desired[*desired_index].coordinate(), row.coordinate())
            })
            .is_err()
        {
            out[written] = ReconciliationOperation::Remove(row);
            written += 1;
        }
    }
    Ok(written)
}

fn check_sorted_duplicates<'a, 'e>(
    rows: &[IngestedVersion<'a, 'e>],
    order: &[usize],
) -> Result<(), ReconciliationFault<'a>> {
    for pair in order.windows(2) {
        let left = &rows[pair[0]];
        let right = &rows[pair[1]];
        if coordinate_cmp(left.coordinate(), right.coordinate()).is_eq() {
            return Err(ReconciliationFault::DuplicateCoordinate(
                PackageCoordinate {
                    lineage: left.coordinate().lineage,
                    version: left.coordinate().version,
                },
            ));
        }
    }
    Ok(())
}

fn coordinate_cmp<'a>(
    left: PackageCoordinate<'a>,
    right: PackageCoordinate<'a>,
) -> core::cmp::Ordering {
    left.lineage
        .ecosystem
        .cmp(right.lineage.ecosystem)
        .then(left.lineage.name.cmp(right.lineage.name))
        .then(left.version.as_str().cmp(right.version.as_str()))
}
