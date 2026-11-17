//! Narrow preflight for the empty-project source relation used by native retirement.
//!
//! This intentionally refuses file rows, membership pages, Cargo aliases and
//! historical project encodings. Those require a separate allocation proof.
use super::relation::{ProductSourceRecord, ProductSourceRelation};
use crate::workspace::{WorkspacePreflightRelationHandle, WorkspaceRelationError};
use backend_store::StoreError;
use crate::workspace::retirement_budget::{Metered, RetirementBudget, RetirementReservation};

const MAX_RETIREMENT_NODE_LOADS: usize = 4096;

/// Clone the supported empty Project only after reserving its retained heap.
/// The empty membership Arc is charged even though cloning shares it, so the
/// clone remains accounted for if its predecessor is dropped first.
/// This provisional helper does not prevent an unmetered clone through
/// `Metered::get`; retirement admission must encapsulate it before use.
pub fn clone_empty_source_project_retirement(
    record: &ProductSourceRecord,
    budget: &RetirementBudget,
    bookkeeping: usize,
) -> Result<Metered<ProductSourceRecord>, StoreError> {
    use super::relation::ProductProjectMembership;
    let ProductSourceRecord::Project { label, files: ProductProjectMembership::Inline(files), cargo_aliases: None, source_selection, .. } = record else {
        return Err(StoreError::NeedsScopedRebuild);
    };
    if label.is_empty() || label.len() > ProductSourceRecord::MAX_LABEL_BYTES || !files.is_empty() {
        return Err(StoreError::NeedsScopedRebuild);
    }
    let mut bytes = label.len().checked_add(2 * std::mem::size_of::<usize>()).ok_or(StoreError::Bounds)?;
    let mut allocations = 2usize; // label and shared empty membership Arc
    if let Some(selection) = source_selection {
        let rows = &selection.unsupported_extensions;
        if selection.observed_projects != 1 || selection.unknown_projects != 0 || rows.len() != 3
            || rows[0].extension != "css" || rows[1].extension != "sql" || rows[2].extension != "vue" {
            return Err(StoreError::NeedsScopedRebuild);
        }
        // Account for old/new slice allocations if Vec-to-box conversion moves.
        let slots = rows.len().checked_mul(std::mem::size_of::<backend_library::SourceExtensionExclusion>())
            .and_then(|n| n.checked_mul(2)).ok_or(StoreError::Bounds)?;
        bytes = bytes.checked_add(slots).ok_or(StoreError::Bounds)?;
        for row in rows.iter() { bytes = bytes.checked_add(row.extension.len()).ok_or(StoreError::Bounds)?; }
        allocations = allocations.checked_add(5).ok_or(StoreError::Bounds)?;
    }
    bytes = bytes.checked_add(allocations.checked_mul(bookkeeping).ok_or(StoreError::Bounds)?).ok_or(StoreError::Bounds)?;
    let lease = budget.reserve(bytes).map_err(|_| StoreError::Bounds)?;
    Ok(lease.meter(record.clone()))
}

/// Ledger-backed gate for the narrow empty-project schema. All node charges
/// are retained until the read or path-copy has dropped its decoded nodes.
pub struct EmptySourceRetirementReservations<'a> {
    leases: Vec<RetirementReservation>,
    _table: RetirementReservation,
    gate: EmptySourceNodeReservations<'a>,
}

impl<'a> EmptySourceRetirementReservations<'a> {
    pub fn new(budget: &RetirementBudget, cancelled: &'a dyn Fn() -> bool, bookkeeping: usize, whole_node: bool) -> Result<Self, StoreError> {
        let bytes = MAX_RETIREMENT_NODE_LOADS.checked_mul(std::mem::size_of::<RetirementReservation>())
            .and_then(|n| n.checked_add(bookkeeping)).ok_or(StoreError::Bounds)?;
        let table = budget.reserve(bytes).map_err(|_| StoreError::Bounds)?;
        let leases = Vec::with_capacity(MAX_RETIREMENT_NODE_LOADS);
        Ok(Self { leases, _table: table, gate: EmptySourceNodeReservations::new(cancelled, bookkeeping, whole_node) })
    }

    pub fn preflight(&mut self, node: &backend_version::CanonicalNodeView<'_>, budget: &RetirementBudget) -> Result<(), StoreError> {
        if self.leases.len() == MAX_RETIREMENT_NODE_LOADS { return Err(StoreError::Bounds); }
        let bytes = self.gate.node_charge(node)?;
        self.leases.push(budget.reserve(bytes).map_err(|_| StoreError::Bounds)?);
        Ok(())
    }
}

pub fn source_visit_retirement<V>(
    relation: &WorkspacePreflightRelationHandle<ProductSourceRelation>,
    after: Option<&[u8; 32]>, limit: usize, budget: &RetirementBudget,
    bookkeeping: usize, cancelled: &dyn Fn() -> bool, visit: &mut V,
) -> Result<Metered<Option<[u8; 32]>>, WorkspaceRelationError>
where V: FnMut(&[u8; 32], ProductSourceRecord, &RetirementBudget) -> Result<(), StoreError>,
{
    let mut gate = EmptySourceRetirementReservations::new(budget, cancelled, bookkeeping, false).map_err(WorkspaceRelationError::Store)?;
    relation.visit_retirement(after, limit, 32, budget, 0,
        &mut |node, budget| gate.preflight(node, budget),
        &mut |key, row, budget| {
            if cancelled() { return Err(StoreError::Bounds); }
            visit(key, row, budget)
        })
}

// Only the producer's current fixed selection schema is admitted. Inspecting
// bytes first keeps serde away from arbitrary arrays, strings and nesting.
fn selection(mut s: &[u8]) -> bool {
    fn take(s: &mut &[u8], prefix: &[u8]) -> bool {
        if let Some(rest) = s.strip_prefix(prefix) {
            *s = rest;
            true
        } else {
            false
        }
    }
    fn number(s: &mut &[u8]) -> bool {
        let n = s.iter().take_while(|b| b.is_ascii_digit()).count();
        if n == 0 || n > 20 || (n > 1 && s[0] == b'0') {
            return false;
        }
        if std::str::from_utf8(&s[..n])
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .is_none()
        {
            return false;
        }
        *s = &s[n..];
        true
    }
    take(&mut s, b"{\"observed_projects\":1,\"unknown_projects\":0,\"unsupported_extensions\":[{\"extension\":\"css\",\"files\":")
        && number(&mut s) && take(&mut s, b"},{\"extension\":\"sql\",\"files\":")
        && number(&mut s) && take(&mut s, b"},{\"extension\":\"vue\",\"files\":")
        && number(&mut s) && take(&mut s, b"}]}") && s.is_empty()
}

// Return label length only after validating all fields which control an
// allocation in this narrow schema, including the zero membership count.
fn empty_project(mut bytes: &[u8]) -> Option<(usize, usize)> {
    let mut json_len = 0;
    if bytes.starts_with(b"PSRI") {
        let len = u32::from_be_bytes(bytes.get(4..8)?.try_into().ok()?) as usize;
        if len > 2048 || !selection(bytes.get(8..8usize.checked_add(len)?)?) {
            return None;
        }
        json_len = len;
        bytes = bytes.get(8usize.checked_add(len)?..)?;
    }
    if bytes.get(..5)? != b"PSR8\x01" {
        return None;
    }
    let len = u32::from_be_bytes(bytes.get(5..9)?.try_into().ok()?) as usize;
    if len == 0 || len > ProductSourceRecord::MAX_LABEL_BYTES {
        return None;
    }
    let end = 9usize.checked_add(len)?;
    std::str::from_utf8(bytes.get(9..end)?).ok()?;
    let rest = bytes.get(end..)?;
    if rest.len() != 36 || rest.get(32..)? != [0, 0, 0, 0] {
        return None;
    }
    Some((len, json_len))
}

/// Borrowed validation before decoding an untrusted V17 project after-image.
pub fn empty_source_project_wire_admitted(bytes: &[u8]) -> bool {
    empty_project(bytes).is_some()
}

/// Conservative decode/re-encode peak for one admitted empty Project row.
/// This provisional formula must be independently reviewed before authority.
pub fn empty_source_project_decode_charge(value: &[u8], bookkeeping: usize) -> Result<usize, StoreError> {
    let (label, json) = empty_project(value).ok_or(StoreError::NeedsScopedRebuild)?;
    // Rust 1.97.1 RawVec::grow_amortized requests max(2*old,
    // required, minimum). On a moving realloc, both old and new
    // layouts can coexist: three times the final logical length
    // bounds their sum. The PSRI encoder writes 8 wrapper bytes,
    // the selection JSON, and 45 bytes plus label for PSR8.
    // The PSRI decoder retains its canonical inner encoding
    // through with_source_selection, so reserve that Vec too.
    // Account for both original and cloned labels, a moving output
    // buffer, a moving serde_json buffer (initial capacity 128),
    // the original serde sequence (4 slots), its boxed conversion
    // (3), and its clone (3). Three fixed 3-byte extension names
    // can coexist in each observation. Empty membership uses an
    // Arc allocation containing only its two reference counts.
    // Counts below include those allocations and their possible
    // realloc predecessors; bookkeeping is a separate caller policy.
    let output_len = label
        .checked_add(json)
        .and_then(|n| n.checked_add(53))
        .ok_or(StoreError::Bounds)?;
    let output_peak = output_len.max(8).checked_mul(3).ok_or(StoreError::Bounds)?;
    let json_peak = json.max(128).checked_mul(3).ok_or(StoreError::Bounds)?;
    let legacy = label
        .checked_add(45)
        .and_then(|n| n.checked_mul(2))
        .ok_or(StoreError::Bounds)?;
    let observations = 10usize
        .checked_mul(std::mem::size_of::<backend_library::SourceExtensionExclusion>())
        .and_then(|n| n.checked_add(18))
        .ok_or(StoreError::Bounds)?;
    let arc = 2usize
        .checked_mul(std::mem::size_of::<usize>())
        .ok_or(StoreError::Bounds)?;
    let row = label
        .checked_mul(2)
        .and_then(|n| n.checked_add(output_peak))
        .and_then(|n| n.checked_add(json_peak))
        .and_then(|n| n.checked_add(legacy))
        .and_then(|n| n.checked_add(observations))
        .and_then(|n| n.checked_add(arc))
        .and_then(|n| {
            bookkeeping
                .checked_mul(32)
                .and_then(|h| n.checked_add(h))
        })
        .ok_or(StoreError::Bounds)?;
    Ok(row)
}

/// Holds temporary allocations until all checked nodes have been released.
/// Set `whole_leaf` when an update decodes all leaf rows simultaneously.
pub struct EmptySourceNodeReservations<'a> {
    cancelled: &'a dyn Fn() -> bool,
    bookkeeping: usize,
    held: usize,
    whole_leaf: bool,
}
impl<'a> EmptySourceNodeReservations<'a> {
    /// Establish a shared-budget gate; caller must refund only after node data drops.
    pub const fn new(cancelled: &'a dyn Fn() -> bool, bookkeeping: usize, whole_leaf: bool) -> Self {
        Self {
            cancelled,
            bookkeeping,
            held: 0,
            whole_leaf,
        }
    }
    /// Validate every leaf value before the typed decoder sees the node.
    pub fn preflight(
        &mut self,
        node: &backend_version::CanonicalNodeView<'_>,
        budget: &mut usize,
    ) -> Result<(), StoreError> {
        let charge = self.node_charge(node)?;
        let held = self.held.checked_add(charge).ok_or(StoreError::Bounds)?;
        *budget = budget.checked_sub(charge).ok_or(StoreError::Bounds)?;
        self.held = held;
        Ok(())
    }

    fn node_charge(&self, node: &backend_version::CanonicalNodeView<'_>) -> Result<usize, StoreError> {
        if (self.cancelled)() {
            return Err(StoreError::Bounds);
        }
        let mut rows = 0usize;
        if node.is_leaf() {
            for entry in node.leaf_field_iter().map_err(|_| StoreError::Corrupt)? {
                let (key, value) = entry.map_err(|_| StoreError::Corrupt)?;
                if key.len() != 32 {
                    return Err(StoreError::Corrupt);
                }
                // A well-formed row may simply be outside this narrow proof.
                let row = empty_source_project_decode_charge(value, self.bookkeeping)?;
                rows = if self.whole_leaf {
                    rows.checked_add(row).ok_or(StoreError::Bounds)?
                } else {
                    rows.max(row)
                };
            }
        } else {
            for child in node.branch_children().map_err(|_| StoreError::Corrupt)? {
                if child.map_err(|_| StoreError::Corrupt)?.first_key.len() != 32 {
                    return Err(StoreError::Corrupt);
                }
                if self.whole_leaf {
                    // Whole-node updates may retain all children while a
                    // growing vector reallocates. Include old and new slots.
                    let child_bytes = std::mem::size_of::<backend_version::CommittedChild<ProductSourceRelation>>()
                        .checked_mul(3).ok_or(StoreError::Bounds)?;
                    rows = rows.checked_add(child_bytes).ok_or(StoreError::Bounds)?;
                }
            }
        }
        let copies = node
            .encoded_len()
            .checked_mul(2)
            .and_then(|n| {
                self.bookkeeping
                    .checked_mul(2)
                    .and_then(|h| n.checked_add(h))
            })
            .ok_or(StoreError::Bounds)?;
        let key = 32usize
            .checked_add(self.bookkeeping)
            .ok_or(StoreError::Bounds)?;
        rows.max(key).checked_add(copies).ok_or(StoreError::Bounds)
    }
    /// Refund temporary reservations after the operation and all nodes end.
    pub fn release(&mut self, budget: &mut usize) -> Result<(), StoreError> {
        *budget = budget.checked_add(self.held).ok_or(StoreError::Bounds)?;
        self.held = 0;
        Ok(())
    }
}

/// Stream checked empty projects; the visitor separately charges retained rows.
pub fn source_visit_preflighted<V>(
    relation: &WorkspacePreflightRelationHandle<ProductSourceRelation>,
    after: Option<&[u8; 32]>,
    limit: usize,
    budget: &mut usize,
    bookkeeping: usize,
    cancelled: &dyn Fn() -> bool,
    visit: &mut V,
) -> Result<Option<[u8; 32]>, WorkspaceRelationError>
where
    V: FnMut(&[u8; 32], ProductSourceRecord, &mut usize) -> Result<(), StoreError>,
{
    let mut gate = EmptySourceNodeReservations::new(cancelled, bookkeeping, false);
    let result = relation.visit(
        after,
        limit,
        32,
        budget,
        &mut |node, budget| gate.preflight(node, budget),
        &mut |key, row, budget| {
            if cancelled() {
                return Err(StoreError::Bounds);
            }
            visit(key, row, budget)
        },
    );
    gate.release(budget)
        .map_err(WorkspaceRelationError::Store)?;
    result
}
