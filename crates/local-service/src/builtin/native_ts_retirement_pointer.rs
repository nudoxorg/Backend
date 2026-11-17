//! Narrow, non-authoritative staged pointer for future V17 recovery.
//!
//! The pointer alone grants no authority. Its basis and exact request still
//! require owner/base and canonical-intent admission after the selected
//! closure's complete stage-plus-controls union is proven.
use super::profile::BuiltinIntentSchema;
use super::staged_intent::{self, StageBasis};
use backend_store::{ArtifactBudget, ClosureId, FileStore, StoreError, UntrustedObjectId};
use backend_version::{ObjectVersionHasher, Schema, SchemaIdentity};
use backend_version::retirement_budget::{Metered, RetirementBudget};

const MAGIC: &[u8; 4] = b"BPS2";
const POINTER_BYTES: usize = 180;
const PAGE_BYTES: usize = 64 * 1024;
const PAGE_HEADER: usize = 44;
const MAX_INTENT_BYTES: usize = 2 * PAGE_BYTES;

/// Sealed stage bytes. Only the bounded pointer reader in this module can
/// construct this owner; the buffer and its prospective lease cannot be
/// separated or cloned into an authority capability.
pub(super) struct RetirementWire {
    bytes: Metered<Vec<u8>>,
}

impl RetirementWire {
    /// Borrow untrusted bytes for narrow parsing; callers must not retain an
    /// uncharged copy or treat these bytes as admitted authority.
    pub(super) fn bytes(&self) -> &[u8] { self.bytes.get().as_slice() }

    /// Check same-ledger ownership and the complete retained buffer charge.
    pub(super) fn covered_by(&self, budget: &RetirementBudget, bookkeeping: usize) -> bool {
        self.bytes.get().capacity().checked_add(bookkeeping)
            .is_some_and(|bytes| budget.owns_and_covers(&self.bytes, bytes))
    }
}

/// Parsed wire fields only. None is authenticated by parsing.
pub(super) struct RetirementPointer {
    manifest: UntrustedObjectId,
    basis: StageBasis,
}

impl RetirementPointer {
    /// Parse a fixed-width, separately tagged retirement pointer without I/O
    /// or heap allocation. Legacy/unknown pointer kinds fail closed here.
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() != POINTER_BYTES || &bytes[..4] != MAGIC {
            return Err("invalid retirement stage pointer");
        }
        fn hash(bytes: &[u8]) -> Result<[u8; 32], &'static str> {
            bytes.try_into().map_err(|_| "invalid retirement pointer field")
        }
        let manifest = UntrustedObjectId::from_bytes(hash(&bytes[4..36])?);
        let owner_epoch = u64::from_be_bytes(bytes[36..44].try_into().map_err(|_| "invalid owner epoch")?);
        let workspace_sequence = u64::from_be_bytes(bytes[44..52].try_into().map_err(|_| "invalid workspace sequence")?);
        let basis = StageBasis {
            owner_epoch,
            workspace_sequence,
            owner_fence: hash(&bytes[52..84])?,
            workspace_root: hash(&bytes[84..116])?,
            closure_id: hash(&bytes[116..148])?,
            source_capture: hash(&bytes[148..180])?,
        };
        Ok(Self { manifest, basis })
    }

    /// Reconstruct exact untrusted request bytes from a selected closure.
    /// `controls` must be the selected pack's metered control frontier. The
    /// result is not an admitted intent and cannot authorize publication.
    pub(super) fn read_bytes(
        &self,
        store: &FileStore,
        selected: ClosureId,
        controls: &backend_engine::workspace::retirement_controls::RetirementControlFrontier,
        budget: &RetirementBudget,
        bookkeeping: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<RetirementWire, StoreError> {
        if !controls.belongs_to(store, budget, selected) || cancelled() { return Err(StoreError::Bounds); }
        let _sink_charge = budget.reserve(store.retirement_arc_clone_bytes(bookkeeping).ok_or(StoreError::Bounds)?)
            .map_err(|_| StoreError::Bounds)?;
        let max_object = PAGE_HEADER.checked_add(PAGE_BYTES).ok_or(StoreError::Bounds)?;
        let artifact = ArtifactBudget::new(1, 1, u64::try_from(max_object).map_err(|_| StoreError::Bounds)?, max_object, 1);
        let sink = store.artifact_sink(artifact);
        let (manifest, schema) = sink.read_retirement_identity(self.manifest, max_object, budget, bookkeeping, cancelled)?;
        if schema != SchemaIdentity::new(0x96, 13, 1) || manifest.as_bytes() != self.manifest.as_bytes() {
            return Err(StoreError::Corrupt);
        }
        let output_charge = MAX_INTENT_BYTES.checked_add(bookkeeping).ok_or(StoreError::Bounds)?;
        let output = budget.reserve(output_charge).map_err(|_| StoreError::Bounds)?;
        // Vec::with_capacity requests exactly MAX_INTENT_BYTES from the
        // allocator. A larger reported capacity is outside the retained
        // capacity charge and must not escape this admission.
        let mut bytes = Vec::with_capacity(MAX_INTENT_BYTES);
        if bytes.capacity() > MAX_INTENT_BYTES { return Err(StoreError::Bounds); }
        // This sink is only used to open individually bounded objects; exact
        // closure membership and aggregate control union are proven separately.
        staged_intent::visit_empty_retirement_scope(
            store, manifest, selected, self.basis, artifact, budget, Some(controls), bookkeeping,
            cancelled, |_, payload| {
                if bytes.len().checked_add(payload.len()).is_none_or(|n| n > MAX_INTENT_BYTES) {
                    return Err(StoreError::Bounds);
                }
                // Individual pushes cannot reallocate while capacity is
                // sufficient. Bulk insertion has no equivalent guarantee.
                for chunk in payload.chunks(4096) {
                    if cancelled() { return Err(StoreError::Bounds); }
                    for byte in chunk {
                        bytes.push(*byte);
                    }
                }
                Ok(())
            },
        )?;
        if cancelled() || !bytes.starts_with(b"BPIH\x11") { return Err(StoreError::Corrupt); }
        let _hash = budget.reserve(std::mem::size_of::<ObjectVersionHasher>()).map_err(|_| StoreError::Bounds)?;
        let schema = SchemaIdentity::new(BuiltinIntentSchema::DOMAIN, BuiltinIntentSchema::TYPE, BuiltinIntentSchema::VERSION);
        let mut hasher = ObjectVersionHasher::new(schema, bytes.len()).map_err(|_| StoreError::Bounds)?;
        hasher.update(&bytes).map_err(|_| StoreError::Corrupt)?;
        if hasher.finish().map_err(|_| StoreError::Corrupt)? != self.basis.source_capture || cancelled() {
            return Err(StoreError::Corrupt);
        }
        Ok(RetirementWire { bytes: output.meter(bytes) })
    }
}
