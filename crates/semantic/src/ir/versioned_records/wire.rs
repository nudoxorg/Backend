//! Shared strict field framing for canonical SPIR row families.

use alloc::vec::Vec;

use crate::ir::{
    CheckedJumboValueDescriptor, DeclarationIdentity, JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES,
    JumboRopeLimits, JumboValueEncoding, JumboValueFamily, SemanticPlaneRecordError,
    UntrustedJumboValueDescriptor,
};

pub(super) fn encode_identity(identity: DeclarationIdentity, out: &mut Vec<u8>) {
    out.extend_from_slice(identity.family.as_bytes());
    out.extend_from_slice(identity.variant.as_bytes());
}

pub(super) fn put_bytes(out: &mut Vec<u8>, value: &[u8]) -> Result<(), SemanticPlaneRecordError> {
    put_u32(out, value.len())?;
    out.extend_from_slice(value);
    Ok(())
}

pub(super) fn put_text(out: &mut Vec<u8>, value: &str) -> Result<(), SemanticPlaneRecordError> {
    put_bytes(out, value.as_bytes())
}

pub(super) fn put_u32(out: &mut Vec<u8>, value: usize) -> Result<(), SemanticPlaneRecordError> {
    let value = u32::try_from(value).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    out.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

pub(super) fn read_identity(
    cursor: &mut Cursor<'_>,
) -> Result<DeclarationIdentity, SemanticPlaneRecordError> {
    let family: [u8; 16] = cursor
        .take(16)?
        .try_into()
        .map_err(|_| SemanticPlaneRecordError::Truncated)?;
    let variant: [u8; 16] = cursor
        .take(16)?
        .try_into()
        .map_err(|_| SemanticPlaneRecordError::Truncated)?;
    Ok(DeclarationIdentity {
        family: crate::ir::DeclarationFamilyId::from_raw(family),
        variant: crate::ir::VariantFingerprint::from_raw(variant),
    })
}

pub(super) fn read_checked_jumbo_descriptor(
    cursor: &mut Cursor<'_>,
    expected_owner: [u8; 32],
    expected_family: JumboValueFamily,
    expected_field_ordinal: u32,
    expected_encoding: JumboValueEncoding,
) -> Result<CheckedJumboValueDescriptor, SemanticPlaneRecordError> {
    let wire = cursor.take(JUMBO_VALUE_DESCRIPTOR_WIRE_BYTES)?;
    let untrusted = UntrustedJumboValueDescriptor::decode_wire(wire)?;
    let descriptor = untrusted.check(JumboRopeLimits::default())?;
    if descriptor.owner() != &expected_owner
        || descriptor.family() != expected_family
        || descriptor.field_ordinal() != expected_field_ordinal
        || descriptor.encoding() != expected_encoding
    {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    Ok(descriptor)
}

pub(super) fn validate_jumbo_row_size(
    descriptor: &CheckedJumboValueDescriptor,
    fixed_row_bytes: usize,
    maximum_inline_row_bytes: usize,
) -> Result<(), SemanticPlaneRecordError> {
    let fixed_row_bytes =
        u64::try_from(fixed_row_bytes).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    let maximum_inline_row_bytes = u64::try_from(maximum_inline_row_bytes)
        .map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
    if fixed_row_bytes
        .checked_add(descriptor.byte_length())
        .is_none_or(|row_bytes| row_bytes <= maximum_inline_row_bytes)
    {
        return Err(SemanticPlaneRecordError::RowGrammar);
    }
    Ok(())
}

pub(super) struct Cursor<'bytes> {
    bytes: &'bytes [u8],
}

impl<'bytes> Cursor<'bytes> {
    pub(super) const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes }
    }

    pub(super) fn take(&mut self, length: usize) -> Result<&'bytes [u8], SemanticPlaneRecordError> {
        if self.bytes.len() < length {
            return Err(SemanticPlaneRecordError::Truncated);
        }
        let (value, rest) = self.bytes.split_at(length);
        self.bytes = rest;
        Ok(value)
    }

    pub(super) fn u8(&mut self) -> Result<u8, SemanticPlaneRecordError> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn u16(&mut self) -> Result<u16, SemanticPlaneRecordError> {
        Ok(u16::from_be_bytes(
            self.take(2)?
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?,
        ))
    }

    pub(super) fn u32(&mut self) -> Result<u32, SemanticPlaneRecordError> {
        Ok(u32::from_be_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| SemanticPlaneRecordError::Truncated)?,
        ))
    }

    pub(super) fn bytes32(&mut self) -> Result<&'bytes [u8], SemanticPlaneRecordError> {
        let length =
            usize::try_from(self.u32()?).map_err(|_| SemanticPlaneRecordError::RowTooLarge)?;
        self.take(length)
    }

    pub(super) fn utf8(&mut self) -> Result<&'bytes str, SemanticPlaneRecordError> {
        core::str::from_utf8(self.bytes32()?).map_err(|_| SemanticPlaneRecordError::RowGrammar)
    }

    pub(super) const fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}
