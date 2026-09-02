//! Defines identity behavior for the direct Clang semantic frontend of `compiler-driver`.
//! Interns libclang unified symbol resolution identities into typed fact coordinates inside
//! caller scratch. The USR is derived from source authority, so coordinates are stable within
//! an analysis across repeated declarations and identical spellings; absolute checkout paths
//! and arena ordinals never enter the identity.

use std::{ffi::CStr, path::Path};

use super::{
    error::ClangError,
    ffi,
    protocol::{FactId, FactRole, SemanticKind},
};

/// Minimum hash-table slots the interner can operate with.
pub(crate) const IDENTITY_MIN_TABLE_SLOTS: usize = 8;
/// Fixed byte width of one identity slot.
pub(crate) const IDENTITY_SLOT_BYTES: usize = 32;
/// Minimum USR bytes the pool must be able to hold.
pub(crate) const IDENTITY_MIN_USR_BYTES: usize = 1;

/// Slot byte offset of the occupancy cell.
const SLOT_OCCUPIED: usize = 0;
/// Slot byte offset of the role code.
const SLOT_ROLE: usize = 1;
/// Slot byte offset of the USR length.
const SLOT_USR_LEN: usize = 2;
/// Slot byte offset of the role-seeded FNV-1a hash.
const SLOT_HASH: usize = 6;
/// Slot byte offset of the dense ordinal.
const SLOT_ORDINAL: usize = 14;
/// Slot byte offset of the retained span start.
const SLOT_SPAN_START: usize = 18;
/// Slot byte offset of the retained span end.
const SLOT_SPAN_END: usize = 22;
/// Slot byte offset of the declaration kind code.
const SLOT_KIND: usize = 26;
/// Slot byte offset of the USR pool offset.
const SLOT_USR_OFFSET: usize = 27;

/// Interns USR identities into dense typed coordinates inside one caller scratch region.
pub(crate) struct IdentityInterner<'scratch> {
    scratch: &'scratch mut [u8],
    table_slots: usize,
    pool_start: usize,
    pool_used: usize,
    max_entries: usize,
    next_symbol_ordinal: u32,
    next_type_ordinal: u32,
    entries: usize,
    probes: usize,
    high_water: usize,
}

impl<'scratch> IdentityInterner<'scratch> {
    /// Computes the admitted table geometry for one scratch length.
    pub(crate) fn preflight(scratch_len: usize) -> Option<(usize, usize, usize)> {
        let max_slots = scratch_len / IDENTITY_SLOT_BYTES;
        let mut table_slots = IDENTITY_MIN_TABLE_SLOTS;
        while let Some(next) = table_slots.checked_mul(2) {
            if next > max_slots {
                break;
            }
            table_slots = next;
        }
        while table_slots >= IDENTITY_MIN_TABLE_SLOTS {
            let table_bytes = table_slots.checked_mul(IDENTITY_SLOT_BYTES)?;
            let pool_bytes = scratch_len.checked_sub(table_bytes)?;
            let pool_entries = pool_bytes / IDENTITY_MIN_USR_BYTES;
            let max_entries = (table_slots / 2).min(pool_entries);
            if max_entries >= IDENTITY_MIN_USR_BYTES {
                return Some((table_slots, table_bytes, max_entries));
            }
            table_slots /= 2;
        }
        None
    }

    /// Partitions one scratch region into an empty interner or reports the exact shortfall.
    pub(crate) fn new(scratch: &'scratch mut [u8]) -> Result<Self, (usize, usize)> {
        let Some((table_slots, pool_start, max_entries)) = Self::preflight(scratch.len()) else {
            return Err((
                scratch.len(),
                IDENTITY_MIN_TABLE_SLOTS * IDENTITY_SLOT_BYTES + IDENTITY_MIN_USR_BYTES,
            ));
        };
        scratch[..pool_start].fill(0);
        Ok(Self {
            scratch,
            table_slots,
            pool_start,
            pool_used: 0,
            max_entries,
            next_symbol_ordinal: 0,
            next_type_ordinal: 0,
            entries: 0,
            probes: 0,
            high_water: pool_start,
        })
    }

    /// Interns the cursor's USR under the closed role, returning its coordinate and the
    /// declaration span retained for the identity.
    pub(crate) fn intern<'input>(
        &mut self,
        cursor: ffi::CxCursor,
        role: FactRole,
        kind: Option<SemanticKind>,
        span: super::protocol::ClangSourceSpan,
        source_name: &'input Path,
    ) -> Result<(FactId, super::protocol::ClangSourceSpan), ClangError<'input>> {
        // SAFETY: the cursor belongs to the live translation unit and the returned string is
        // disposed exactly once on every path below.
        let canonical = unsafe { ffi::clang_get_canonical_cursor(cursor) };
        let string = unsafe { ffi::clang_get_cursor_usr(canonical) };
        // SAFETY: the string is live for this borrow and disposed at the end of the block.
        let bytes = {
            let pointer = unsafe { ffi::clang_get_c_string(string) };
            if pointer.is_null() {
                &[][..]
            } else {
                // SAFETY: libclang returns a NUL-terminated string for a live handle.
                unsafe { CStr::from_ptr(pointer) }.to_bytes()
            }
        };
        if bytes.is_empty() {
            // SAFETY: the string was live and is disposed exactly once here.
            unsafe { ffi::clang_dispose_string(string) };
            return Err(ClangError::LibclangIdentityUnavailable);
        }
        let hash = fnv1a_with_role(role, bytes);
        let result = self.intern_key(role, hash, bytes, kind, span, source_name);
        // SAFETY: the string was live and is disposed exactly once here.
        unsafe { ffi::clang_dispose_string(string) };
        result
    }

    /// Projects a traversal-scratch-relative offset into the whole caller scratch extent.
    pub(crate) fn scratch_offset(&self, relative: usize) -> Option<usize> {
        self.scratch.len().checked_add(relative)
    }

    /// Exact admitted identity capacity.
    pub(crate) fn max_entries(&self) -> usize {
        self.max_entries
    }

    /// Exact hash-probe work performed so far.
    pub(crate) fn probes(&self) -> usize {
        self.probes
    }

    /// Exact identity scratch high-water mark.
    pub(crate) fn high_water(&self) -> usize {
        self.high_water
    }

    fn intern_key<'input>(
        &mut self,
        role: FactRole,
        hash: u64,
        usr: &[u8],
        kind: Option<SemanticKind>,
        span: super::protocol::ClangSourceSpan,
        source_name: &'input Path,
    ) -> Result<(FactId, super::protocol::ClangSourceSpan), ClangError<'input>> {
        let usr_len =
            u32::try_from(usr.len()).map_err(|_| ClangError::LibclangIdentityUnavailable)?;
        let start = (hash as usize) % self.table_slots;
        for step in 0..self.table_slots {
            let index = (start + step) % self.table_slots;
            self.probes = self
                .probes
                .checked_add(1)
                .ok_or(ClangError::FactCountOverflow)?;
            let offset = index * IDENTITY_SLOT_BYTES;
            if self.scratch[offset + SLOT_OCCUPIED] == 0 {
                let observed = self
                    .entries
                    .checked_add(1)
                    .ok_or(ClangError::FactCountOverflow)?;
                if self.entries >= self.max_entries {
                    return Err(ClangError::BindingCapacity {
                        limit: self.max_entries,
                        observed,
                    });
                }
                let usr_offset = self
                    .pool_start
                    .checked_add(self.pool_used)
                    .ok_or(ClangError::FactCountOverflow)?;
                let pool_end = usr_offset
                    .checked_add(usr.len())
                    .ok_or(ClangError::FactCountOverflow)?;
                if pool_end > self.scratch.len() {
                    return Err(ClangError::LibclangIdentityScratchTooSmall {
                        source_name,
                        provided: self.scratch.len(),
                        required: pool_end,
                    });
                }
                let usr_offset_u32 =
                    u32::try_from(usr_offset).map_err(|_| ClangError::FactCountOverflow)?;
                let ordinal = self
                    .allocate_ordinal(role)
                    .ok_or(ClangError::BindingCapacity {
                        limit: self.max_entries,
                        observed,
                    })?;
                self.entries = observed;
                self.scratch[usr_offset..pool_end].copy_from_slice(usr);
                self.pool_used = pool_end - self.pool_start;
                self.high_water = self.high_water.max(pool_end);
                self.write_slot(
                    offset,
                    IdentitySlot {
                        role,
                        hash,
                        usr_offset: usr_offset_u32,
                        usr_len,
                        ordinal,
                        span,
                        kind,
                    },
                );
                return Ok((FactId { role, ordinal }, span));
            }
            if self.same_key(offset, role, hash, usr) {
                let existing = read_span(
                    &self.scratch[offset..offset + IDENTITY_SLOT_BYTES],
                    SLOT_SPAN_START,
                );
                if let Some(declaration_kind) = kind {
                    self.scratch[offset + SLOT_SPAN_START..offset + SLOT_SPAN_START + 4]
                        .copy_from_slice(&span.start.to_le_bytes());
                    self.scratch[offset + SLOT_SPAN_END..offset + SLOT_SPAN_END + 4]
                        .copy_from_slice(&span.end.to_le_bytes());
                    self.scratch[offset + SLOT_KIND] = kind_code(declaration_kind);
                }
                let ordinal = read_u32(
                    &self.scratch[offset..offset + IDENTITY_SLOT_BYTES],
                    SLOT_ORDINAL,
                );
                return Ok((
                    FactId { role, ordinal },
                    if kind.is_some() { span } else { existing },
                ));
            }
        }
        let observed = self
            .entries
            .checked_add(1)
            .ok_or(ClangError::FactCountOverflow)?;
        Err(ClangError::BindingCapacity {
            limit: self.max_entries,
            observed,
        })
    }

    fn allocate_ordinal(&mut self, role: FactRole) -> Option<u32> {
        let next = match role {
            FactRole::Symbol => &mut self.next_symbol_ordinal,
            FactRole::Type => &mut self.next_type_ordinal,
        };
        let ordinal = *next;
        *next = (*next).checked_add(1)?;
        Some(ordinal)
    }

    fn write_slot(&mut self, offset: usize, slot: IdentitySlot) {
        let record = &mut self.scratch[offset..offset + IDENTITY_SLOT_BYTES];
        record[SLOT_OCCUPIED] = 1;
        record[SLOT_ROLE] = slot.role.code();
        record[SLOT_USR_LEN..SLOT_USR_LEN + 4].copy_from_slice(&slot.usr_len.to_le_bytes());
        record[SLOT_HASH..SLOT_HASH + 8].copy_from_slice(&slot.hash.to_le_bytes());
        record[SLOT_ORDINAL..SLOT_ORDINAL + 4].copy_from_slice(&slot.ordinal.to_le_bytes());
        write_span(record, SLOT_SPAN_START, slot.span);
        record[SLOT_KIND] = slot.kind.map_or(0, kind_code);
        record[SLOT_USR_OFFSET..SLOT_USR_OFFSET + 4]
            .copy_from_slice(&slot.usr_offset.to_le_bytes());
    }

    fn same_key(&self, offset: usize, role: FactRole, hash: u64, usr: &[u8]) -> bool {
        let record = &self.scratch[offset..offset + IDENTITY_SLOT_BYTES];
        if record[SLOT_ROLE] != role.code()
            || read_u64(record, SLOT_HASH) != hash
            || usize::try_from(read_u32(record, SLOT_USR_LEN)).ok() != Some(usr.len())
        {
            return false;
        }
        let usr_offset = read_u32(record, SLOT_USR_OFFSET) as usize;
        let Some(end) = usr_offset.checked_add(usr.len()) else {
            return false;
        };
        self.scratch
            .get(usr_offset..end)
            .is_some_and(|stored| stored == usr)
    }
}

struct IdentitySlot {
    role: FactRole,
    hash: u64,
    usr_offset: u32,
    usr_len: u32,
    ordinal: u32,
    span: super::protocol::ClangSourceSpan,
    kind: Option<SemanticKind>,
}

/// Stable one-byte journal code for a canonical semantic kind.
pub(crate) fn kind_code(kind: SemanticKind) -> u8 {
    u16::from(kind) as u8
}

/// Writes one span into a record at the named offset.
pub(crate) fn write_span(record: &mut [u8], offset: usize, span: super::protocol::ClangSourceSpan) {
    record[offset..offset + 4].copy_from_slice(&span.start.to_le_bytes());
    record[offset + 4..offset + 8].copy_from_slice(&span.end.to_le_bytes());
}

/// Reads one span from a record at the named offset.
pub(crate) fn read_span(record: &[u8], offset: usize) -> super::protocol::ClangSourceSpan {
    super::protocol::ClangSourceSpan {
        start: read_u32(record, offset),
        end: read_u32(record, offset + 4),
    }
}

/// Reads a little-endian `u32` from a record at the named offset.
pub(crate) fn read_u32(record: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        record[offset],
        record[offset + 1],
        record[offset + 2],
        record[offset + 3],
    ])
}

/// Reads a little-endian `u64` from a record at the named offset.
pub(crate) fn read_u64(record: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        record[offset],
        record[offset + 1],
        record[offset + 2],
        record[offset + 3],
        record[offset + 4],
        record[offset + 5],
        record[offset + 6],
        record[offset + 7],
    ])
}

/// FNV-1a over the USR bytes seeded by the closed identity role.
fn fnv1a_with_role(role: FactRole, bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    hash ^= u64::from(role.code());
    hash = hash.wrapping_mul(0x100000001b3);
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
