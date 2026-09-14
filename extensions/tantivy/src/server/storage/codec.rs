//! Canonical sidecar codec for durable lexical projections.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
};

use backend_semantic::index_core::{
    ENTITY_DOCUMENT_ID_BYTES, EntityDocumentId, LexicalRow, LexicalRowValue, LexicalSegment,
    LexicalSegmentId, MAX_LEXICAL_PAYLOAD_BYTES, MAX_LEXICAL_ROWS,
};

use super::{
    model::{StorePhase, TantivySegmentStoreError},
    store::io_error,
};

#[derive(Clone)]
pub(crate) struct StoredRow {
    pub(crate) term: Vec<u8>,
    pub(crate) document: EntityDocumentId,
    pub(crate) value: LexicalRowValue,
}

impl StoredRow {
    pub(crate) fn from_row(row: LexicalRow<'_>) -> Self {
        Self {
            term: row.term.to_vec(),
            document: row.document,
            value: row.value,
        }
    }
    pub(crate) fn as_row(&self) -> LexicalRow<'_> {
        LexicalRow {
            term: &self.term,
            document: self.document,
            value: self.value,
        }
    }
}

pub(crate) const MAGIC: &[u8; 8] = b"NTVXSC01";
pub(crate) const VERSION: u32 = 1;
pub(crate) const MAX_TANTIVY_TERM_BYTES: usize = 65_530;
pub(crate) const MAX_RAW_TERM_BYTES: usize = (MAX_TANTIVY_TERM_BYTES - 1) / 2;
const HEX: &[u8; 16] = b"0123456789abcdef";
pub(crate) const SIDECAR_NAME: &str = "rows.sidecar";
const HEADER_BYTES: u64 = 8 + 4 + 32 + 4;
#[allow(clippy::as_conversions, reason = "fixed core width is bounded")]
const ROW_FIXED_BYTES: u64 = 4 + ENTITY_DOCUMENT_ID_BYTES as u64 + 1 + 4;
#[allow(
    clippy::as_conversions,
    reason = "fixed core bounds fit the on-disk width"
)]
pub(crate) const MAX_SIDECAR_BYTES: u64 =
    HEADER_BYTES + MAX_LEXICAL_PAYLOAD_BYTES as u64 + MAX_LEXICAL_ROWS as u64 * ROW_FIXED_BYTES;

pub(crate) fn sidecar_path(path: &Path) -> std::path::PathBuf {
    path.join(SIDECAR_NAME)
}

#[allow(
    clippy::as_conversions,
    clippy::indexing_slicing,
    reason = "masked nybbles are indexes into the fixed 16-byte hex table"
)]
pub(crate) fn encode_term(term: &[u8]) -> Result<String, TantivySegmentStoreError> {
    let encoded_bytes = term
        .len()
        .checked_mul(2)
        .and_then(|value| value.checked_add(1))
        .ok_or(TantivySegmentStoreError::TermTooLong {
            raw_bytes: term.len(),
            encoded_bytes: usize::MAX,
            max_encoded_bytes: MAX_TANTIVY_TERM_BYTES,
        })?;
    if encoded_bytes > MAX_TANTIVY_TERM_BYTES {
        return Err(TantivySegmentStoreError::TermTooLong {
            raw_bytes: term.len(),
            encoded_bytes,
            max_encoded_bytes: MAX_TANTIVY_TERM_BYTES,
        });
    }
    let mut encoded = String::with_capacity(1 + term.len() * 2);
    encoded.push('x');
    for byte in term {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(encoded)
}

pub(crate) fn validate_term_len(raw_bytes: usize) -> Result<(), TantivySegmentStoreError> {
    if raw_bytes > MAX_RAW_TERM_BYTES {
        let encoded_bytes = raw_bytes.saturating_mul(2).saturating_add(1);
        return Err(TantivySegmentStoreError::TermTooLong {
            raw_bytes,
            encoded_bytes,
            max_encoded_bytes: MAX_TANTIVY_TERM_BYTES,
        });
    }
    let encoded_bytes = raw_bytes
        .checked_mul(2)
        .and_then(|value| value.checked_add(1))
        .ok_or(TantivySegmentStoreError::TermTooLong {
            raw_bytes,
            encoded_bytes: usize::MAX,
            max_encoded_bytes: MAX_TANTIVY_TERM_BYTES,
        })?;
    if encoded_bytes > MAX_TANTIVY_TERM_BYTES {
        return Err(TantivySegmentStoreError::TermTooLong {
            raw_bytes,
            encoded_bytes,
            max_encoded_bytes: MAX_TANTIVY_TERM_BYTES,
        });
    }
    Ok(())
}

pub(crate) fn stored_rows(segment: LexicalSegment<'_>) -> Vec<StoredRow> {
    segment
        .rows
        .iter()
        .copied()
        .map(StoredRow::from_row)
        .collect()
}

#[allow(
    clippy::ignored_unit_patterns,
    reason = "each chained write intentionally advances the same bounded file"
)]
pub(crate) fn write_sidecar(
    path: &Path,
    id: LexicalSegmentId,
    rows: &[StoredRow],
) -> Result<(), TantivySegmentStoreError> {
    let sidecar = sidecar_path(path);
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&sidecar)
        .map_err(|source| io_error(StorePhase::WriteSidecar, &sidecar, source))?;
    file.write_all(MAGIC)
        .and_then(|_| file.write_all(&VERSION.to_le_bytes()))
        .and_then(|_| file.write_all(id.as_ref()))
        .and_then(|_| {
            file.write_all(
                &u32::try_from(rows.len())
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "row count overflow"))?
                    .to_le_bytes(),
            )
        })
        .map_err(|source| io_error(StorePhase::WriteSidecar, &sidecar, source))?;
    for row in rows {
        let length =
            u32::try_from(row.term.len()).map_err(|_| TantivySegmentStoreError::Corrupt {
                path: sidecar.clone(),
                detail: "term length overflow",
            })?;
        file.write_all(&length.to_le_bytes())
            .and_then(|_| file.write_all(&row.term))
            .and_then(|_| file.write_all(&<[u8; ENTITY_DOCUMENT_ID_BYTES]>::from(row.document)))
            .and_then(|_| match row.value {
                LexicalRowValue::Present(score) => file
                    .write_all(&[1])
                    .and_then(|_| file.write_all(&u32::from(score).to_le_bytes())),
                LexicalRowValue::Tombstone => file.write_all(&[0, 0, 0, 0, 0]),
            })
            .map_err(|source| io_error(StorePhase::WriteSidecar, &sidecar, source))?;
    }
    file.sync_all()
        .map_err(|source| io_error(StorePhase::SyncSidecar, &sidecar, source))
}

#[allow(
    clippy::verbose_file_reads,
    reason = "metadata bounds the sidecar before this fixed-size read"
)]
pub(crate) fn read_sidecar(
    path: &Path,
    expected: LexicalSegmentId,
) -> Result<Vec<StoredRow>, TantivySegmentStoreError> {
    let sidecar = sidecar_path(path);
    let metadata = fs::metadata(&sidecar).map_err(|source| {
        if source.kind() == io::ErrorKind::NotFound {
            TantivySegmentStoreError::Corrupt {
                path: sidecar.clone(),
                detail: "sidecar missing",
            }
        } else {
            io_error(StorePhase::ReadSidecar, &sidecar, source)
        }
    })?;
    if !metadata.is_file() || metadata.len() > MAX_SIDECAR_BYTES {
        return Err(TantivySegmentStoreError::Corrupt {
            path: sidecar,
            detail: "sidecar exceeds bounded size",
        });
    }
    let length =
        usize::try_from(metadata.len()).map_err(|_| TantivySegmentStoreError::CountOverflow)?;
    let mut bytes = Vec::with_capacity(length);
    File::open(&sidecar)
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|source| io_error(StorePhase::ReadSidecar, &sidecar, source))?;
    let mut cursor = Cursor::new(&bytes);
    if cursor.take(8)? != MAGIC || cursor.u32()? != VERSION || cursor.take(32)? != expected.as_ref()
    {
        return Err(TantivySegmentStoreError::Corrupt {
            path: sidecar,
            detail: "sidecar header mismatch",
        });
    }
    let count =
        usize::try_from(cursor.u32()?).map_err(|_| TantivySegmentStoreError::CountOverflow)?;
    if count > MAX_LEXICAL_ROWS {
        return Err(TantivySegmentStoreError::Corrupt {
            path: sidecar,
            detail: "row count exceeds bound",
        });
    }
    let mut rows = Vec::with_capacity(count);
    let mut payload = 0_usize;
    for _ in 0..count {
        let length =
            usize::try_from(cursor.u32()?).map_err(|_| TantivySegmentStoreError::CountOverflow)?;
        payload = payload
            .checked_add(length)
            .ok_or(TantivySegmentStoreError::CountOverflow)?;
        if payload > MAX_LEXICAL_PAYLOAD_BYTES {
            return Err(TantivySegmentStoreError::Corrupt {
                path: sidecar.clone(),
                detail: "payload exceeds bound",
            });
        }
        let term = cursor.take_vec(length)?;
        let document = EntityDocumentId::try_from(cursor.take(ENTITY_DOCUMENT_ID_BYTES)?)
            .map_err(|source| TantivySegmentStoreError::MalformedDocument { source })?;
        let marker = cursor.byte()?;
        let score = u32::from_le_bytes(cursor.take(4)?.try_into().map_err(|_| {
            TantivySegmentStoreError::Corrupt {
                path: sidecar.clone(),
                detail: "score width",
            }
        })?);
        let value = match marker {
            0 => LexicalRowValue::Tombstone,
            1 => LexicalRowValue::Present(score.into()),
            _ => {
                return Err(TantivySegmentStoreError::Corrupt {
                    path: sidecar.clone(),
                    detail: "value marker",
                });
            }
        };
        rows.push(StoredRow {
            term,
            document,
            value,
        });
    }
    if cursor.remaining() != 0 {
        return Err(TantivySegmentStoreError::Corrupt {
            path: sidecar.clone(),
            detail: "trailing sidecar bytes",
        });
    }
    let borrowed = rows.iter().map(StoredRow::as_row).collect::<Vec<_>>();
    let verified =
        LexicalSegment::new(&borrowed).map_err(|_| TantivySegmentStoreError::Corrupt {
            path: sidecar.clone(),
            detail: "canonical row verification failed",
        })?;
    if verified.id != expected {
        return Err(TantivySegmentStoreError::IdentityMismatch {
            expected,
            observed: verified.id,
        });
    }
    Ok(rows)
}

struct Cursor<'bytes> {
    bytes: &'bytes [u8],
    position: usize,
}

impl<'bytes> Cursor<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, position: 0 }
    }
    fn take(&mut self, count: usize) -> Result<&'bytes [u8], TantivySegmentStoreError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(TantivySegmentStoreError::CountOverflow)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(TantivySegmentStoreError::Truncated)?;
        self.position = end;
        Ok(value)
    }
    fn take_vec(&mut self, count: usize) -> Result<Vec<u8>, TantivySegmentStoreError> {
        Ok(self.take(count)?.to_vec())
    }
    fn u32(&mut self) -> Result<u32, TantivySegmentStoreError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| TantivySegmentStoreError::Truncated)?,
        ))
    }
    fn byte(&mut self) -> Result<u8, TantivySegmentStoreError> {
        self.take(1)?
            .first()
            .copied()
            .ok_or(TantivySegmentStoreError::Truncated)
    }
    const fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.position)
    }
}
