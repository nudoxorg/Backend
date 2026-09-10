//! Defines codec behavior for `interface-library`, whose purpose is to own the one shared local library every surface reads, adds to, and searches.
//! This module owns the codec invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! The versioned, checksummed byte grammar of the durable shelf file.
//!
//! Hand written rather than derived: the shelf is read by three processes that may be different
//! builds of this crate, so its grammar must be a decision with a version number on it, not a
//! consequence of field order. Every record is length-prefixed and the whole payload carries a
//! CRC-32, so a torn or truncated file is rejected rather than half-believed.

use compiler_vocabulary::LanguageProfile;
use heart_identity::GenerationId;
use interface_core::{CorrelationId, PackageCompilePhase, PackageEcosystem, SemanticImageAuthority};
use interface_documents::{Census, Count};
use interface_identity::PackageCoordinate;
use server_index_vocabulary::{IndexSnapshotId, LexicalSegmentId};

use crate::{
    PackageCard, PublicationLocator, ShelfEntry, ShelfFailure, ShelfStatus, Timestamp,
    store::ShelfDecodeError,
};

/// Magic that identifies a shelf file at a glance and rejects an unrelated one immediately.
pub(crate) const SHELF_MAGIC: [u8; 6] = *b"NUDXSH";
/// Grammar version. A reader that does not know a version refuses the file rather than guessing.
pub(crate) const SHELF_VERSION: u16 = 1;
/// Content-addressed identity width shared by generations, snapshots, and segments.
const IDENTITY_BYTES: usize = 32;

/// Appends one shelf file's complete bytes.
pub(crate) fn encode(entries: &[ShelfEntry]) -> Vec<u8> {
    let mut body = Vec::new();
    put_u32(&mut body, u32::try_from(entries.len()).unwrap_or(u32::MAX));
    for entry in entries {
        encode_entry(&mut body, entry);
    }
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(&SHELF_MAGIC);
    out.extend_from_slice(&SHELF_VERSION.to_le_bytes());
    put_u32(&mut out, crc32(&body));
    out.extend_from_slice(&body);
    out
}

/// Reads one shelf file's complete bytes.
///
/// # Errors
///
/// Reports the exact byte offset and reason a file was refused; a refused file never yields rows.
pub(crate) fn decode(bytes: &[u8]) -> Result<Vec<ShelfEntry>, ShelfDecodeError> {
    let mut cursor = Cursor::new(bytes);
    let magic = cursor.take(SHELF_MAGIC.len())?;
    if magic != SHELF_MAGIC {
        return Err(ShelfDecodeError::Magic);
    }
    let version = u16::from_le_bytes(cursor.array::<2>()?);
    if version != SHELF_VERSION {
        return Err(ShelfDecodeError::Version {
            observed: version,
            expected: SHELF_VERSION,
        });
    }
    let checksum = cursor.u32()?;
    let body = cursor.rest();
    if crc32(body) != checksum {
        return Err(ShelfDecodeError::Checksum);
    }
    let mut body = Cursor::new(body);
    let count = body.u32()?;
    let mut entries = Vec::new();
    for _ in 0..count {
        entries.push(decode_entry(&mut body)?);
    }
    Ok(entries)
}

fn encode_entry(out: &mut Vec<u8>, entry: &ShelfEntry) {
    encode_coordinate(out, &entry.coordinate);
    put_u64(out, entry.requested_at.0);
    put_u64(out, entry.correlation.0);
    match &entry.status {
        ShelfStatus::Requested => out.push(0),
        ShelfStatus::Compiling { phase } => {
            out.push(1);
            out.push(phase_code(*phase));
        }
        ShelfStatus::Ready { card } => {
            out.push(2);
            encode_card(out, card);
        }
        ShelfStatus::Failed { cause } => {
            out.push(3);
            encode_failure(out, cause);
        }
    }
}

fn decode_entry(cursor: &mut Cursor<'_>) -> Result<ShelfEntry, ShelfDecodeError> {
    let coordinate = decode_coordinate(cursor)?;
    let requested_at = Timestamp(cursor.u64()?);
    let correlation = CorrelationId(cursor.u64()?);
    let status = match cursor.byte()? {
        0 => ShelfStatus::Requested,
        1 => ShelfStatus::Compiling {
            phase: decode_phase(cursor.byte()?)?,
        },
        2 => ShelfStatus::Ready {
            card: decode_card(cursor, coordinate.clone())?,
        },
        3 => ShelfStatus::Failed {
            cause: decode_failure(cursor)?,
        },
        observed => return Err(ShelfDecodeError::Status { observed }),
    };
    Ok(ShelfEntry {
        coordinate,
        status,
        requested_at,
        correlation,
    })
}

fn encode_coordinate(out: &mut Vec<u8>, coordinate: &PackageCoordinate) {
    out.push(ecosystem_code(coordinate.ecosystem));
    put_str(out, coordinate.name.as_str());
    put_str(out, coordinate.version.as_str());
}

fn decode_coordinate(cursor: &mut Cursor<'_>) -> Result<PackageCoordinate, ShelfDecodeError> {
    let ecosystem = decode_ecosystem(cursor.byte()?)?;
    let name = cursor.string()?;
    let version = cursor.string()?;
    PackageCoordinate::new(ecosystem, &name, &version)
        .map_err(|_| ShelfDecodeError::Coordinate { offset: cursor.at })
}

fn encode_card(out: &mut Vec<u8>, card: &PackageCard) {
    let profile: [u8; 2] = card.profile.into();
    out.extend_from_slice(&profile);
    out.extend_from_slice(card.generation.as_ref());
    out.extend_from_slice(card.image.identity.as_ref());
    put_u32(out, card.image.byte_len);
    put_u64(out, card.published_at.0);
    encode_census(out, &card.census);
    out.extend_from_slice(card.publication.snapshot().as_ref());
    put_u32(
        out,
        u32::try_from(card.publication.segments().len()).unwrap_or(u32::MAX),
    );
    for segment in card.publication.segments() {
        out.extend_from_slice(segment.as_ref());
    }
}

fn decode_card(
    cursor: &mut Cursor<'_>,
    coordinate: PackageCoordinate,
) -> Result<PackageCard, ShelfDecodeError> {
    let profile = LanguageProfile::try_from(cursor.array::<2>()?)
        .map_err(|_| ShelfDecodeError::Profile { offset: cursor.at })?;
    let generation = GenerationId::try_from(cursor.take(IDENTITY_BYTES)?)
        .map_err(|_| ShelfDecodeError::Identity { offset: cursor.at })?;
    let identity = interface_core::ArtifactId::try_from(cursor.take(IDENTITY_BYTES)?)
        .map_err(|_| ShelfDecodeError::Identity { offset: cursor.at })?;
    let byte_len = cursor.u32()?;
    let published_at = Timestamp(cursor.u64()?);
    let census = decode_census(cursor)?;
    let snapshot = IndexSnapshotId::try_from(cursor.take(IDENTITY_BYTES)?)
        .map_err(|_| ShelfDecodeError::Identity { offset: cursor.at })?;
    let segment_count = cursor.u32()?;
    let mut segments = Vec::new();
    for _ in 0..segment_count {
        segments.push(
            LexicalSegmentId::try_from(cursor.take(IDENTITY_BYTES)?)
                .map_err(|_| ShelfDecodeError::Identity { offset: cursor.at })?,
        );
    }
    Ok(PackageCard {
        coordinate,
        profile,
        generation,
        image: SemanticImageAuthority { identity, byte_len },
        census,
        published_at,
        publication: PublicationLocator::new(snapshot, segments.into_boxed_slice()),
    })
}

fn encode_census(out: &mut Vec<u8>, census: &Census) {
    put_u32(out, census.entities.0);
    put_u32(out, census.public.0);
    put_u32(out, census.documented.0);
    let kinds: Vec<_> = census.kinds().collect();
    put_u32(out, u32::try_from(kinds.len()).unwrap_or(u32::MAX));
    for row in kinds {
        out.push(kind_code(row.kind));
        put_u32(out, row.count.0);
    }
}

fn decode_census(cursor: &mut Cursor<'_>) -> Result<Census, ShelfDecodeError> {
    let entities = Count(cursor.u32()?);
    let public = Count(cursor.u32()?);
    let documented = Count(cursor.u32()?);
    let rows = cursor.u32()?;
    let mut census = Census::default();
    for _ in 0..rows {
        let kind = decode_kind(cursor.byte()?)?;
        let count = cursor.u32()?;
        for _ in 0..count {
            census.record(kind, false, false);
        }
    }
    census.entities = entities;
    census.public = public;
    census.documented = documented;
    Ok(census)
}

fn encode_failure(out: &mut Vec<u8>, cause: &ShelfFailure) {
    match cause {
        ShelfFailure::PackageNotFound => out.push(0),
        ShelfFailure::EcosystemRootUnavailable => out.push(1),
        ShelfFailure::Compiler { summary } => {
            out.push(2);
            put_str(out, summary);
        }
        ShelfFailure::Publication => out.push(3),
        ShelfFailure::Cancelled => out.push(4),
        ShelfFailure::Orphaned => out.push(5),
        ShelfFailure::Index { summary } => {
            out.push(6);
            put_str(out, summary);
        }
    }
}

fn decode_failure(cursor: &mut Cursor<'_>) -> Result<ShelfFailure, ShelfDecodeError> {
    Ok(match cursor.byte()? {
        0 => ShelfFailure::PackageNotFound,
        1 => ShelfFailure::EcosystemRootUnavailable,
        2 => ShelfFailure::Compiler {
            summary: cursor.string()?.into_boxed_str(),
        },
        3 => ShelfFailure::Publication,
        4 => ShelfFailure::Cancelled,
        5 => ShelfFailure::Orphaned,
        6 => ShelfFailure::Index {
            summary: cursor.string()?.into_boxed_str(),
        },
        observed => return Err(ShelfDecodeError::Failure { observed }),
    })
}

const fn ecosystem_code(ecosystem: PackageEcosystem) -> u8 {
    match ecosystem {
        PackageEcosystem::Cargo => 0,
        PackageEcosystem::Npm => 1,
        PackageEcosystem::Pypi => 2,
        PackageEcosystem::Golang => 3,
        PackageEcosystem::Maven => 4,
        PackageEcosystem::Nuget => 5,
        PackageEcosystem::Generic => 6,
    }
}

const fn decode_ecosystem(code: u8) -> Result<PackageEcosystem, ShelfDecodeError> {
    Ok(match code {
        0 => PackageEcosystem::Cargo,
        1 => PackageEcosystem::Npm,
        2 => PackageEcosystem::Pypi,
        3 => PackageEcosystem::Golang,
        4 => PackageEcosystem::Maven,
        5 => PackageEcosystem::Nuget,
        6 => PackageEcosystem::Generic,
        observed => return Err(ShelfDecodeError::Ecosystem { observed }),
    })
}

const fn phase_code(phase: PackageCompilePhase) -> u8 {
    match phase {
        PackageCompilePhase::Locate => 0,
        PackageCompilePhase::EnterSource => 1,
        PackageCompilePhase::Authority => 2,
        PackageCompilePhase::Lower => 3,
        PackageCompilePhase::Publish => 4,
        PackageCompilePhase::Reopen => 5,
        PackageCompilePhase::Discover => 6,
        PackageCompilePhase::Render => 7,
    }
}

const fn decode_phase(code: u8) -> Result<PackageCompilePhase, ShelfDecodeError> {
    Ok(match code {
        0 => PackageCompilePhase::Locate,
        1 => PackageCompilePhase::EnterSource,
        2 => PackageCompilePhase::Authority,
        3 => PackageCompilePhase::Lower,
        4 => PackageCompilePhase::Publish,
        5 => PackageCompilePhase::Reopen,
        6 => PackageCompilePhase::Discover,
        7 => PackageCompilePhase::Render,
        observed => return Err(ShelfDecodeError::Phase { observed }),
    })
}

const fn kind_code(kind: compiler_ir_vocabulary::EntityKind) -> u8 {
    use compiler_ir_vocabulary::EntityKind;
    match kind {
        EntityKind::Function => 0,
        EntityKind::Constant => 1,
        EntityKind::Record => 2,
        EntityKind::Module => 3,
        EntityKind::Field => 4,
        EntityKind::Alias => 5,
        EntityKind::Trait => 6,
        EntityKind::Implementation => 7,
        EntityKind::Enum => 8,
        EntityKind::Variant => 9,
        EntityKind::Static => 10,
        EntityKind::Reexport => 11,
        EntityKind::Parameter => 12,
        EntityKind::Macro => 13,
        EntityKind::Namespace => 14,
    }
}

const fn decode_kind(
    code: u8,
) -> Result<compiler_ir_vocabulary::EntityKind, ShelfDecodeError> {
    use compiler_ir_vocabulary::EntityKind;
    Ok(match code {
        0 => EntityKind::Function,
        1 => EntityKind::Constant,
        2 => EntityKind::Record,
        3 => EntityKind::Module,
        4 => EntityKind::Field,
        5 => EntityKind::Alias,
        6 => EntityKind::Trait,
        7 => EntityKind::Implementation,
        8 => EntityKind::Enum,
        9 => EntityKind::Variant,
        10 => EntityKind::Static,
        11 => EntityKind::Reexport,
        12 => EntityKind::Parameter,
        13 => EntityKind::Macro,
        14 => EntityKind::Namespace,
        observed => return Err(ShelfDecodeError::Kind { observed }),
    })
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_str(out: &mut Vec<u8>, text: &str) {
    put_u32(out, u32::try_from(text.len()).unwrap_or(u32::MAX));
    out.extend_from_slice(text.as_bytes());
}

/// A forward-only reader that reports the exact offset at which a file stopped making sense.
struct Cursor<'bytes> {
    bytes: &'bytes [u8],
    at: usize,
}

impl<'bytes> Cursor<'bytes> {
    const fn new(bytes: &'bytes [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, width: usize) -> Result<&'bytes [u8], ShelfDecodeError> {
        let end = self.at.saturating_add(width);
        let slice = self
            .bytes
            .get(self.at..end)
            .ok_or(ShelfDecodeError::Truncated {
                offset: self.at,
                required: width,
            })?;
        self.at = end;
        Ok(slice)
    }

    fn array<const WIDTH: usize>(&mut self) -> Result<[u8; WIDTH], ShelfDecodeError> {
        let slice = self.take(WIDTH)?;
        <[u8; WIDTH]>::try_from(slice).map_err(|_| ShelfDecodeError::Truncated {
            offset: self.at,
            required: WIDTH,
        })
    }

    fn byte(&mut self) -> Result<u8, ShelfDecodeError> {
        Ok(self.array::<1>()?[0])
    }

    fn u32(&mut self) -> Result<u32, ShelfDecodeError> {
        Ok(u32::from_le_bytes(self.array::<4>()?))
    }

    fn u64(&mut self) -> Result<u64, ShelfDecodeError> {
        Ok(u64::from_le_bytes(self.array::<8>()?))
    }

    fn string(&mut self) -> Result<String, ShelfDecodeError> {
        let width = usize::try_from(self.u32()?).unwrap_or(usize::MAX);
        let bytes = self.take(width)?;
        core::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| ShelfDecodeError::Utf8 { offset: self.at })
    }

    const fn rest(&self) -> &'bytes [u8] {
        match self.bytes.split_at_checked(self.at) {
            Some((_, rest)) => rest,
            None => &[],
        }
    }
}

/// CRC-32 over the IEEE polynomial, computed bitwise so no table has to be indexed.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}
