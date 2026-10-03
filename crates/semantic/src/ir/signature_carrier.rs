//! Packed, entity-aligned structural function-signature carrier roles.

use alloc::vec::Vec;

use super::semantic::BuildError;
use super::{CapacityError, CapacitySpace, SignatureCarrierRole};

/// Complete two-bit role observations for one exact entity row sequence.
pub(crate) struct PackedSignatureCarrierRoles {
    entity_count: usize,
    bytes: Vec<u8>,
}

impl PackedSignatureCarrierRoles {
    pub(crate) fn from_roles(roles: &[SignatureCarrierRole]) -> Result<Self, BuildError> {
        let byte_count = roles.len().div_ceil(4);
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(byte_count).map_err(|_| {
            BuildError::Capacity(CapacityError {
                space: CapacitySpace::Value,
                actual: byte_count,
            })
        })?;
        bytes.resize(byte_count, 0);
        for (index, role) in roles.iter().copied().enumerate() {
            let byte = index / 4;
            let shift = (index % 4) * 2;
            if let Some(slot) = bytes.get_mut(byte) {
                *slot |= role.bits() << shift;
            }
        }
        Ok(Self {
            entity_count: roles.len(),
            bytes,
        })
    }

    pub(crate) fn entity_count(&self) -> usize {
        self.entity_count
    }

    pub(crate) fn role(&self, entity: usize) -> Option<SignatureCarrierRole> {
        if entity >= self.entity_count {
            return None;
        }
        let byte = *self.bytes.get(entity / 4)?;
        let shift = (entity % 4) * 2;
        SignatureCarrierRole::from_bits((byte >> shift) & 0b11)
    }
}
