//! The only post-validation semantic reconstruction boundary.

use nudox_object::{ObjectRef, ProviderSet};

use super::errors::LocalityReadError;

/// Reconstructs a provider bitmap checked as non-empty during locality parse.
pub(super) fn provider(raw: u64, ordinal: u32) -> Result<ProviderSet, LocalityReadError> {
    ProviderSet::try_from(raw).map_err(|source| LocalityReadError::Provider {
        ordinal,
        observed: raw,
        source,
    })
}

/// Reconstructs a descriptor whose closed schema was checked during locality
/// parse while borrowing these same immutable bytes.
pub(super) fn descriptor<DomainTag>(
    record_bytes: &[u8],
    ordinal: u32,
) -> Result<ObjectRef<DomainTag>, LocalityReadError> {
    ObjectRef::try_from(record_bytes)
        .map_err(|source| LocalityReadError::Descriptor { ordinal, source })
}

#[cfg(test)]
mod tests {
    use nudox_id::ObjectDomain;
    use nudox_object::{
        OBJECT_DESCRIPTOR_RECORD_BYTES, ObjectDescriptorDecodeError, ProviderSetError,
    };
    use nudox_schema::UnknownSchemaId;

    use super::{LocalityReadError, descriptor, provider};

    #[test]
    fn post_validation_provider_drift_is_contained() {
        assert_eq!(
            provider(0, 7),
            Err(LocalityReadError::Provider {
                ordinal: 7,
                observed: 0,
                source: ProviderSetError::Empty,
            })
        );
    }

    #[test]
    fn post_validation_descriptor_drift_is_contained() {
        assert_eq!(
            descriptor::<ObjectDomain>(&[0; OBJECT_DESCRIPTOR_RECORD_BYTES - 1], 3),
            Err(LocalityReadError::Descriptor {
                ordinal: 3,
                source: ObjectDescriptorDecodeError::Width {
                    actual: OBJECT_DESCRIPTOR_RECORD_BYTES - 1,
                },
            })
        );
        let mut bytes = [0_u8; OBJECT_DESCRIPTOR_RECORD_BYTES];
        bytes[40..44].copy_from_slice(&99_u32.to_be_bytes());
        assert_eq!(
            descriptor::<ObjectDomain>(&bytes, 5),
            Err(LocalityReadError::Descriptor {
                ordinal: 5,
                source: ObjectDescriptorDecodeError::Schema(UnknownSchemaId(99)),
            })
        );
    }
}
