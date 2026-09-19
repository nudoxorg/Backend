//! Authority sources used while lowering complete view coverage.

use super::WireCertificate;
use crate::CoverageCapability;
use backend_version::ProducerObservationVerifier;

/// Source of authority used while lowering complete view coverage. A typed
/// capability is accepted for embedded callers; a process peer instead
/// supplies a verifier that admits the complete wire observation before any
/// `ViewRoot` is constructed.
pub(crate) trait CoverageAdmission {
    fn admit(
        &self,
        certificate: &WireCertificate,
        object_id: &str,
    ) -> Result<Option<CoverageCapability>, String>;
}

pub(crate) struct CapabilityAdmission(pub(crate) Option<CoverageCapability>);

impl CoverageAdmission for CapabilityAdmission {
    fn admit(
        &self,
        certificate: &WireCertificate,
        object_id: &str,
    ) -> Result<Option<CoverageCapability>, String> {
        self.0
            .as_ref()
            .map(|capability| {
                certificate
                    .admit_coverage_capability(object_id, capability)
                    .map(|()| capability.clone())
            })
            .transpose()
    }
}

pub(crate) struct VerifierAdmission<'a, V: ProducerObservationVerifier>(pub(crate) &'a V);

impl<V: ProducerObservationVerifier> CoverageAdmission for VerifierAdmission<'_, V> {
    fn admit(
        &self,
        certificate: &WireCertificate,
        object_id: &str,
    ) -> Result<Option<CoverageCapability>, String> {
        certificate
            .admit_coverage_with_verifier(object_id, self.0)
            .map(Some)
    }
}
