//! Bounded streaming gates for the selected semantic publication and capture
//! relations. Every field in an authenticated node is precharged before the
//! relation decoder allocates owned keys or values.

use crate::workspace::{
    WorkspacePreflightRelationHandle, WorkspaceRelationError, WorkspaceSnapshot,
};
use backend_store::StoreError;
use backend_version::{CanonicalNodeView, Schema, SchemaIdentity};

use super::semantic_capture_relation::{
    ProductSemanticCaptureRecord, ProductSemanticCaptureRelation, ProductSemanticCaptureRootSchema,
    semantic_capture_root_pointer_key,
};
use super::semantic_relation::{
    ProductSemanticPublicationKey, ProductSemanticPublicationRecord,
    ProductSemanticPublicationRelation,
};

const KEY_FIXED_BYTES: usize = 4 + 4 + 2 + 1;
const MAX_REFERENCE_BYTES: usize = backend_library::MAX_PRODUCT_TEXT_BYTES
    .max(backend_semantic::vocabulary::MAX_PACKAGE_URL_BYTES);
const MAX_COORDINATE_BYTES: usize = backend_semantic::vocabulary::MAX_PACKAGE_URL_BYTES;
const MAX_SEMANTIC_KEY_BYTES: usize =
    KEY_FIXED_BYTES + MAX_REFERENCE_BYTES + MAX_COORDINATE_BYTES + backend_version::ID_BYTES;
const MAX_PUBLICATION_VALUE_BYTES: usize = 256;
// PSC3 decoding checks CargoTargetScope's encoded length against 8192 and
// PackageCompilerFailure::MAX_ENCODED_BYTES is 8192. PSC1/PSC2 have neither or
// only the latter slot, so this covers every supported version plus fixed data.
const MAX_CAPTURE_JSON_BYTES: usize = 8192;
const MAX_CAPTURE_VALUE_BYTES: usize = 1024 + MAX_CAPTURE_JSON_BYTES * 2;
// CargoTargetScope's `kinds` bound is checked after serde_json constructs its
// Box<[String]>. An 8192-byte hostile array can contain up to 2730 empty
// strings; Vec growth rounds that to at most 4096 slots, and into_boxed_slice
// may overlap old capacity with the new length. Reserve for that pre-validation
// vector plus DTO/parser slack. The compiler failure's closed cause graph has
// no arbitrary collection fields.
const MAX_CAPTURE_SCOPE_TRANSIENT_BYTES: usize = 320 * 1024;
const MAX_CAPTURE_DECODE_ALLOCATIONS: usize = MAX_CAPTURE_JSON_BYTES + 128;
// Five times the encoded bytes covers decoded strings, canonical-JSON scratch,
// and retained failure bytes. The scope vector allowance above is separate.
const MAX_DECODE_ALLOCATIONS: usize = 128;
const MAX_DECODE_OVERHEAD: usize = 64 * 1024;

/// Selects the primary semantic publication root without decoding any node.
pub fn semantic_publication_preflight_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<
    WorkspacePreflightRelationHandle<ProductSemanticPublicationRelation>,
    WorkspaceRelationError,
> {
    snapshot.relation_preflight::<ProductSemanticPublicationRelation>()
}

/// Selects the closure-bound capture root without decoding any node. An absent
/// pointer is preserved as `None`, matching the legacy empty-capture state.
pub fn semantic_capture_preflight_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<
    Option<WorkspacePreflightRelationHandle<ProductSemanticCaptureRelation>>,
    WorkspaceRelationError,
> {
    let key = semantic_capture_root_pointer_key();
    snapshot.auxiliary_relation_preflight::<ProductSemanticCaptureRelation>(
        SchemaIdentity::new(
            ProductSemanticCaptureRootSchema::DOMAIN,
            ProductSemanticCaptureRootSchema::TYPE,
            ProductSemanticCaptureRootSchema::VERSION,
        ),
        *key.as_bytes(),
    )
}

/// Visits selected publication rows through bounded, authenticated nodes.
/// The caller must charge retained rows in `visit`; temporary node/key/value
/// reservations stay held until the streaming operation and all node buffers
/// have been dropped. A returned cursor consumes `semantic_cursor_reservation_bytes`
/// until the caller drops it and refunds that amount with
/// [`release_semantic_cursor`].
pub fn semantic_visit_preflighted<V>(
    relation: &WorkspacePreflightRelationHandle<ProductSemanticPublicationRelation>,
    after: Option<&ProductSemanticPublicationKey>,
    limit: usize,
    budget: &mut usize,
    allocation_bookkeeping_bytes: usize,
    visit: &mut V,
) -> Result<Option<ProductSemanticPublicationKey>, WorkspaceRelationError>
where
    V: FnMut(
        &ProductSemanticPublicationKey,
        ProductSemanticPublicationRecord,
        &mut usize,
    ) -> Result<(), StoreError>,
{
    let mut reservations =
        SemanticRelationDecodeReservations::publication(allocation_bookkeeping_bytes);
    let cursor_charge = reserve_semantic_cursor(budget, allocation_bookkeeping_bytes)
        .map_err(WorkspaceRelationError::Store)?;
    let result = relation.visit(
        after,
        limit,
        32,
        budget,
        &mut |node, budget| reservations.preflight(node, budget),
        visit,
    );
    finish_visit(result, &mut reservations, budget, cursor_charge)
}

/// Visits source-capture rows through bounded, authenticated nodes.
/// Reservations are held through the visitor so returned compiler-failure
/// payloads cannot coexist with uncharged decode scratch. A returned cursor
/// consumes `semantic_cursor_reservation_bytes` until the caller drops it and
/// refunds that amount with [`release_semantic_cursor`].
pub fn capture_visit_preflighted<V>(
    relation: &WorkspacePreflightRelationHandle<ProductSemanticCaptureRelation>,
    after: Option<&ProductSemanticPublicationKey>,
    limit: usize,
    budget: &mut usize,
    allocation_bookkeeping_bytes: usize,
    visit: &mut V,
) -> Result<Option<ProductSemanticPublicationKey>, WorkspaceRelationError>
where
    V: FnMut(
        &ProductSemanticPublicationKey,
        ProductSemanticCaptureRecord,
        &mut usize,
    ) -> Result<(), StoreError>,
{
    let mut reservations =
        SemanticRelationDecodeReservations::capture(allocation_bookkeeping_bytes);
    let cursor_charge = reserve_semantic_cursor(budget, allocation_bookkeeping_bytes)
        .map_err(WorkspaceRelationError::Store)?;
    let result = relation.visit(
        after,
        limit,
        32,
        budget,
        &mut |node, budget| reservations.preflight(node, budget),
        visit,
    );
    finish_visit(result, &mut reservations, budget, cursor_charge)
}

/// Returns the conservative budget reservation held by one returned cursor.
#[must_use]
pub fn semantic_cursor_reservation_bytes(allocation_bookkeeping_bytes: usize) -> Option<usize> {
    MAX_SEMANTIC_KEY_BYTES
        .checked_mul(4)
        .and_then(|n| n.checked_add(MAX_DECODE_OVERHEAD))
        .and_then(|n| {
            MAX_DECODE_ALLOCATIONS
                .checked_mul(allocation_bookkeeping_bytes)
                .and_then(|h| n.checked_add(h))
        })
}

/// Drops one returned semantic cursor and refunds its previously charged
/// reservation to the same budget.
pub fn release_semantic_cursor(
    cursor: ProductSemanticPublicationKey,
    budget: &mut usize,
    allocation_bookkeeping_bytes: usize,
) -> Result<(), StoreError> {
    drop(cursor);
    let charge = semantic_cursor_reservation_bytes(allocation_bookkeeping_bytes)
        .ok_or(StoreError::Bounds)?;
    *budget = budget.checked_add(charge).ok_or(StoreError::Bounds)?;
    Ok(())
}

fn reserve_semantic_cursor(
    budget: &mut usize,
    allocation_bookkeeping_bytes: usize,
) -> Result<usize, StoreError> {
    let charge = semantic_cursor_reservation_bytes(allocation_bookkeeping_bytes)
        .ok_or(StoreError::Bounds)?;
    *budget = budget.checked_sub(charge).ok_or(StoreError::Bounds)?;
    Ok(charge)
}

fn finish_visit(
    result: Result<Option<ProductSemanticPublicationKey>, WorkspaceRelationError>,
    reservations: &mut SemanticRelationDecodeReservations,
    budget: &mut usize,
    cursor_charge: usize,
) -> Result<Option<ProductSemanticPublicationKey>, WorkspaceRelationError> {
    if let Err(error) = reservations.release(budget) {
        *budget = budget
            .checked_add(cursor_charge)
            .ok_or(WorkspaceRelationError::Store(StoreError::Bounds))?;
        return Err(WorkspaceRelationError::Store(error));
    }
    match result {
        Ok(cursor @ Some(_)) => Ok(cursor),
        Ok(None) => {
            *budget = budget
                .checked_add(cursor_charge)
                .ok_or(WorkspaceRelationError::Store(StoreError::Bounds))?;
            Ok(None)
        }
        Err(error) => {
            *budget = budget
                .checked_add(cursor_charge)
                .ok_or(WorkspaceRelationError::Store(StoreError::Bounds))?;
            Err(error)
        }
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Publication,
    Capture,
}

/// Reusable per-node decode reservation for visit and bounded update paths.
/// The kind selects the closed relation grammar whose typed decoder follows.
pub struct SemanticRelationDecodeReservations {
    bookkeeping: usize,
    held: usize,
    kind: Kind,
}

impl SemanticRelationDecodeReservations {
    /// Creates a reservation for publication relation nodes.
    #[must_use]
    pub const fn publication(bookkeeping: usize) -> Self {
        Self::new(bookkeeping, Kind::Publication)
    }

    /// Creates a reservation for capture relation nodes.
    #[must_use]
    pub const fn capture(bookkeeping: usize) -> Self {
        Self::new(bookkeeping, Kind::Capture)
    }

    const fn new(bookkeeping: usize, kind: Kind) -> Self {
        Self {
            bookkeeping,
            held: 0,
            kind,
        }
    }

    /// Charges every field in one authenticated node before typed decode.
    /// Charges accumulate until [`Self::release`] runs after node storage and
    /// transient decoded data have been dropped.
    pub fn preflight(
        &mut self,
        node: &CanonicalNodeView<'_>,
        budget: &mut usize,
    ) -> Result<(), StoreError> {
        self.preflight_mode(node, budget, false)
    }

    /// Reserves the simultaneous decode footprint of a whole leaf. Use this
    /// before bounded path-copy updates that retain every decoded leaf row;
    /// the streaming [`Self::preflight`] method only needs the largest row.
    pub fn preflight_update_node(
        &mut self,
        node: &CanonicalNodeView<'_>,
        budget: &mut usize,
    ) -> Result<(), StoreError> {
        self.preflight_mode(node, budget, true)
    }

    fn preflight_mode(
        &mut self,
        node: &CanonicalNodeView<'_>,
        budget: &mut usize,
        whole_leaf: bool,
    ) -> Result<(), StoreError> {
        let mut row_charge = 0usize;
        if node.is_leaf() {
            for field in node.leaf_field_iter().map_err(|_| StoreError::Corrupt)? {
                let (key, value) = field.map_err(|_| StoreError::Corrupt)?;
                check_key(key)?;
                let decode_bound = match self.kind {
                    Kind::Publication => {
                        if value.len() > MAX_PUBLICATION_VALUE_BYTES || !value.starts_with(b"PSP1")
                        {
                            return Err(StoreError::Corrupt);
                        }
                        value.len().checked_mul(2).ok_or(StoreError::Bounds)?
                    }
                    Kind::Capture => {
                        if value.len() > MAX_CAPTURE_VALUE_BYTES
                            || !(value.starts_with(b"PSC1")
                                || value.starts_with(b"PSC2")
                                || value.starts_with(b"PSC3"))
                        {
                            return Err(StoreError::Corrupt);
                        }
                        value.len().checked_mul(5).ok_or(StoreError::Bounds)?
                    }
                };
                let key_bound = key.len().checked_mul(4).ok_or(StoreError::Bounds)?;
                let payload = key_bound
                    .max(decode_bound)
                    .checked_add(MAX_DECODE_OVERHEAD)
                    .ok_or(StoreError::Bounds)?;
                let allocation_count = match self.kind {
                    Kind::Publication => MAX_DECODE_ALLOCATIONS,
                    Kind::Capture => MAX_CAPTURE_DECODE_ALLOCATIONS,
                };
                let allocations = allocation_count
                    .checked_mul(self.bookkeeping)
                    .ok_or(StoreError::Bounds)?;
                let transient = match self.kind {
                    Kind::Publication => 0,
                    Kind::Capture => MAX_CAPTURE_SCOPE_TRANSIENT_BYTES,
                };
                let one_row = payload
                    .checked_add(allocations)
                    .and_then(|n| n.checked_add(transient))
                    .ok_or(StoreError::Bounds)?;
                row_charge = if whole_leaf {
                    row_charge.checked_add(one_row).ok_or(StoreError::Bounds)?
                } else {
                    row_charge.max(one_row)
                };
            }
        } else {
            for child in node.branch_children().map_err(|_| StoreError::Corrupt)? {
                let child = child.map_err(|_| StoreError::Corrupt)?;
                check_key(child.first_key)?;
            }
        }
        let node_copies = node
            .encoded_len()
            .checked_mul(2)
            .and_then(|n| {
                self.bookkeeping
                    .checked_mul(2)
                    .and_then(|h| n.checked_add(h))
            })
            .ok_or(StoreError::Bounds)?;
        let charge = row_charge
            .checked_add(node_copies)
            .ok_or(StoreError::Bounds)?;
        let held = self.held.checked_add(charge).ok_or(StoreError::Bounds)?;
        let remaining = budget.checked_sub(charge).ok_or(StoreError::Bounds)?;
        self.held = held;
        *budget = remaining;
        Ok(())
    }

    /// Refunds temporary decode reservations after the guarded operation ends.
    pub fn release(&mut self, budget: &mut usize) -> Result<(), StoreError> {
        *budget = budget.checked_add(self.held).ok_or(StoreError::Bounds)?;
        self.held = 0;
        Ok(())
    }
}

fn check_key(bytes: &[u8]) -> Result<(), StoreError> {
    if bytes.len() > MAX_SEMANTIC_KEY_BYTES || bytes.len() < KEY_FIXED_BYTES {
        return Err(StoreError::Corrupt);
    }
    let (package_len, rest) = bytes.split_at_checked(4).ok_or(StoreError::Corrupt)?;
    let package_len =
        u32::from_be_bytes(package_len.try_into().map_err(|_| StoreError::Corrupt)?) as usize;
    if package_len > MAX_REFERENCE_BYTES || rest.len() < package_len + 4 {
        return Err(StoreError::Corrupt);
    }
    let (_, rest) = rest
        .split_at_checked(package_len)
        .ok_or(StoreError::Corrupt)?;
    let (coordinate_len, rest) = rest.split_at_checked(4).ok_or(StoreError::Corrupt)?;
    let coordinate_len =
        u32::from_be_bytes(coordinate_len.try_into().map_err(|_| StoreError::Corrupt)?) as usize;
    if coordinate_len > MAX_COORDINATE_BYTES
        || rest.len() != coordinate_len + 2 + 1
            && rest.len() != coordinate_len + 2 + 1 + backend_version::ID_BYTES
    {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}
