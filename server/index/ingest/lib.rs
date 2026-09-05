//! Allocation-free admission and reconciliation of observed index versions.
#![no_std]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use compiler_ir::DeclarationIdentity;
use server_index_vocabulary::{
    IndexLocatorFacts, PackageCoordinate, SemanticImageLocator, VerifiedCanonicalEntityLocator,
};

/// Maximum number of versions admitted from either side of one reconciliation page/batch.
pub const MAX_RECONCILIATION_ROWS: usize = 64;

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
    facts: IndexLocatorFacts,
    image: SemanticImageLocator,
    entities: &'entities [VerifiedCanonicalEntityLocator],
}

/// Exact admission failures for an observed version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngestedVersionFault {
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
        facts: IndexLocatorFacts,
        image: SemanticImageLocator,
        entities: &'entities [VerifiedCanonicalEntityLocator],
    ) -> Result<Self, IngestedVersionFault> {
        let mut left = 0;
        while left < entities.len() {
            let current = entities[left].as_locator();
            if current.image != image {
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
            facts,
            image,
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
        self.facts
    }
    /// Returns the canonical image locator.
    pub const fn image(self) -> SemanticImageLocator {
        self.image
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
    check_duplicates(local)?;
    check_duplicates(desired)?;
    let mut removals = 0;
    for row in local {
        if find_coordinate(desired, row.coordinate()).is_none() {
            removals += 1;
        }
    }
    let required = desired.len() + removals;
    if out.len() < required {
        return Err(ReconciliationFault::OutputTooShort { required });
    }
    let mut written = 0;
    let mut desired_used = [false; MAX_RECONCILIATION_ROWS];
    let mut rank = 0;
    while rank < desired.len() {
        let index = next_coordinate(desired, &mut desired_used);
        let desired_row = &desired[index];
        match find_coordinate(local, desired_row.coordinate()) {
            Some(local_index) if local[local_index] == *desired_row => {
                out[written] = ReconciliationOperation::Unchanged(desired_row)
            }
            Some(local_index) if local[local_index].image == desired_row.image => {
                out[written] = ReconciliationOperation::Rebind {
                    local: &local[local_index],
                    desired: desired_row,
                }
            }
            Some(local_index) => {
                out[written] = ReconciliationOperation::Replace {
                    local: &local[local_index],
                    desired: desired_row,
                }
            }
            None => out[written] = ReconciliationOperation::Insert(desired_row),
        }
        written += 1;
        rank += 1;
    }
    let mut local_used = [false; MAX_RECONCILIATION_ROWS];
    let mut local_rank = 0;
    while local_rank < local.len() {
        let index = next_coordinate(local, &mut local_used);
        let row = &local[index];
        if find_coordinate(desired, row.coordinate()).is_none() {
            out[written] = ReconciliationOperation::Remove(row);
            written += 1;
        }
        local_rank += 1;
    }
    Ok(written)
}

fn check_duplicates<'a, 'e>(
    rows: &[IngestedVersion<'a, 'e>],
) -> Result<(), ReconciliationFault<'a>> {
    let mut left = 0;
    while left < rows.len() {
        let mut right = left + 1;
        while right < rows.len() {
            if rows[left].coordinate() == rows[right].coordinate() {
                return Err(ReconciliationFault::DuplicateCoordinate(
                    PackageCoordinate {
                        lineage: rows[left].coordinate().lineage,
                        version: rows[left].coordinate().version,
                    },
                ));
            }
            right += 1;
        }
        left += 1;
    }
    Ok(())
}

fn find_coordinate<'a, 'e>(
    rows: &[IngestedVersion<'a, 'e>],
    coordinate: PackageCoordinate<'a>,
) -> Option<usize> {
    rows.iter().position(|row| row.coordinate() == coordinate)
}

fn next_coordinate<'a, 'e>(
    rows: &[IngestedVersion<'a, 'e>],
    used: &mut [bool; MAX_RECONCILIATION_ROWS],
) -> usize {
    let mut selected = 0;
    while used[selected] {
        selected += 1;
    }
    for index in (selected + 1)..rows.len() {
        if !used[index]
            && coordinate_cmp(rows[index].coordinate(), rows[selected].coordinate()).is_lt()
        {
            selected = index;
        }
    }
    used[selected] = true;
    selected
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
