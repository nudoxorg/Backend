pub(super) use crate::{
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencyTarget, PackageReference, ProductAdmissionError,
    ProductText, RegistryEcosystem,
};
use std::fmt;

pub(super) use super::{
    MAX_REGISTRY_NATIVE_METADATA_BYTES, MAX_REGISTRY_NATIVE_ROWS, MAX_REGISTRY_NATIVE_TEXT_BYTES,
    RegistryCargoMetadata, RegistryConanMetadata, RegistryConanSourceAvailability,
    RegistryGoMetadata, RegistryGoRetract, RegistryGoSourceFacts, RegistryMavenChecksum,
    RegistryMavenMetadata, RegistryNativeArtifact, RegistryNativeArtifactKind,
    RegistryNativeAvailability, RegistryNativeChecksum, RegistryNativeChecksumAlgorithm,
    RegistryNativeDetails, RegistryNativeDistTag, RegistryNativeEvidenceClaim,
    RegistryNativeFeature, RegistryNativeMetadata, RegistryNativeObservation,
    RegistryNativeProvenance, RegistryNativeVulnerability, RegistryNpmMetadata,
    RegistryNugetMetadata, RegistryPypiMetadata,
};

#[path = "decode.rs"]
mod decode;
#[path = "encode.rs"]
mod encode;

const REGISTRY_NATIVE_CODEC_MAGIC: &[u8; 4] = b"RNMD";
const REGISTRY_NATIVE_CODEC_VERSION: u8 = 1;

/// Errors returned while decoding the canonical native metadata wire value.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryNativeMetadataCodecError {
    /// The value ended before a complete field could be read.
    Truncated,
    /// A length or collection count exceeded its fixed bound.
    Bounds,
    /// A text field was not valid UTF-8 or did not pass product admission.
    Text,
    /// A closed enum contained an unknown discriminant.
    Tag,
    /// The decoded DTO failed its semantic admission checks.
    Admission,
}

impl fmt::Display for RegistryNativeMetadataCodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Truncated => "truncated registry native metadata",
            Self::Bounds => "bounded registry native metadata field exceeded",
            Self::Text => "invalid registry native metadata text",
            Self::Tag => "unknown registry native metadata tag",
            Self::Admission => "registry native metadata failed admission",
        })
    }
}

impl std::error::Error for RegistryNativeMetadataCodecError {}

impl RegistryNativeMetadata {
    /// Encodes the DTO using the explicit, versioned canonical journal grammar.
    ///
    /// The value is normally admitted before it reaches durable storage. This
    /// encoder itself is deliberately infallible so a journal implementation
    /// cannot panic while constructing a record; the reader and publication
    /// admission reject malformed or over-bound values.
    #[must_use]
    pub fn encode_canonical(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(REGISTRY_NATIVE_CODEC_MAGIC);
        output.push(REGISTRY_NATIVE_CODEC_VERSION);
        encode::put_u16(&mut output, self.version);
        encode::write_availability(&mut output, &self.availability);
        encode::write_provenance(&mut output, &self.provenance);
        encode::write_details(&mut output, &self.details);
        output
    }

    /// Decodes and admits the explicit canonical journal grammar.
    pub fn decode_canonical(bytes: &[u8]) -> Result<Self, RegistryNativeMetadataCodecError> {
        if bytes.len() > MAX_REGISTRY_NATIVE_METADATA_BYTES {
            return Err(RegistryNativeMetadataCodecError::Bounds);
        }
        let mut reader = decode::CanonicalReader::new(bytes);
        if reader.take_exact(4)? != REGISTRY_NATIVE_CODEC_MAGIC {
            return Err(RegistryNativeMetadataCodecError::Tag);
        }
        if reader.take_u8()? != REGISTRY_NATIVE_CODEC_VERSION {
            return Err(RegistryNativeMetadataCodecError::Tag);
        }
        let value = RegistryNativeMetadata {
            version: reader.take_u16()?,
            availability: decode::read_availability(&mut reader)?,
            provenance: decode::read_provenance(&mut reader)?,
            details: decode::read_details(&mut reader)?,
        };
        if !reader.is_empty() {
            return Err(RegistryNativeMetadataCodecError::Bounds);
        }
        value
            .admit()
            .map_err(|_| RegistryNativeMetadataCodecError::Admission)?;
        Ok(value)
    }

    /// Computes the stable identity of this admitted metadata DTO.
    pub fn identity(&self) -> Result<[u8; 32], ProductAdmissionError> {
        self.admit()?;
        let encoded = self.encode_canonical();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.registry.native-metadata.v1\0");
        hasher.update(&encoded);
        Ok(*hasher.finalize().as_bytes())
    }
}
