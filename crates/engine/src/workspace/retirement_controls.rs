//! Flat, ledger-bound selected control identities for retirement replay.
//!
//! These are physical identity facts, not typed relation admissions. The
//! selected transition still requires its normal typed model admission.

use backend_store::{ArtifactBudget, ClosureId, FileStore, GcPinGuard, ObjectId, StoreError};
use backend_version::{
    SchemaIdentity,
    retirement_budget::{Metered, RetirementBudget},
};

// The canonical workspace pack has five payloads and at most 128 auxiliary
// IDs. Keeping the array inline avoids a second allocation or cloneable graph.
const MAX_CONTROLS: usize =
    super::transition::TRANSITION_PAYLOAD_COUNT + super::pack::MAX_AUXILIARY_OBJECTS;

type Entry = (ObjectId, SchemaIdentity);

struct Entries {
    closure: ClosureId,
    len: usize,
    items: [Option<Entry>; MAX_CONTROLS],
}

/// Selected pack control IDs and their authenticated physical schemas.
///
/// The owner is not cloneable, does not expose typed objects, and keeps both
/// its accounting reservation and the store pin alive until destruction.
pub struct RetirementControlFrontier<'a> {
    selected: &'a Metered<super::pack::RetirementPackFrontier<'a>>,
    entries: Metered<Entries>,
}

impl<'a> RetirementControlFrontier<'a> {
    // Only the checked selected-pack parser may call this. It must pass the
    // complete payload and auxiliary IDs, not a caller-selected subset.
    pub(super) fn from_checked_pack(
        selected: &'a Metered<super::pack::RetirementPackFrontier<'a>>,
        artifact_budget: ArtifactBudget,
        maximum_payload_bytes: usize,
        budget: &RetirementBudget,
        bookkeeping: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Self, StoreError> {
        if !budget.owns_and_covers(
            selected,
            std::mem::size_of::<super::pack::RetirementPackFrontier<'a>>(),
        ) || !selected.get().belongs_to_budget(budget)
        {
            return Err(StoreError::Bounds);
        }
        let store = selected.get().store();
        let reservation = budget
            .reserve(std::mem::size_of::<Entries>())
            .map_err(|_| StoreError::Bounds)?;
        let mut entries = Entries {
            closure: selected.get().closure(),
            len: 0,
            items: [None; MAX_CONTROLS],
        };
        let _sink_reservation = budget
            .reserve(
                store
                    .retirement_arc_clone_bytes(bookkeeping)
                    .ok_or(StoreError::Bounds)?,
            )
            .map_err(|_| StoreError::Bounds)?;
        let sink = store.artifact_sink(artifact_budget);
        for claim in selected.get().ids() {
            if cancelled() || entries.len == MAX_CONTROLS {
                return Err(StoreError::Bounds);
            }
            if entries.items[..entries.len]
                .iter()
                .flatten()
                .any(|entry| entry.0.as_bytes() == claim.as_bytes())
            {
                return Err(StoreError::Corrupt);
            }
            let (member, _) = store.contains_closure_member_retirement(
                selected.get().pin(),
                entries.closure,
                claim,
                32,
                budget,
                cancelled,
            )?;
            if !member {
                return Err(StoreError::Corrupt);
            }
            let (actual, schema) = sink.read_retirement_identity(
                claim,
                maximum_payload_bytes,
                budget,
                bookkeeping,
                cancelled,
            )?;
            if actual.as_bytes() != claim.as_bytes() {
                return Err(StoreError::Corrupt);
            }
            entries.items[entries.len] = Some((actual, schema));
            entries.len += 1;
        }
        if cancelled() {
            return Err(StoreError::Bounds);
        }
        Ok(Self {
            selected,
            entries: reservation.meter(entries),
        })
    }

    /// Check the ledger and selected closure before using these facts.
    pub fn belongs_to(
        &self,
        store: &FileStore,
        budget: &RetirementBudget,
        closure: ClosureId,
    ) -> bool {
        std::ptr::eq(store, self.selected.get().store())
            && budget.owns_and_covers(
                self.selected,
                std::mem::size_of::<super::pack::RetirementPackFrontier<'a>>(),
            )
            && self.selected.get().belongs_to_budget(budget)
            && budget.owns(&self.entries)
            && self.entries.get().closure == closure
    }

    /// Borrow the selected capability's GC pin for the exact union check.
    pub fn pin(&self) -> &GcPinGuard {
        self.selected.get().pin()
    }

    /// Number of distinct selected control identities.
    pub fn len(&self) -> usize {
        self.entries.get().len
    }

    /// Whether no controls were retained.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Visit physical IDs and schemas; this exposes no cloneable control graph.
    pub fn visit<E>(
        &self,
        mut visit: impl FnMut(ObjectId, SchemaIdentity) -> Result<(), E>,
    ) -> Result<(), E> {
        for &(id, schema) in self.entries.get().items[..self.len()].iter().flatten() {
            visit(id, schema)?;
        }
        Ok(())
    }
}
