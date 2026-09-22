//! Canonical registry journal grammar.

use super::identity::{admit_registry_coordinate, coordinate_from_registry_parts};
use super::{
    AcquisitionError, AcquisitionIntent, AcquisitionReceipt, CanonicalFeedV1, DownloadCount,
    DownloadCountGap, FeedCursor, PackageCoordinate, PackageName, PackageVersion, ProvenanceDigest,
    PublishedArtifactClaim, PublishedPackage, RegistryEcosystem, RegistryId, ReleaseFacts,
    ReleaseStanding, RemoteRegistry, SecurityStanding,
};
use crate::acquisition::RawArchiveObjectId;
use crate::journal::{JournalCodec, JournalDomain, JournalError};
use backend_advisory::AdvisoryPackageDto;
use backend_library::{
    MAX_REGISTRY_FORGE_ASSOCIATION_BYTES,
    MAX_REGISTRY_FORGE_ASSOCIATIONS, MAX_REGISTRY_NATIVE_METADATA_BYTES, RegistryNativeMetadata,
    PackageReference, RegistryForgeAssociation,
};

pub(crate) enum RegistryLog {}
impl JournalDomain for RegistryLog {
    const DOMAIN: u8 = 0x91;
    const TYPE: u16 = 1;
    // Version seven makes normalized forge references, canonical association
    // bytes, and authenticated facts roots explicit journal contracts. Older
    // frames are not guessed at package boundaries because their trailing
    // bytes were ambiguous.
    const VERSION: u8 = 7;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RegistryRecord {
    Prepared(AcquisitionIntent),
    Settled(AcquisitionIntent),
    Committed(AcquisitionIntent, AcquisitionReceipt),
    ForgeLinked {
        coordinate: PackageCoordinate,
        associations: Box<[RegistryForgeAssociation]>,
        facts_root: [u8; 32],
    },
}

impl JournalCodec for RegistryLog {
    type Record = RegistryRecord;
    fn encode(record: &Self::Record, out: &mut Vec<u8>) {
        match record {
            RegistryRecord::Prepared(intent) => {
                out.push(1);
                put_intent(out, intent);
            }
            RegistryRecord::Settled(intent) => {
                out.push(2);
                put_intent(out, intent);
            }
            RegistryRecord::Committed(intent, receipt) => {
                out.push(3);
                put_intent(out, intent);
                put_cursor(out, receipt.base);
                put_cursor(out, receipt.target);
                out.extend_from_slice(&receipt.facts_root);
                put_u32(out, receipt.packages.len());
                for package in &receipt.packages {
                    put_package(out, package);
                }
            }
            RegistryRecord::ForgeLinked {
                coordinate,
                associations,
                facts_root,
            } => {
                debug_assert!(associations
                    .windows(2)
                    .all(|pair| pair[0].facts_version < pair[1].facts_version));
                out.push(4);
                put_text(out, coordinate.as_str());
                out.extend_from_slice(facts_root);
                put_u32(out, associations.len());
                for association in associations {
                    put_association(out, association);
                }
            }
        }
    }
    fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError> {
        Self::decode_record(bytes).map_err(|_| JournalError::Corrupt("registry record"))
    }
}

impl RegistryLog {
    pub(crate) fn decode_record(bytes: &[u8]) -> Result<RegistryRecord, AcquisitionError> {
        let mut at = 0;
        let tag = take_byte(bytes, &mut at)?;
        let record = match tag {
            1 => RegistryRecord::Prepared(read_intent(bytes, &mut at)?),
            2 => RegistryRecord::Settled(read_intent(bytes, &mut at)?),
            3 => {
                let intent = read_intent(bytes, &mut at)?;
                let base = read_cursor(bytes, &mut at)?;
                let target = read_cursor(bytes, &mut at)?;
                let facts_root = take_array(bytes, &mut at)?;
                if facts_root == [0; 32] {
                    return Err(AcquisitionError::CorruptJournal);
                }
                let count = usize::try_from(read_u32(bytes, &mut at)?)
                    .map_err(|_| AcquisitionError::Bounds)?;
                if count > 4096 {
                    return Err(AcquisitionError::Bounds);
                }
                let mut packages = Vec::with_capacity(count);
                for _ in 0..count {
                    packages.push(read_package(bytes, &mut at)?);
                }
                let receipt = AcquisitionReceipt {
                    effect: intent.key,
                    base,
                    target,
                    packages,
                    facts_root,
                };
                RegistryRecord::Committed(intent, receipt)
            }
            4 => {
                let coordinate = PackageCoordinate::parse(read_text(bytes, &mut at)?)
                    .map_err(|_| AcquisitionError::CorruptJournal)?;
                let facts_root = take_array(bytes, &mut at)?;
                if facts_root == [0; 32] {
                    return Err(AcquisitionError::CorruptJournal);
                }
                let count = usize::try_from(read_u32(bytes, &mut at)?)
                    .map_err(|_| AcquisitionError::Bounds)?;
                if count > MAX_REGISTRY_FORGE_ASSOCIATIONS {
                    return Err(AcquisitionError::Bounds);
                }
                let registry = PackageReference::Purl(coordinate.clone());
                let mut associations = Vec::with_capacity(count);
                for _ in 0..count {
                    let length = usize::try_from(read_u32(bytes, &mut at)?)
                        .map_err(|_| AcquisitionError::Bounds)?;
                    if length > MAX_REGISTRY_FORGE_ASSOCIATION_BYTES {
                        return Err(AcquisitionError::Bounds);
                    }
                    let association = RegistryForgeAssociation::decode_canonical(
                        take(bytes, &mut at, length)?,
                    )
                    .map_err(|_| AcquisitionError::CorruptJournal)?;
                    if association.admit_for_registry(&registry).is_err() {
                        return Err(AcquisitionError::CorruptJournal);
                    }
                    associations.push(association);
                }
                if associations
                    .windows(2)
                    .any(|pair| pair[0].facts_version >= pair[1].facts_version)
                {
                    return Err(AcquisitionError::CorruptJournal);
                }
                RegistryRecord::ForgeLinked {
                    coordinate,
                    associations: associations.into_boxed_slice(),
                    facts_root,
                }
            }
            _ => return Err(AcquisitionError::CorruptJournal),
        };
        if at != bytes.len() {
            return Err(AcquisitionError::CorruptJournal);
        }
        Ok(record)
    }
}

fn put_intent(out: &mut Vec<u8>, value: &AcquisitionIntent) {
    out.extend_from_slice(value.key.as_bytes());
    put_cursor(out, value.cursor);
    put_u64(out, u64::try_from(value.max_items).unwrap_or(u64::MAX));
}
fn read_intent(bytes: &[u8], at: &mut usize) -> Result<AcquisitionIntent, AcquisitionError> {
    let stored = take_array(bytes, at)?;
    let cursor = read_cursor(bytes, at)?;
    let max_items = usize::try_from(read_u64(bytes, at)?).map_err(|_| AcquisitionError::Bounds)?;
    let intent = AcquisitionIntent::new(cursor, max_items);
    if intent.key.as_bytes() != &stored {
        return Err(AcquisitionError::CorruptJournal);
    }
    Ok(intent)
}
fn put_cursor(out: &mut Vec<u8>, value: FeedCursor<RemoteRegistry, CanonicalFeedV1>) {
    out.extend_from_slice(&value.registry().as_bytes());
    put_u64(out, value.sequence());
    out.extend_from_slice(&value.token());
}
fn read_cursor(
    bytes: &[u8],
    at: &mut usize,
) -> Result<FeedCursor<RemoteRegistry, CanonicalFeedV1>, AcquisitionError> {
    Ok(FeedCursor::from_parts(
        RegistryId::from_bytes(take_array(bytes, at)?),
        read_u64(bytes, at)?,
        take_array(bytes, at)?,
    ))
}
fn put_package(out: &mut Vec<u8>, value: &PublishedPackage) {
    debug_assert!(
        value
            .forge_source_ids
            .windows(2)
            .all(|window| window[0] < window[1]),
        "forge source references must be sorted and unique before encoding"
    );
    let coordinate = &value.registry;
    let name = coordinate.qualified_name();
    out.push(coordinate.ecosystem() as u8);
    put_text(out, name.as_str());
    put_text(out, coordinate.version().as_str());
    out.extend_from_slice(value.raw_object.as_bytes());
    out.extend_from_slice(&value.artifact.as_bytes());
    put_u64(out, value.bytes);
    out.extend_from_slice(&value.provenance.as_bytes());
    out.extend_from_slice(&value.upstream_integrity);
    let native_metadata = value.native_metadata.encode_canonical();
    put_u32(out, native_metadata.len());
    out.extend_from_slice(&native_metadata);
    put_facts(out, value.facts);
    put_json(out, &value.advisory);
    put_json(out, &value.dependency_facts);
    put_u32(out, value.forge_source_ids.len());
    for id in &value.forge_source_ids {
        out.extend_from_slice(id);
    }
}
fn read_package(bytes: &[u8], at: &mut usize) -> Result<PublishedPackage, AcquisitionError> {
    let ecosystem = RegistryEcosystem::try_from(take_byte(bytes, at)?)
        .map_err(|_| AcquisitionError::CorruptJournal)?;
    let name = PackageName::new(read_text(bytes, at)?)?;
    let version = PackageVersion::new(read_text(bytes, at)?)?;
    let raw_object = RawArchiveObjectId::from_encoded(take_array(bytes, at)?);
    let digest = take_array(bytes, at)?;
    let byte_count = read_u64(bytes, at)?;
    let provenance = take_array(bytes, at)?;
    let upstream_integrity = take_array(bytes, at)?;
    let native_metadata_len =
        usize::try_from(read_u32(bytes, at)?).map_err(|_| AcquisitionError::Bounds)?;
    if native_metadata_len > MAX_REGISTRY_NATIVE_METADATA_BYTES {
        return Err(AcquisitionError::Bounds);
    }
    let native_metadata =
        RegistryNativeMetadata::decode_canonical(take(bytes, at, native_metadata_len)?)
            .map_err(|_| AcquisitionError::CorruptJournal)?;
    let native_metadata_version = native_metadata
        .identity()
        .map_err(|_| AcquisitionError::CorruptJournal)?;
    let facts = read_facts(bytes, at, native_metadata_version)?;
    let advisory_len =
        usize::try_from(read_u32(bytes, at)?).map_err(|_| AcquisitionError::Bounds)?;
    if advisory_len > 4 * 1024 * 1024 {
        return Err(AcquisitionError::Bounds);
    }
    let advisory = serde_json::from_slice::<AdvisoryPackageDto>(take(bytes, at, advisory_len)?)
        .map_err(|_| AcquisitionError::CorruptJournal)?;
    let dependency_len =
        usize::try_from(read_u32(bytes, at)?).map_err(|_| AcquisitionError::Bounds)?;
    if dependency_len > 16 * 1024 * 1024 {
        return Err(AcquisitionError::Bounds);
    }
    let dependency_facts = serde_json::from_slice(take(bytes, at, dependency_len)?)
        .map_err(|_| AcquisitionError::CorruptJournal)?;
    let coordinate = coordinate_from_registry_parts(ecosystem, name.as_str(), version.as_str())?;
    let registry = admit_registry_coordinate(&coordinate)?;
    let forge_count = usize::try_from(read_u32(bytes, at)?)
        .map_err(|_| AcquisitionError::Bounds)?;
    if forge_count > MAX_REGISTRY_FORGE_ASSOCIATIONS {
        return Err(AcquisitionError::Bounds);
    }
    let mut ids = Vec::with_capacity(forge_count);
    for _ in 0..forge_count {
        ids.push(take_array(bytes, at)?);
    }
    if ids.windows(2).any(|window| window[0] >= window[1]) {
        return Err(AcquisitionError::CorruptJournal);
    }
    let forge_source_ids = ids.into_boxed_slice();
    Ok(PublishedPackage {
        coordinate,
        registry,
        artifact: PublishedArtifactClaim::from_journal(digest),
        raw_object,
        bytes: byte_count,
        provenance: ProvenanceDigest::from_journal(provenance),
        upstream_integrity,
        native_metadata,
        forge_source_ids,
        facts,
        advisory,
        dependency_facts,
    })
}

fn put_facts(out: &mut Vec<u8>, facts: ReleaseFacts) {
    out.push(facts.standing() as u8);
    match facts.downloads() {
        DownloadCount::Exact(value) => {
            out.push(0);
            put_u64(out, value);
        }
        DownloadCount::Approximate(value) => {
            out.push(1);
            put_u64(out, value);
        }
        DownloadCount::NotReported(reason) => {
            out.push(2);
            out.push(reason as u8);
        }
    }
    match facts.security() {
        SecurityStanding::Unassessed => out.push(0),
        SecurityStanding::NoKnownAdvisory => out.push(1),
        SecurityStanding::Affected {
            advisories,
            maximum_severity,
        } => {
            out.push(2);
            out.extend_from_slice(&advisories.to_be_bytes());
            out.push(maximum_severity.min(4));
        }
    }
    out.extend_from_slice(&facts.native_metadata_version());
}

fn read_facts(
    bytes: &[u8],
    at: &mut usize,
    native_metadata_version: [u8; 32],
) -> Result<ReleaseFacts, AcquisitionError> {
    let standing = ReleaseStanding::try_from(take_byte(bytes, at)?)
        .map_err(|()| AcquisitionError::CorruptJournal)?;
    let downloads = match take_byte(bytes, at)? {
        0 => DownloadCount::Exact(read_u64(bytes, at)?),
        1 => DownloadCount::Approximate(read_u64(bytes, at)?),
        2 => DownloadCount::NotReported(
            DownloadCountGap::try_from(take_byte(bytes, at)?)
                .map_err(|()| AcquisitionError::CorruptJournal)?,
        ),
        _ => return Err(AcquisitionError::CorruptJournal),
    };
    let security = match take_byte(bytes, at)? {
        0 => SecurityStanding::Unassessed,
        1 => SecurityStanding::NoKnownAdvisory,
        2 => {
            let advisories = u32::from_be_bytes(take_array(bytes, at)?);
            let maximum_severity = take_byte(bytes, at)?;
            if maximum_severity > 4 {
                return Err(AcquisitionError::CorruptJournal);
            }
            SecurityStanding::Affected {
                advisories,
                maximum_severity,
            }
        }
        _ => return Err(AcquisitionError::CorruptJournal),
    };
    let stored_native_metadata_version = take_array(bytes, at)?;
    if stored_native_metadata_version != native_metadata_version {
        return Err(AcquisitionError::CorruptJournal);
    }
    Ok(ReleaseFacts::from_wire(
        standing,
        downloads,
        security,
        native_metadata_version,
    ))
}
fn put_text(out: &mut Vec<u8>, value: &str) {
    put_u32(out, value.len());
    out.extend_from_slice(value.as_bytes());
}

fn put_json<T: serde::Serialize>(out: &mut Vec<u8>, value: &T) {
    match serde_json::to_vec(value) {
        Ok(bytes) => {
            put_u32(out, bytes.len());
            out.extend_from_slice(&bytes);
        }
        Err(_) => {
            // JournalCodec is intentionally infallible. A serialization
            // failure is encoded as an impossible bounded length so the
            // reader rejects the record instead of silently dropping facts.
            out.extend_from_slice(&u32::MAX.to_be_bytes());
        }
    }
}

fn put_association(out: &mut Vec<u8>, value: &RegistryForgeAssociation) {
    match value.encode_canonical() {
        Ok(bytes) => {
            put_u32(out, bytes.len());
            out.extend_from_slice(&bytes);
        }
        Err(_) => {
            out.extend_from_slice(&u32::MAX.to_be_bytes());
        }
    }
}
fn read_text(bytes: &[u8], at: &mut usize) -> Result<String, AcquisitionError> {
    let length = usize::try_from(read_u32(bytes, at)?).map_err(|_| AcquisitionError::Bounds)?;
    if length > 256 {
        return Err(AcquisitionError::Bounds);
    }
    let slice = take(bytes, at, length)?;
    String::from_utf8(slice.to_vec()).map_err(|_| AcquisitionError::CorruptJournal)
}
fn put_u32(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&u32::try_from(value).unwrap_or(u32::MAX).to_be_bytes());
}
fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}
fn read_u32(bytes: &[u8], at: &mut usize) -> Result<u32, AcquisitionError> {
    Ok(u32::from_be_bytes(take_array(bytes, at)?))
}
fn read_u64(bytes: &[u8], at: &mut usize) -> Result<u64, AcquisitionError> {
    Ok(u64::from_be_bytes(take_array(bytes, at)?))
}
fn take_byte(bytes: &[u8], at: &mut usize) -> Result<u8, AcquisitionError> {
    let value = *bytes.get(*at).ok_or(AcquisitionError::CorruptJournal)?;
    *at = at.checked_add(1).ok_or(AcquisitionError::Bounds)?;
    Ok(value)
}
fn take<'a>(bytes: &'a [u8], at: &mut usize, length: usize) -> Result<&'a [u8], AcquisitionError> {
    let end = at.checked_add(length).ok_or(AcquisitionError::Bounds)?;
    let value = bytes
        .get(*at..end)
        .ok_or(AcquisitionError::CorruptJournal)?;
    *at = end;
    Ok(value)
}
fn take_array<const N: usize>(bytes: &[u8], at: &mut usize) -> Result<[u8; N], AcquisitionError> {
    take(bytes, at, N)?
        .try_into()
        .map_err(|_| AcquisitionError::CorruptJournal)
}
