//! `PSP1` bytes for one semantic publication relation.
//!
//! Keys length-prefix their text. Order keys follow the key's sort, field by
//! field, so a Merkle page of these rows stays sorted. A claim is the
//! locality-independent manifest and binding; journal offsets never enter it.

use std::{mem::size_of, num::NonZeroU32};

use crate::publication::{
    binding::{CompilationBindingFacts, CompilationBindingIdentity},
    manifest::{CompilationManifestFacts, CompilationManifestFormat, CompilationManifestIdentity},
};
use backend_library::PackageReference;
use backend_semantic::vocabulary::{LanguageProfile, PackageUrl};
use backend_store::hydration::VerifiedGenerationFacts;
use backend_version::{
    CanonicalRelation, ContentId, DependencySetDomain, GenerationId, Relation, RelationDecodeError,
};

use super::{
    PartialSemanticCoverage, ProductSemanticPublicationKey, ProductSemanticPublicationRecord,
    ProductSemanticPublicationRelation, SemanticPublicationClaim, SemanticPublicationCoverage,
    SemanticPublicationSelection, SemanticUnavailableReason,
};

pub(super) const MAGIC: &[u8; 4] = b"PSP1";
const IDENTITY_BYTES: usize = size_of::<[u8; 32]>();
const GENERATION_BYTES: usize = IDENTITY_BYTES * 2;
pub(super) const MANIFEST_BYTES: usize = IDENTITY_BYTES + size_of::<u8>() + size_of::<u32>() * 2;
const BINDING_FACT_BYTES: usize = IDENTITY_BYTES + GENERATION_BYTES + IDENTITY_BYTES;
pub(super) const CLAIM_BYTES: usize = MANIFEST_BYTES + BINDING_FACT_BYTES;

impl Relation for ProductSemanticPublicationRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 3;
    const VERSION: u8 = 3;
    type Key = ProductSemanticPublicationKey;
    type Value = ProductSemanticPublicationRecord;

    fn encode_key(key: &Self::Key, output: &mut Vec<u8>) {
        push_text(key.package.as_str(), output);
        push_text(key.coordinate.as_str(), output);
        output.extend_from_slice(&<[u8; 2]>::from(key.profile));
        match key.selection {
            SemanticPublicationSelection::Selected => output.push(0),
            SemanticPublicationSelection::Generation(identity) => {
                output.push(1);
                output.extend_from_slice(identity.as_ref());
            }
        }
    }

    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(MAGIC);
        match value {
            ProductSemanticPublicationRecord::Published { coverage, claim } => {
                output.push(1);
                encode_coverage(*coverage, output);
                encode_claim(*claim, output);
            }
            ProductSemanticPublicationRecord::Unavailable(reason) => {
                output.push(2);
                output.push(*reason as u8);
            }
        }
    }
}

impl CanonicalRelation for ProductSemanticPublicationRelation {
    /// The canonical key length-prefixes its text fields, so two packages
    /// whose names differ in length sort by length in `encode_key` bytes but
    /// by text in the relation. A Merkle page of such rows is then refused as
    /// unsorted and remote execution silently falls back to local. This
    /// encoding follows the key's `Ord` field by field instead: the reference
    /// variant, each text terminated (with its zero bytes escaped), the
    /// profile's variant and revision, then the selection.
    fn encode_order_key(key: &Self::Key, output: &mut Vec<u8>) {
        let (reference, text) = match &key.package {
            PackageReference::Purl(url) => (0, url.as_str()),
            PackageReference::Local(label) => (1, label.as_str()),
        };
        output.push(reference);
        push_order_text(text, output);
        push_order_text(key.coordinate.as_str(), output);
        output.extend_from_slice(&profile_order(key.profile));
        match key.selection {
            SemanticPublicationSelection::Selected => output.push(0),
            SemanticPublicationSelection::Generation(identity) => {
                output.push(1);
                output.extend_from_slice(identity.as_ref());
            }
        }
    }

    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        let (package, rest) = take_text(bytes)?;
        let package = PackageReference::parse(package.to_owned())
            .map_err(|_| RelationDecodeError::Malformed)?;
        let (coordinate, rest) = take_text(rest)?;
        let coordinate =
            PackageUrl::parse(coordinate.to_owned()).map_err(|_| RelationDecodeError::Malformed)?;
        let (profile, rest) = rest
            .split_first_chunk::<2>()
            .ok_or(RelationDecodeError::Malformed)?;
        let profile =
            LanguageProfile::try_from(*profile).map_err(|_| RelationDecodeError::Malformed)?;
        let (&selection, rest) = rest.split_first().ok_or(RelationDecodeError::Malformed)?;
        let key = Self::Key::new(package, coordinate, profile)
            .map_err(|_| RelationDecodeError::Malformed)?;
        match selection {
            0 if rest.is_empty() => Ok(key),
            1 => {
                let (identity, rest) = take_identity(rest)?;
                if !rest.is_empty() {
                    return Err(RelationDecodeError::Malformed);
                }
                let identity = CompilationBindingIdentity::try_from(identity)
                    .map_err(|_| RelationDecodeError::Malformed)?;
                Ok(key.for_generation(identity))
            }
            _ => Err(RelationDecodeError::Malformed),
        }
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        let rest = bytes
            .strip_prefix(MAGIC)
            .ok_or(RelationDecodeError::Malformed)?;
        let (&tag, rest) = rest.split_first().ok_or(RelationDecodeError::Malformed)?;
        match tag {
            1 => decode_published(rest),
            2 => {
                let [reason] = rest else {
                    return Err(RelationDecodeError::Malformed);
                };
                Ok(ProductSemanticPublicationRecord::Unavailable(
                    SemanticUnavailableReason::from_tag(*reason)
                        .ok_or(RelationDecodeError::Malformed)?,
                ))
            }
            _ => Err(RelationDecodeError::Malformed),
        }
    }
}

fn encode_coverage(coverage: SemanticPublicationCoverage, output: &mut Vec<u8>) {
    match coverage {
        SemanticPublicationCoverage::Complete => output.push(1),
        SemanticPublicationCoverage::Partial(partial) => {
            output.push(2);
            output.extend_from_slice(&partial.completed().get().to_be_bytes());
            output.extend_from_slice(&partial.total().get().to_be_bytes());
        }
    }
}

fn decode_published(bytes: &[u8]) -> Result<ProductSemanticPublicationRecord, RelationDecodeError> {
    let (&tag, mut rest) = bytes.split_first().ok_or(RelationDecodeError::Malformed)?;
    let coverage = match tag {
        1 => SemanticPublicationCoverage::Complete,
        2 => {
            let (completed, next) = take_u32(rest)?;
            let (total, next) = take_u32(next)?;
            rest = next;
            let completed = NonZeroU32::new(completed).ok_or(RelationDecodeError::Malformed)?;
            let total = NonZeroU32::new(total).ok_or(RelationDecodeError::Malformed)?;
            SemanticPublicationCoverage::Partial(
                PartialSemanticCoverage::new(completed, total)
                    .map_err(|_| RelationDecodeError::Malformed)?,
            )
        }
        _ => return Err(RelationDecodeError::Malformed),
    };
    let claim = decode_claim(rest)?;
    Ok(ProductSemanticPublicationRecord::Published { coverage, claim })
}

fn encode_claim(claim: SemanticPublicationClaim, output: &mut Vec<u8>) {
    let manifest = claim.manifest();
    let binding = claim.binding();
    output.extend_from_slice(manifest.identity.as_ref());
    output.push(manifest_format_tag(manifest.format));
    output.extend_from_slice(&manifest.fragment_count.to_be_bytes());
    output.extend_from_slice(&manifest.byte_length.to_be_bytes());
    output.extend_from_slice(binding.identity.as_ref());
    encode_generation(binding.generation, output);
    output.extend_from_slice(binding.manifest.as_ref());
}

fn decode_claim(bytes: &[u8]) -> Result<SemanticPublicationClaim, RelationDecodeError> {
    if bytes.len() != CLAIM_BYTES {
        return Err(RelationDecodeError::Malformed);
    }
    let (manifest_identity, rest) = take_identity(bytes)?;
    let (&manifest_format, rest) = rest.split_first().ok_or(RelationDecodeError::Malformed)?;
    let (fragment_count, rest) = take_u32(rest)?;
    let (manifest_length, rest) = take_u32(rest)?;
    let (binding_identity, rest) = take_identity(rest)?;
    let (binding_generation, rest) = take_generation(rest)?;
    let (binding_manifest, rest) = take_identity(rest)?;
    if !rest.is_empty() {
        return Err(RelationDecodeError::Malformed);
    }
    let manifest_identity = CompilationManifestIdentity::try_from(manifest_identity)
        .map_err(|_| RelationDecodeError::Malformed)?;
    SemanticPublicationClaim::admit(
        CompilationManifestFacts {
            identity: manifest_identity,
            format: match manifest_format {
                2 => CompilationManifestFormat::SemanticV2,
                _ => return Err(RelationDecodeError::Malformed),
            },
            fragment_count,
            byte_length: manifest_length,
        },
        CompilationBindingFacts {
            identity: CompilationBindingIdentity::try_from(binding_identity)
                .map_err(|_| RelationDecodeError::Malformed)?,
            generation: binding_generation,
            manifest: CompilationManifestIdentity::try_from(binding_manifest)
                .map_err(|_| RelationDecodeError::Malformed)?,
        },
    )
    .map_err(|_| RelationDecodeError::Malformed)
}

const fn manifest_format_tag(format: CompilationManifestFormat) -> u8 {
    match format {
        CompilationManifestFormat::CompactV1 => 1,
        CompilationManifestFormat::SemanticV2 => 2,
    }
}

fn encode_generation(generation: VerifiedGenerationFacts, output: &mut Vec<u8>) {
    output.extend_from_slice(generation.pinned_root.as_ref());
    output.extend_from_slice(generation.dep_set.as_ref());
}

fn take_generation(bytes: &[u8]) -> Result<(VerifiedGenerationFacts, &[u8]), RelationDecodeError> {
    let (root, rest) = take_identity(bytes)?;
    let (dependencies, rest) = take_identity(rest)?;
    Ok((
        VerifiedGenerationFacts {
            pinned_root: GenerationId::try_from(root)
                .map_err(|_| RelationDecodeError::Malformed)?,
            dep_set: ContentId::<DependencySetDomain>::try_from(dependencies)
                .map_err(|_| RelationDecodeError::Malformed)?,
        },
        rest,
    ))
}

/// Writes text so that byte order equals text order: a zero byte is escaped
/// as `00 FF` and the text ends with `00 00`, so a prefix sorts first.
fn push_order_text(value: &str, output: &mut Vec<u8>) {
    for &byte in value.as_bytes() {
        output.push(byte);
        if byte == 0 {
            output.push(0xFF);
        }
    }
    output.extend_from_slice(&[0, 0]);
}

/// A profile's position in `LanguageProfile`'s derived order: its variant in
/// declaration order, then its revision, whose discriminants ascend with
/// their declaration order.
fn profile_order(profile: LanguageProfile) -> [u8; 2] {
    match profile {
        LanguageProfile::Rust(value) => [0, value as u8],
        LanguageProfile::TypeScript(value) => [1, value as u8],
        LanguageProfile::Python(value) => [2, value as u8],
        LanguageProfile::Go(value) => [3, value as u8],
        LanguageProfile::Java(value) => [4, value as u8],
        LanguageProfile::CSharp(value) => [5, value as u8],
        LanguageProfile::C(value) => [6, value as u8],
        LanguageProfile::Cxx(value) => [7, value as u8],
    }
}

fn push_text(value: &str, output: &mut Vec<u8>) {
    output.extend_from_slice(&u32::try_from(value.len()).unwrap_or(u32::MAX).to_be_bytes());
    output.extend_from_slice(value.as_bytes());
}

fn take_text(bytes: &[u8]) -> Result<(&str, &[u8]), RelationDecodeError> {
    let (length, rest) = take_u32(bytes)?;
    let length = usize::try_from(length).map_err(|_| RelationDecodeError::Malformed)?;
    let (value, rest) = rest
        .split_at_checked(length)
        .ok_or(RelationDecodeError::Malformed)?;
    let value = std::str::from_utf8(value).map_err(|_| RelationDecodeError::Malformed)?;
    Ok((value, rest))
}

fn take_identity(bytes: &[u8]) -> Result<(&[u8], &[u8]), RelationDecodeError> {
    bytes
        .split_at_checked(32)
        .ok_or(RelationDecodeError::Malformed)
}

fn take_u32(bytes: &[u8]) -> Result<(u32, &[u8]), RelationDecodeError> {
    let (value, rest) = bytes
        .split_first_chunk::<4>()
        .ok_or(RelationDecodeError::Malformed)?;
    Ok((u32::from_be_bytes(*value), rest))
}
