use crate::{DependencyFacts, PackageDependencyRecord, ProductAdmissionError, RegistryEcosystem};

use super::{
    MAX_REGISTRY_NATIVE_METADATA_BYTES, MAX_REGISTRY_NATIVE_ROWS, REGISTRY_NATIVE_METADATA_VERSION,
    RegistryGoSourceFacts, RegistryMavenChecksum, RegistryNativeArtifact,
    RegistryNativeAvailability, RegistryNativeChecksum, RegistryNativeChecksumAlgorithm,
    RegistryNativeDetails, RegistryNativeEvidenceClaim, RegistryNativeMetadata,
    RegistryNativeObservation, RegistryNativeProvenance,
};

impl RegistryNativeMetadata {
    /// Creates a typed unavailable value for a feed without native facts.
    #[must_use]
    pub fn unavailable(ecosystem: RegistryEcosystem, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            version: REGISTRY_NATIVE_METADATA_VERSION,
            availability: RegistryNativeAvailability::NotRecorded(reason.clone()),
            provenance: RegistryNativeProvenance::NotRecorded(reason.clone()),
            details: RegistryNativeDetails::Unavailable { ecosystem, reason },
        }
    }

    /// Validates bounds, canonical ordering, and closed checksum widths.
    pub fn admit(&self) -> Result<(), ProductAdmissionError> {
        if self.version != REGISTRY_NATIVE_METADATA_VERSION {
            return Err(ProductAdmissionError::NativeMetadata);
        }
        validate_availability(&self.availability)?;
        validate_provenance(&self.provenance)?;
        match (&self.availability, &self.details) {
            (RegistryNativeAvailability::Recorded, RegistryNativeDetails::Unavailable { .. })
            | (
                RegistryNativeAvailability::NotRecorded(_),
                RegistryNativeDetails::Cargo(_)
                | RegistryNativeDetails::Npm(_)
                | RegistryNativeDetails::Pypi(_)
                | RegistryNativeDetails::Maven(_)
                | RegistryNativeDetails::Nuget(_)
                | RegistryNativeDetails::Golang(_)
                | RegistryNativeDetails::Cpp(_),
            ) => return Err(ProductAdmissionError::NativeMetadata),
            _ => {}
        }
        match (&self.availability, &self.provenance) {
            (RegistryNativeAvailability::Recorded, RegistryNativeProvenance::NotRecorded(_))
            | (
                RegistryNativeAvailability::NotRecorded(_),
                RegistryNativeProvenance::SourceDigest(_),
            ) => return Err(ProductAdmissionError::NativeMetadata),
            _ => {}
        }
        validate_details(&self.details)?;
        let encoded = self.encode_canonical();
        if encoded.len() > MAX_REGISTRY_NATIVE_METADATA_BYTES {
            return Err(ProductAdmissionError::NativeMetadata);
        }
        Ok(())
    }
}

fn validate_availability(value: &RegistryNativeAvailability) -> Result<(), ProductAdmissionError> {
    match value {
        RegistryNativeAvailability::Recorded => Ok(()),
        RegistryNativeAvailability::NotRecorded(reason) => validate_text(reason),
    }
}

fn validate_provenance(value: &RegistryNativeProvenance) -> Result<(), ProductAdmissionError> {
    match value {
        RegistryNativeProvenance::SourceDigest(_) => Ok(()),
        RegistryNativeProvenance::NotRecorded(reason) => validate_text(reason),
    }
}

fn validate_details(value: &RegistryNativeDetails) -> Result<(), ProductAdmissionError> {
    match value {
        RegistryNativeDetails::Cargo(value) => {
            validate_artifacts(&value.artifacts)?;
            if value.features.len() > MAX_REGISTRY_NATIVE_ROWS {
                return Err(ProductAdmissionError::NativeMetadata);
            }
            let mut previous = None;
            for feature in &value.features {
                validate_text(&feature.name)?;
                if previous.is_some_and(|previous| previous >= feature.name.as_str()) {
                    return Err(ProductAdmissionError::NativeMetadata);
                }
                previous = Some(feature.name.as_str());
                if feature.members.len() > MAX_REGISTRY_NATIVE_ROWS {
                    return Err(ProductAdmissionError::NativeMetadata);
                }
                validate_sorted_texts(&feature.members)?;
            }
        }
        RegistryNativeDetails::Npm(value) => {
            validate_artifacts(&value.artifacts)?;
            if value.dist_tags.len() > MAX_REGISTRY_NATIVE_ROWS {
                return Err(ProductAdmissionError::NativeMetadata);
            }
            let mut previous = None;
            for tag in &value.dist_tags {
                validate_text(&tag.name)?;
                validate_text(&tag.version)?;
                if previous.is_some_and(|previous| previous >= tag.name.as_str()) {
                    return Err(ProductAdmissionError::NativeMetadata);
                }
                previous = Some(tag.name.as_str());
            }
            validate_optional_text(value.deprecation.as_ref())?;
        }
        RegistryNativeDetails::Pypi(value) => {
            validate_artifacts(&value.artifacts)?;
            validate_optional_text(value.requires_python.as_ref())?;
            validate_optional_text(value.standing_reason.as_ref())?;
        }
        RegistryNativeDetails::Maven(value) => {
            validate_artifacts(&value.artifacts)?;
            validate_observation(&value.checksum, validate_maven_checksum)?;
            validate_observation(&value.signature, validate_evidence_claim)?;
            validate_observation(&value.pom, validate_evidence_claim)?;
            validate_dependency_observation(&value.dependencies)?;
        }
        RegistryNativeDetails::Nuget(value) => {
            validate_artifacts(&value.artifacts)?;
            validate_observation(&value.vulnerabilities, |rows| {
                if rows.len() > MAX_REGISTRY_NATIVE_ROWS {
                    return Err(ProductAdmissionError::NativeMetadata);
                }
                let mut previous = None;
                for row in rows {
                    if row.severity > 4 {
                        return Err(ProductAdmissionError::NativeMetadata);
                    }
                    validate_optional_text(row.advisory_url.as_ref())?;
                    let key = (row.advisory_url.as_deref().unwrap_or(""), row.severity);
                    if previous.is_some_and(|previous| previous >= key) {
                        return Err(ProductAdmissionError::NativeMetadata);
                    }
                    previous = Some(key);
                }
                Ok(())
            })?;
            validate_dependency_observation(&value.dependencies)?;
            validate_optional_text(value.deprecation.as_ref())?;
        }
        RegistryNativeDetails::Golang(value) => {
            validate_artifacts(&value.artifacts)?;
            if value.retracts.len() > MAX_REGISTRY_NATIVE_ROWS {
                return Err(ProductAdmissionError::NativeMetadata);
            }
            let mut previous = None;
            for retract in &value.retracts {
                validate_text(&retract.lower)?;
                validate_text(&retract.upper)?;
                let key = (retract.lower.as_str(), retract.upper.as_str());
                if previous.is_some_and(|previous| previous >= key) {
                    return Err(ProductAdmissionError::NativeMetadata);
                }
                previous = Some(key);
            }
            validate_observation(&value.source, validate_go_source)?;
        }
        RegistryNativeDetails::Cpp(value) => {
            validate_artifacts(&value.artifacts)?;
            validate_optional_text(value.source_url.as_ref())?;
        }
        RegistryNativeDetails::Unavailable { reason, .. } => validate_text(reason)?,
    }
    Ok(())
}

fn validate_artifacts(value: &[RegistryNativeArtifact]) -> Result<(), ProductAdmissionError> {
    if value.len() > MAX_REGISTRY_NATIVE_ROWS {
        return Err(ProductAdmissionError::NativeMetadata);
    }
    let mut previous = None;
    for artifact in value {
        validate_text(&artifact.filename)?;
        validate_text(&artifact.url)?;
        validate_checksum(&artifact.checksum)?;
        validate_optional_text(artifact.requires_python.as_ref())?;
        validate_optional_text(artifact.yanked_reason.as_ref())?;
        if artifact.yanked != Some(true) && artifact.yanked_reason.is_some() {
            return Err(ProductAdmissionError::NativeMetadata);
        }
        if previous.is_some_and(|previous| previous >= artifact.filename.as_str()) {
            return Err(ProductAdmissionError::NativeMetadata);
        }
        previous = Some(artifact.filename.as_str());
    }
    Ok(())
}

fn validate_checksum(value: &RegistryNativeChecksum) -> Result<(), ProductAdmissionError> {
    let expected = match value.algorithm {
        RegistryNativeChecksumAlgorithm::Sha1 => 20,
        RegistryNativeChecksumAlgorithm::Sha256 | RegistryNativeChecksumAlgorithm::GoModule => 32,
        RegistryNativeChecksumAlgorithm::Sha512 => 64,
    };
    (value.digest.len() == expected)
        .then_some(())
        .ok_or(ProductAdmissionError::NativeMetadata)
}

fn validate_maven_checksum(value: &RegistryMavenChecksum) -> Result<(), ProductAdmissionError> {
    validate_text(&value.url)?;
    validate_checksum(&value.checksum)
}

fn validate_evidence_claim(
    value: &RegistryNativeEvidenceClaim,
) -> Result<(), ProductAdmissionError> {
    validate_text(&value.url)
}

fn validate_go_source(value: &RegistryGoSourceFacts) -> Result<(), ProductAdmissionError> {
    validate_text(&value.module)?;
    validate_text(&value.version)?;
    validate_evidence_claim(&value.info)?;
    validate_evidence_claim(&value.module_file)?;
    validate_evidence_claim(&value.checksum)
}

fn validate_dependency_observation(
    value: &RegistryNativeObservation<DependencyFacts<Box<[PackageDependencyRecord]>>>,
) -> Result<(), ProductAdmissionError> {
    validate_observation(value, |facts| match facts {
        DependencyFacts::Known(rows) => {
            let canonical = crate::admit_dependency_rows(rows.to_vec())
                .map_err(|_| ProductAdmissionError::DependencyShape)?;
            if canonical.as_ref() == rows.as_ref() {
                Ok(())
            } else {
                Err(ProductAdmissionError::NativeMetadata)
            }
        }
        DependencyFacts::Unknown(reason) | DependencyFacts::Unavailable(reason) => {
            validate_text(reason.as_str())
        }
    })
}

fn validate_observation<T>(
    value: &RegistryNativeObservation<T>,
    validate: impl Fn(&T) -> Result<(), ProductAdmissionError>,
) -> Result<(), ProductAdmissionError> {
    match value {
        RegistryNativeObservation::Recorded(value) => validate(value),
        RegistryNativeObservation::NotRecorded(reason)
        | RegistryNativeObservation::Unavailable(reason) => validate_text(reason),
    }
}

fn validate_sorted_texts(values: &[String]) -> Result<(), ProductAdmissionError> {
    let mut previous = None;
    for value in values {
        validate_text(value)?;
        if previous.is_some_and(|previous| previous >= value.as_str()) {
            return Err(ProductAdmissionError::NativeMetadata);
        }
        previous = Some(value.as_str());
    }
    Ok(())
}

fn validate_optional_text(value: Option<&String>) -> Result<(), ProductAdmissionError> {
    value.map_or(Ok(()), |value| validate_text(value))
}

fn validate_text(value: &str) -> Result<(), ProductAdmissionError> {
    crate::ProductText::new(value.to_owned()).map(|_| ())
}
