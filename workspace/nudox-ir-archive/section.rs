//! Section building and reading helpers.
//!
//! # StringTable
//!
//! The `StringTable` is a pair of:
//!   - an array of LE u32 byte offsets (one per interned string), and
//!   - a dense UTF-8 blob.
//!
//! [`StrId`] is a zero-based index into the offset array. Resolve by reading
//! `blob[offsets[id]..offsets[id+1]]` (the last entry's end is `blob.len()`).
//!
//! The builder interns strings in insertion order, deduplicating via a
//! HashMap.  The reader borrows the raw bytes without allocation.
//!
//! # Section I/O
//!
//! [`write_section`] serialises `(section_id, bytes)` → a pair of bytes
//! suitable for later concatenation. [`crc32_of`] computes the CRC32 used in
//! [`crate::header::TocEntry`].
//!
//! # CSR
//!
//! [`CsrBuilder`] accumulates `(row, value: u32)` pairs and emits the standard
//! CSR (Compressed Sparse Row) format: an offset array of length `(nrows + 1)`
//! followed by a values array.  [`CsrView`] reads a CSR back from bytes.

use std::collections::HashMap;

use nudox_ir::index::StrId;

use crate::error::ArchiveError;

// ---------------------------------------------------------------------------
// CRC32 helper (Castagnoli — standard for file integrity)
// ---------------------------------------------------------------------------

/// Compute CRC32C (Castagnoli) over `bytes`.
pub fn crc32_of(bytes: &[u8]) -> u32 {
    // Use the well-known IEEE polynomial since the `crc32fast` crate is not
    // listed as a dependency. We use a simple software implementation.
    //
    // NOTE: we use the standard Ethernet/IEEE 802.3 polynomial (0xEDB88320
    // reflected) to avoid adding a new crate dep. This is the same polynomial
    // that e.g. zip/gzip use, so tooling can verify archives externally.
    crc32_ieee(bytes)
}

fn crc32_ieee(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        let idx = ((crc ^ byte as u32) & 0xFF) as usize;
        crc = CRC32_TABLE[idx] ^ (crc >> 8);
    }
    crc ^ 0xFFFF_FFFF
}

/// Pre-computed IEEE CRC32 lookup table.
static CRC32_TABLE: [u32; 256] = {
    let poly: u32 = 0xEDB8_8320;
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ poly;
            } else {
                crc >>= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

// ---------------------------------------------------------------------------
// StringTable builder
// ---------------------------------------------------------------------------

/// Incrementally builds the StringTable section, deduplicating strings.
///
/// Strings are interned in deterministic (insertion) order; callers should
/// intern in a defined order (e.g., entry-sorted by IntroId) to ensure
/// determinism.
pub struct StringTableBuilder {
    /// Maps `String → StrId`.
    map: HashMap<String, StrId>,
    /// Byte offsets into the blob, one per string.
    offsets: Vec<u32>,
    /// UTF-8 blob.
    blob: Vec<u8>,
}

impl StringTableBuilder {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            offsets: Vec::new(),
            blob: Vec::new(),
        }
    }

    /// Intern `s`, returning its [`StrId`].  Duplicate calls return the same id.
    pub fn intern(&mut self, s: &str) -> StrId {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = StrId(self.offsets.len() as u32);
        let offset = self.blob.len() as u32;
        self.offsets.push(offset);
        self.blob.extend_from_slice(s.as_bytes());
        self.map.insert(s.to_owned(), id);
        id
    }

    /// Number of interned strings.
    pub fn len(&self) -> u32 {
        self.offsets.len() as u32
    }

    /// True if no strings have been interned.
    pub fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    /// Encode the StringTable as bytes:
    ///   `u32le(n_strings) || u32le(offsets[0]) || … || u32le(offsets[n-1]) || blob`
    pub fn into_bytes(self) -> Vec<u8> {
        let n = self.offsets.len() as u32;
        let mut out = Vec::with_capacity(4 + self.offsets.len() * 4 + self.blob.len());
        out.extend_from_slice(&n.to_le_bytes());
        for off in &self.offsets {
            out.extend_from_slice(&off.to_le_bytes());
        }
        out.extend_from_slice(&self.blob);
        out
    }
}

impl Default for StringTableBuilder {
    fn default() -> Self { Self::new() }
}

// ---------------------------------------------------------------------------
// StringTableView — zero-alloc reader
// ---------------------------------------------------------------------------

/// Borrows an encoded StringTable section and resolves [`StrId`]→`&str` with
/// zero copies.
#[derive(Clone, Copy)]
pub struct StringTableView<'a> {
    offsets: &'a [[u8; 4]],   // n_strings LE u32 offsets (each 4 bytes)
    blob: &'a [u8],
    n: u32,
}

impl<'a> StringTableView<'a> {
    /// Parse a StringTable from raw section bytes.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ArchiveError> {
        if bytes.len() < 4 {
            return Err(ArchiveError::Truncated);
        }
        let n = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let offsets_end = 4 + n * 4;
        if bytes.len() < offsets_end {
            return Err(ArchiveError::Truncated);
        }
        // Reinterpret the offsets as `[[u8; 4]]` slices.
        let offsets_bytes = &bytes[4..offsets_end];
        // SAFETY: &[u8] is valid to reinterpret as &[[u8;4]] if len % 4 == 0.
        let offsets: &'a [[u8; 4]] = bytemuck_cast_slice(offsets_bytes);
        let blob = &bytes[offsets_end..];
        Ok(Self { offsets, blob, n: n as u32 })
    }

    /// Resolve a [`StrId`] to a `&str`, borrowing from the section bytes.
    pub fn resolve(&self, id: StrId) -> Result<&'a str, ArchiveError> {
        let idx = id.0 as usize;
        if idx >= self.n as usize {
            return Err(ArchiveError::Truncated);
        }
        let start = u32::from_le_bytes(self.offsets[idx]) as usize;
        let end = if idx + 1 < self.n as usize {
            u32::from_le_bytes(self.offsets[idx + 1]) as usize
        } else {
            self.blob.len()
        };
        if end > self.blob.len() || start > end {
            return Err(ArchiveError::Truncated);
        }
        std::str::from_utf8(&self.blob[start..end])
            .map_err(|_| ArchiveError::Truncated)
    }

    /// Total number of interned strings.
    pub fn len(&self) -> u32 { self.n }

    /// True if the table holds no strings.
    pub fn is_empty(&self) -> bool { self.n == 0 }
}

// ---------------------------------------------------------------------------
// Bytemuck-style cast helper (avoids adding bytemuck dep)
// ---------------------------------------------------------------------------

/// Reinterpret `&[u8]` as `&[[u8; 4]]`. Panics if `bytes.len() % 4 != 0`.
fn bytemuck_cast_slice(bytes: &[u8]) -> &[[u8; 4]] {
    assert_eq!(bytes.len() % 4, 0, "slice length must be a multiple of 4");
    // SAFETY: &[u8] is a valid byte representation for &[[u8; 4]] when aligned
    // to 1 (which &[u8] always is) and the length is a multiple of 4.
    unsafe {
        std::slice::from_raw_parts(bytes.as_ptr() as *const [u8; 4], bytes.len() / 4)
    }
}

// ---------------------------------------------------------------------------
// CSR builder
// ---------------------------------------------------------------------------

/// Builds a Compressed Sparse Row (CSR) structure for tree/link adjacency.
///
/// Call [`add`](CsrBuilder::add) with `(row, value)` pairs in any order, then
/// [`finish`](CsrBuilder::finish) to emit the canonical byte encoding.
///
/// Wire encoding:
///   `u32le(nrows) || u32le(offsets[0]) .. u32le(offsets[nrows]) || u32le(vals[0]) ..`
///
/// `offsets` has length `nrows + 1`; `offsets[nrows] == vals.len()`.
pub struct CsrBuilder {
    nrows: u32,
    /// Unsorted `(row, value)` pairs; sorted on [`finish`].
    pairs: Vec<(u32, u32)>,
}

impl CsrBuilder {
    pub fn new(nrows: u32) -> Self {
        Self { nrows, pairs: Vec::new() }
    }

    /// Add an edge `row → value`.
    pub fn add(&mut self, row: u32, value: u32) {
        self.pairs.push((row, value));
    }

    /// Emit the canonical sorted-CSR bytes.
    pub fn finish(mut self) -> Vec<u8> {
        // Sort by (row, value) for determinism.
        self.pairs.sort_unstable();

        let nrows = self.nrows as usize;
        // Build offset table.
        let mut offsets = vec![0u32; nrows + 1];
        for &(row, _) in &self.pairs {
            if (row as usize) < nrows {
                offsets[row as usize + 1] += 1;
            }
        }
        // Prefix-sum.
        for i in 1..=nrows {
            offsets[i] += offsets[i - 1];
        }

        let nvals = self.pairs.len();
        let mut out = Vec::with_capacity(4 + (nrows + 1) * 4 + nvals * 4);
        out.extend_from_slice(&(nrows as u32).to_le_bytes());
        for off in &offsets {
            out.extend_from_slice(&off.to_le_bytes());
        }
        for &(_, val) in &self.pairs {
            out.extend_from_slice(&val.to_le_bytes());
        }
        out
    }
}

// ---------------------------------------------------------------------------
// CsrView — zero-alloc reader
// ---------------------------------------------------------------------------

/// Zero-copy reader for a CSR section.
#[derive(Clone, Copy)]
pub struct CsrView<'a> {
    nrows: u32,
    offsets: &'a [[u8; 4]], // length nrows+1
    values: &'a [[u8; 4]],
}

impl<'a> CsrView<'a> {
    /// Parse a CSR section from raw bytes.
    pub fn from_bytes(bytes: &'a [u8]) -> Result<Self, ArchiveError> {
        if bytes.len() < 4 {
            return Err(ArchiveError::Truncated);
        }
        let nrows = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let offsets_end = 4 + (nrows + 1) * 4;
        if bytes.len() < offsets_end {
            return Err(ArchiveError::Truncated);
        }
        let offsets = bytemuck_cast_slice(&bytes[4..offsets_end]);
        let values_bytes = &bytes[offsets_end..];
        if !values_bytes.len().is_multiple_of(4) {
            return Err(ArchiveError::Truncated);
        }
        let values = bytemuck_cast_slice(values_bytes);
        Ok(Self { nrows: nrows as u32, offsets, values })
    }

    /// Return the raw LE-encoded value words for `row`.
    ///
    /// This returns `&[[u8; 4]]` (alignment 1), NOT `&[u32]` (alignment 4):
    /// the archive is a `[u8]`-backed buffer with no 4-byte alignment guarantee,
    /// so reinterpreting a sub-slice as `&[u32]` would be undefined behavior.
    /// Callers decode each word with `u32::from_le_bytes`.
    pub fn row(&self, row: u32) -> Result<&'a [[u8; 4]], ArchiveError> {
        if row >= self.nrows {
            return Err(ArchiveError::IndexOutOfRange(row, self.nrows));
        }
        let start = u32::from_le_bytes(self.offsets[row as usize]) as usize;
        let end = u32::from_le_bytes(self.offsets[row as usize + 1]) as usize;
        if end > self.values.len() || start > end {
            return Err(ArchiveError::Truncated);
        }
        Ok(&self.values[start..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_table_roundtrip() {
        let mut builder = StringTableBuilder::new();
        let id_a = builder.intern("hello");
        let id_b = builder.intern("world");
        let id_a2 = builder.intern("hello"); // deduplicated
        assert_eq!(id_a, id_a2);
        assert_ne!(id_a, id_b);
        assert_eq!(builder.len(), 2);

        let bytes = builder.into_bytes();
        let view = StringTableView::from_bytes(&bytes).unwrap();
        assert_eq!(view.resolve(id_a).unwrap(), "hello");
        assert_eq!(view.resolve(id_b).unwrap(), "world");
    }

    #[test]
    fn csr_roundtrip() {
        let mut b = CsrBuilder::new(4);
        b.add(0, 10);
        b.add(0, 20);
        b.add(2, 30);
        b.add(3, 40);
        let bytes = b.finish();
        let view = CsrView::from_bytes(&bytes).unwrap();
        let decode = |r: u32| -> Vec<u32> {
            view.row(r).unwrap().iter().map(|w| u32::from_le_bytes(*w)).collect()
        };
        assert_eq!(decode(0), vec![10u32, 20u32]);
        assert_eq!(decode(1), Vec::<u32>::new());
        assert_eq!(decode(2), vec![30u32]);
        assert_eq!(decode(3), vec![40u32]);
    }

    #[test]
    fn crc32_stable() {
        // Known CRC32 of b"hello" = 0x3610A686
        let c = crc32_of(b"hello");
        assert_eq!(c, 0x3610_A686);
    }
}
