use core::marker::PhantomData;

use crate::{CONTENT_PAYLOAD_BYTES, ContentId, Domain, DomainCode};

/// Checked proof that one observed wire authority selects `DomainTag`.
///
/// Compact artifact grammars may carry this authority once and store only the
/// remaining identity payload in each record. Payload bytes cannot construct a
/// typed identity without first producing this proof from the observed wire
/// cell.
///
/// ```compile_fail
/// use nudox_id::{ContentId, ObjectDomain};
/// let _: ContentId<ObjectDomain> = [0_u8; 31].into();
/// ```
#[derive(Debug, Eq, PartialEq)]
pub struct ContentAuthority<DomainTag> {
    domain: PhantomData<fn() -> DomainTag>,
}

impl<DomainTag> Copy for ContentAuthority<DomainTag> {}

impl<DomainTag> Clone for ContentAuthority<DomainTag> {
    fn clone(&self) -> Self {
        *self
    }
}

/// Rejection while binding one observed wire authority to a typed domain.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("content authority code is {observed}, expected {expected:?}")]
pub struct ContentAuthorityError {
    /// Expected closed registry code.
    pub expected: DomainCode,
    /// Complete observed wire cell.
    pub observed: u8,
}

impl<DomainTag: Domain> TryFrom<u8> for ContentAuthority<DomainTag> {
    type Error = ContentAuthorityError;

    fn try_from(observed: u8) -> Result<Self, Self::Error> {
        if observed != u8::from(DomainTag::CODE) {
            return Err(ContentAuthorityError {
                expected: DomainTag::CODE,
                observed,
            });
        }
        Ok(Self {
            domain: PhantomData,
        })
    }
}

impl<DomainTag: Domain> ContentAuthority<DomainTag> {
    /// Binds payload bytes covered by this validated artifact authority.
    #[must_use]
    pub fn bind(self, payload: [u8; CONTENT_PAYLOAD_BYTES]) -> ContentId<DomainTag> {
        ContentId::bind_authority(self, payload)
    }
}

#[cfg(test)]
mod tests {
    use crate::{ContentAuthority, ContentAuthorityError, ContentId, DomainCode, ObjectDomain};

    #[test]
    fn observed_authority_is_required_before_a_compact_payload_can_bind() {
        let payload = [9; 31];
        let bound = ContentAuthority::<ObjectDomain>::try_from(u8::from(DomainCode::Object))
            .map(|authority| authority.bind(payload));
        assert_eq!(bound, Ok(ContentId::from_digest([9; 32])));
        assert_eq!(
            ContentAuthority::<ObjectDomain>::try_from(u8::from(DomainCode::DependencySet)),
            Err(ContentAuthorityError {
                expected: DomainCode::Object,
                observed: u8::from(DomainCode::DependencySet),
            })
        );
    }
}
