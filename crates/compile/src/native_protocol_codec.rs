//! Canonical native envelope encoding and hostile-input decoding.

use super::{
    EnvelopeState, MAX_NATIVE_KEY_BYTES, MAX_NATIVE_PAYLOAD_BYTES, MAX_NATIVE_RECORDS,
    MAX_NATIVE_VALUE_BYTES, NATIVE_PAYLOAD_VERSION, NativeCoverage, NativeEnvelope,
    NativeProtocolError, NativeRecord, NativeRecordKind, PAYLOAD_HEADER_BYTES, PAYLOAD_MAGIC,
    RECORD_HEADER_BYTES, Unbound, put_u16, put_u32, read_id, read_u16, read_u32, read_u64, take,
    validate_key, validate_records,
};
use std::str;

impl<S: EnvelopeState> NativeEnvelope<S> {
    /// Returns the language identity echoed by the helper.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }

    /// Returns the request session identity echoed by the helper.
    #[must_use]
    pub const fn session(&self) -> S::Session {
        self.session
    }

    /// Returns the request manifest identity echoed by the helper.
    #[must_use]
    pub const fn manifest(&self) -> S::Manifest {
        self.manifest
    }

    /// Returns the authority identity echoed by the helper.
    #[must_use]
    pub const fn authority(&self) -> S::Authority {
        self.authority
    }

    /// Returns the discovery revision echoed by the helper.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Returns the helper's explicit coverage claim.
    #[must_use]
    pub const fn coverage(&self) -> NativeCoverage {
        self.coverage
    }

    /// Returns records in canonical order.
    #[must_use]
    pub fn records(&self) -> &[NativeRecord] {
        &self.records
    }

    /// Encodes the envelope in the bounded canonical wire format.
    ///
    /// # Errors
    ///
    /// Returns [`NativeProtocolError`] when records or encoded lengths are not
    /// valid for the canonical wire format.
    pub fn encode(&self) -> Result<Vec<u8>, NativeProtocolError> {
        validate_records(&self.records)?;
        let length = self.encoded_len()?;
        let mut output = Vec::with_capacity(length);
        output.extend_from_slice(&PAYLOAD_MAGIC);
        output.extend_from_slice(&NATIVE_PAYLOAD_VERSION.to_be_bytes());
        output.push(self.coverage.tag());
        output.push(0);
        put_u32(&mut output, self.records.len())?;
        output.extend_from_slice(&S::session_bytes(&self.session));
        output.extend_from_slice(&S::manifest_bytes(&self.manifest));
        output.extend_from_slice(&S::authority_bytes(&self.authority));
        output.extend_from_slice(&self.revision.to_be_bytes());
        put_u16(&mut output, self.language.len())?;
        output.extend_from_slice(self.language.as_bytes());
        for record in &self.records {
            output.push(record.kind as u8);
            output.push(0);
            put_u32(&mut output, record.key.len())?;
            put_u32(&mut output, record.value.len())?;
            output.extend_from_slice(record.key.as_bytes());
            output.extend_from_slice(&record.value);
        }
        if output.len() != length {
            return Err(NativeProtocolError::LengthOverflow);
        }
        Ok(output)
    }

    pub(crate) fn encoded_len(&self) -> Result<usize, NativeProtocolError> {
        if self.records.len() > MAX_NATIVE_RECORDS {
            return Err(NativeProtocolError::RecordCountLimit {
                actual: self.records.len(),
                maximum: MAX_NATIVE_RECORDS,
            });
        }
        let mut length = PAYLOAD_HEADER_BYTES
            .checked_add(self.language.len())
            .ok_or(NativeProtocolError::LengthOverflow)?;
        for record in &self.records {
            validate_key(&record.key)?;
            if record.value.len() > MAX_NATIVE_VALUE_BYTES {
                return Err(NativeProtocolError::ValueLimit {
                    actual: record.value.len(),
                    maximum: MAX_NATIVE_VALUE_BYTES,
                });
            }
            length = length
                .checked_add(RECORD_HEADER_BYTES)
                .and_then(|length| length.checked_add(record.key.len()))
                .and_then(|length| length.checked_add(record.value.len()))
                .ok_or(NativeProtocolError::LengthOverflow)?;
        }
        if length > MAX_NATIVE_PAYLOAD_BYTES {
            return Err(NativeProtocolError::ByteLimit {
                actual: length,
                maximum: MAX_NATIVE_PAYLOAD_BYTES,
            });
        }
        Ok(length)
    }
}

impl NativeEnvelope<Unbound> {
    /// Decodes and strictly validates one canonical envelope.
    ///
    /// # Errors
    ///
    /// Returns [`NativeProtocolError`] when bytes are truncated, noncanonical,
    /// malformed, or exceed a protocol bound.
    pub fn decode(bytes: &[u8]) -> Result<Self, NativeProtocolError> {
        if bytes.len() > MAX_NATIVE_PAYLOAD_BYTES {
            return Err(NativeProtocolError::ByteLimit {
                actual: bytes.len(),
                maximum: MAX_NATIVE_PAYLOAD_BYTES,
            });
        }
        if bytes.len() < PAYLOAD_HEADER_BYTES {
            return Err(NativeProtocolError::Truncated);
        }
        if bytes[..4] != PAYLOAD_MAGIC {
            return Err(NativeProtocolError::InvalidMagic);
        }
        let version = u16::from_be_bytes([bytes[4], bytes[5]]);
        if version != NATIVE_PAYLOAD_VERSION {
            return Err(NativeProtocolError::UnsupportedVersion(version));
        }
        let coverage = NativeCoverage::from_tag(bytes[6])?;
        if bytes[7] != 0 {
            return Err(NativeProtocolError::InvalidFlags(bytes[7]));
        }
        let count = read_u32(&bytes[8..12])?;
        let count = usize::try_from(count).map_err(|_| NativeProtocolError::RecordCountLimit {
            actual: usize::MAX,
            maximum: MAX_NATIVE_RECORDS,
        })?;
        if count > MAX_NATIVE_RECORDS {
            return Err(NativeProtocolError::RecordCountLimit {
                actual: count,
                maximum: MAX_NATIVE_RECORDS,
            });
        }
        let mut cursor = 12;
        let session = read_id(take(bytes, &mut cursor, 32)?)?;
        let manifest = read_id(take(bytes, &mut cursor, 32)?)?;
        let authority = read_id(take(bytes, &mut cursor, 32)?)?;
        let revision = read_u64(take(bytes, &mut cursor, 8)?)?;
        let language_len = usize::from(read_u16(take(bytes, &mut cursor, 2)?)?);
        if language_len > MAX_NATIVE_KEY_BYTES {
            return Err(NativeProtocolError::KeyLimit {
                actual: language_len,
                maximum: MAX_NATIVE_KEY_BYTES,
            });
        }
        let language = str::from_utf8(take(bytes, &mut cursor, language_len)?)
            .map_err(|_| NativeProtocolError::InvalidUtf8)?
            .to_owned();
        validate_key(&language)?;
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            let header = take(bytes, &mut cursor, RECORD_HEADER_BYTES)?;
            let kind = NativeRecordKind::from_tag(header[0])?;
            if header[1] != 0 {
                return Err(NativeProtocolError::InvalidFlags(header[1]));
            }
            let key_len = usize::try_from(u32::from_be_bytes([
                header[2], header[3], header[4], header[5],
            ]))
            .map_err(|_| NativeProtocolError::LengthOverflow)?;
            let value_len = usize::try_from(u32::from_be_bytes([
                header[6], header[7], header[8], header[9],
            ]))
            .map_err(|_| NativeProtocolError::LengthOverflow)?;
            if key_len > MAX_NATIVE_KEY_BYTES {
                return Err(NativeProtocolError::KeyLimit {
                    actual: key_len,
                    maximum: MAX_NATIVE_KEY_BYTES,
                });
            }
            if value_len > MAX_NATIVE_VALUE_BYTES {
                return Err(NativeProtocolError::ValueLimit {
                    actual: value_len,
                    maximum: MAX_NATIVE_VALUE_BYTES,
                });
            }
            let key = str::from_utf8(take(bytes, &mut cursor, key_len)?)
                .map_err(|_| NativeProtocolError::InvalidUtf8)?
                .to_owned();
            validate_key(&key)?;
            let value = take(bytes, &mut cursor, value_len)?.to_owned();
            records.push(NativeRecord { kind, key, value });
        }
        if cursor != bytes.len() {
            return Err(NativeProtocolError::TrailingBytes {
                actual: bytes.len() - cursor,
            });
        }
        validate_records(&records)?;
        Ok(Self {
            language,
            session,
            manifest,
            authority,
            revision,
            coverage,
            records,
            state: std::marker::PhantomData,
        })
    }
}
