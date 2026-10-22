//! Bounded integrity witness for a Cargo registry `.crate` archive.
//!
//! The archive checksum binds these member digests to the exact locked
//! registry object. This module never reads a filesystem path; callers must
//! obtain archive and source bytes through their existing no-follow readers.

use super::{
    MAX_EXTRACTED_FILE_BYTES, MAX_EXTRACTED_FILES, MAX_EXTRACTED_TOTAL_BYTES, TAR_BLOCK_BYTES,
    admit_expansion, pax_path, validate_tar_checksum,
};
use flate2::bufread::GzDecoder;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{Cursor, Read};

const MAX_ARCHIVE_BYTES: usize = MAX_EXTRACTED_TOTAL_BYTES;
const MAX_MEMBER_PATH_BYTES: usize = 4 * 1024;
// Conservatively account for the BTreeMap node allocation as well as its
// individually owned key and digest bytes.
const MEMBER_MAP_NODE_ACCOUNTING_BYTES: usize = 768;
const WITNESS_BASE_ACCOUNTING_BYTES: usize = 128;
const PARSER_SCRATCH_ACCOUNTING_BYTES: usize = 6 * (MAX_MEMBER_PATH_BYTES + 1);
const WITNESS_HASH_DOMAIN: &[u8] = b"nudox.cargo-crate-archive-witness.v1\0";

/// Bounded error while constructing or checking a registry archive witness.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(in crate::builtin) enum CrateArchiveWitnessError {
    /// The bytes do not match the exact checksum recorded by Cargo.lock.
    ArchiveDigestMismatch,
    /// The archive is malformed, ambiguous, unsafe, or outside supported tar forms.
    InvalidArchive,
    /// A count, path, retained witness, expansion, or payload exceeded its limit.
    Bounds,
    /// The caller's cancellation or owner-budget checkpoint stopped processing.
    Interrupted(String),
}

impl fmt::Display for CrateArchiveWitnessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ArchiveDigestMismatch => {
                formatter.write_str("Cargo registry archive SHA-256 does not match the lockfile")
            }
            Self::InvalidArchive => {
                formatter.write_str("Cargo registry archive is malformed or unsupported")
            }
            Self::Bounds => formatter.write_str("Cargo registry archive witness exceeded a bound"),
            Self::Interrupted(reason) => {
                write!(
                    formatter,
                    "Cargo registry archive witness was interrupted: {reason}"
                )
            }
        }
    }
}

impl std::error::Error for CrateArchiveWitnessError {}

/// Immutable identities of regular files in one exact Cargo registry archive.
///
/// `members` contains canonical slash-separated paths relative to the single
/// `<crate>-<version>/` archive root. `retained_bytes` includes owned path
/// bytes, each digest, and a conservative per-member BTreeMap allocation.
#[derive(Debug, Eq, PartialEq)]
pub(in crate::builtin) struct CrateArchiveWitness {
    archive_sha256: [u8; 32],
    witness_hash: [u8; 32],
    retained_bytes: usize,
    members: BTreeMap<String, [u8; 32]>,
}

impl CrateArchiveWitness {
    /// Authenticates and indexes a bounded gzip/tar `.crate` archive.
    ///
    /// All members must be inside the exact crate-name/version root. Only
    /// regular files are returned; directories are validated and duplicate
    /// checked but are not included in the member map. `checkpoint` runs
    /// during archive/member hashing, decompression, and between tar members
    /// so the caller can apply its existing cancellation and owner-budget
    /// policy.
    #[cfg(test)]
    pub(in crate::builtin) fn capture(
        crate_name: &str,
        version: &str,
        archive: &[u8],
        expected_archive_sha256: [u8; 32],
        checkpoint: &mut impl FnMut() -> Result<(), String>,
    ) -> Result<Self, CrateArchiveWitnessError> {
        Self::capture_with_limit(
            crate_name,
            version,
            archive,
            expected_archive_sha256,
            MAX_EXTRACTED_TOTAL_BYTES,
            checkpoint,
        )
    }

    /// Captures a witness under a caller-supplied peak working-memory budget,
    /// including the borrowed archive length,
    /// expanded tar capacity, parser indexes, and the returned member map.
    /// The budget may be smaller than, but never larger than, the existing
    /// extracted-total-byte limit. Callers should pass their remaining
    /// aggregate budget after accounting for already-retained witnesses.
    pub(in crate::builtin) fn capture_with_limit(
        crate_name: &str,
        version: &str,
        archive: &[u8],
        expected_archive_sha256: [u8; 32],
        max_working_bytes: usize,
        checkpoint: &mut impl FnMut() -> Result<(), String>,
    ) -> Result<Self, CrateArchiveWitnessError> {
        checkpoint().map_err(CrateArchiveWitnessError::Interrupted)?;
        if max_working_bytes > MAX_EXTRACTED_TOTAL_BYTES
            || archive.is_empty()
            || archive.len() > MAX_ARCHIVE_BYTES
            || archive.len() > max_working_bytes
        {
            return Err(CrateArchiveWitnessError::Bounds);
        }
        let archive_sha256 = sha256_checked(archive, checkpoint)?;
        if archive_sha256 != expected_archive_sha256 {
            return Err(CrateArchiveWitnessError::ArchiveDigestMismatch);
        }
        let root = crate_root(crate_name, version)?;
        let tar = decompress_gzip(archive, max_working_bytes, checkpoint)?;
        admit_expansion(archive.len(), tar.len()).map_err(map_parent_error)?;
        let members = parse_tar(
            &root,
            &tar,
            tar.capacity(),
            archive.len(),
            max_working_bytes,
            checkpoint,
        )?;
        let retained_bytes =
            members
                .iter()
                .try_fold(WITNESS_BASE_ACCOUNTING_BYTES, |total, (path, _)| {
                    total
                        .checked_add(path.capacity())
                        .and_then(|sum| sum.checked_add(32))
                        .and_then(|sum| sum.checked_add(MEMBER_MAP_NODE_ACCOUNTING_BYTES))
                        .ok_or(CrateArchiveWitnessError::Bounds)
                })?;
        if retained_bytes > MAX_EXTRACTED_TOTAL_BYTES {
            return Err(CrateArchiveWitnessError::Bounds);
        }

        let witness_hash = witness_hash(&root, archive_sha256, &members);
        Ok(Self {
            archive_sha256,
            witness_hash,
            retained_bytes,
            members,
        })
    }

    /// Returns the authenticated compressed archive digest.
    pub(in crate::builtin) fn archive_sha256(&self) -> &[u8; 32] {
        &self.archive_sha256
    }

    /// Returns the deterministic commitment to the archive and member map.
    pub(in crate::builtin) fn witness_hash(&self) -> &[u8; 32] {
        &self.witness_hash
    }

    /// Returns a conservative retained-memory bound for the witness map.
    pub(in crate::builtin) fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// Returns every regular member digest keyed by canonical package-relative path.
    pub(in crate::builtin) fn members(&self) -> &BTreeMap<String, [u8; 32]> {
        &self.members
    }

    /// Returns a member digest when the exact canonical relative path exists.
    pub(in crate::builtin) fn member_digest(&self, relative_path: &str) -> Option<&[u8; 32]> {
        if !valid_relative_path(relative_path) {
            return None;
        }
        self.members.get(relative_path)
    }

    /// Checks caller-supplied bytes against the digest of one exact member.
    ///
    /// The caller remains responsible for a no-follow read and for proving
    /// that the path it opened is the selected package-root child.
    pub(in crate::builtin) fn matches(&self, relative_path: &str, bytes: &[u8]) -> bool {
        let mut checkpoint = || Ok(());
        self.matches_checked(relative_path, bytes, &mut checkpoint)
            .unwrap_or(false)
    }

    /// Checks caller-supplied bytes while honoring the caller's cancellation
    /// and owner-budget checkpoint during hashing.
    pub(in crate::builtin) fn matches_checked(
        &self,
        relative_path: &str,
        bytes: &[u8],
        checkpoint: &mut impl FnMut() -> Result<(), String>,
    ) -> Result<bool, CrateArchiveWitnessError> {
        checkpoint().map_err(CrateArchiveWitnessError::Interrupted)?;
        let Some(expected) = self.member_digest(relative_path) else {
            return Ok(false);
        };
        Ok(sha256_checked(bytes, checkpoint)? == *expected)
    }
}

fn crate_root(crate_name: &str, version: &str) -> Result<String, CrateArchiveWitnessError> {
    if !valid_root_component(crate_name) || !valid_root_component(version) {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    let root = format!("{crate_name}-{version}");
    if root.len() > MAX_MEMBER_PATH_BYTES {
        return Err(CrateArchiveWitnessError::Bounds);
    }
    Ok(root)
}

fn valid_root_component(component: &str) -> bool {
    !component.is_empty()
        && component != "."
        && component != ".."
        && !component.contains(['/', '\\', '\0', ':'])
        && !component.bytes().any(|byte| byte.is_ascii_control())
}

fn valid_relative_path(path: &str) -> bool {
    if path.is_empty()
        || path.len() > MAX_MEMBER_PATH_BYTES
        || path.starts_with('/')
        || path.contains(['\\', '\0'])
        || (path.len() >= 2
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':')
        || path.bytes().any(|byte| byte.is_ascii_control())
    {
        return false;
    }
    path.split('/')
        .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn decompress_gzip(
    archive: &[u8],
    max_working_bytes: usize,
    checkpoint: &mut impl FnMut() -> Result<(), String>,
) -> Result<Vec<u8>, CrateArchiveWitnessError> {
    if !archive.starts_with(&[0x1f, 0x8b]) {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    let cursor = Cursor::new(archive);
    let mut decoder = GzDecoder::new(cursor);
    let mut expanded = Vec::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        checkpoint().map_err(CrateArchiveWitnessError::Interrupted)?;
        let count = decoder
            .read(&mut buffer)
            .map_err(|_| CrateArchiveWitnessError::InvalidArchive)?;
        if count == 0 {
            break;
        }
        let next_len = expanded
            .len()
            .checked_add(count)
            .ok_or(CrateArchiveWitnessError::Bounds)?;
        if archive
            .len()
            .checked_add(next_len)
            .is_none_or(|working| working > max_working_bytes)
        {
            return Err(CrateArchiveWitnessError::Bounds);
        }
        expanded
            .try_reserve_exact(count)
            .map_err(|_| CrateArchiveWitnessError::Bounds)?;
        if archive
            .len()
            .checked_add(expanded.capacity())
            .is_none_or(|working| working > max_working_bytes)
        {
            return Err(CrateArchiveWitnessError::Bounds);
        }
        expanded.extend_from_slice(&buffer[..count]);
    }
    let consumed = usize::try_from(decoder.get_ref().position())
        .map_err(|_| CrateArchiveWitnessError::Bounds)?;
    if archive
        .get(consumed..)
        .is_none_or(|trailing| trailing.iter().any(|byte| *byte != 0))
    {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    Ok(expanded)
}

fn parse_tar(
    root: &str,
    bytes: &[u8],
    bytes_capacity: usize,
    archive_len: usize,
    max_working_bytes: usize,
    checkpoint: &mut impl FnMut() -> Result<(), String>,
) -> Result<BTreeMap<String, [u8; 32]>, CrateArchiveWitnessError> {
    let mut offset = 0usize;
    let mut entries = 0usize;
    let mut pending_path: Option<String> = None;
    let mut members = BTreeMap::new();
    let mut directories = BTreeSet::new();
    let mut source_bytes = 0usize;
    let mut working_bytes = archive_len
        .checked_add(bytes_capacity)
        .and_then(|bytes| bytes.checked_add(WITNESS_BASE_ACCOUNTING_BYTES))
        .and_then(|bytes| bytes.checked_add(PARSER_SCRATCH_ACCOUNTING_BYTES))
        .ok_or(CrateArchiveWitnessError::Bounds)?;
    if working_bytes > max_working_bytes {
        return Err(CrateArchiveWitnessError::Bounds);
    }

    loop {
        checkpoint().map_err(CrateArchiveWitnessError::Interrupted)?;
        let header_end = offset
            .checked_add(TAR_BLOCK_BYTES)
            .ok_or(CrateArchiveWitnessError::Bounds)?;
        let header = bytes
            .get(offset..header_end)
            .ok_or(CrateArchiveWitnessError::InvalidArchive)?;
        if header.iter().all(|byte| *byte == 0) {
            let trailer = bytes
                .get(offset..)
                .ok_or(CrateArchiveWitnessError::InvalidArchive)?;
            if trailer.len() < TAR_BLOCK_BYTES * 2
                || trailer.len() % TAR_BLOCK_BYTES != 0
                || trailer.iter().any(|byte| *byte != 0)
                || pending_path.is_some()
            {
                return Err(CrateArchiveWitnessError::InvalidArchive);
            }
            break;
        }
        strict_tar_octal(&header[148..156])?;
        validate_tar_checksum(header).map_err(map_parent_error)?;
        let name = witness_tar_name(header)?;
        let size = strict_tar_octal(&header[124..136])?;
        if size > MAX_EXTRACTED_FILE_BYTES {
            return Err(CrateArchiveWitnessError::Bounds);
        }
        let data_start = header_end;
        let data_end = data_start
            .checked_add(size)
            .ok_or(CrateArchiveWitnessError::Bounds)?;
        let data = bytes
            .get(data_start..data_end)
            .ok_or(CrateArchiveWitnessError::InvalidArchive)?;
        let padded = size
            .checked_add(TAR_BLOCK_BYTES - 1)
            .ok_or(CrateArchiveWitnessError::Bounds)?
            / TAR_BLOCK_BYTES
            * TAR_BLOCK_BYTES;
        offset = data_start
            .checked_add(padded)
            .ok_or(CrateArchiveWitnessError::Bounds)?;
        if offset > bytes.len() {
            return Err(CrateArchiveWitnessError::InvalidArchive);
        }
        entries = entries
            .checked_add(1)
            .ok_or(CrateArchiveWitnessError::Bounds)?;
        if entries > MAX_EXTRACTED_FILES {
            return Err(CrateArchiveWitnessError::Bounds);
        }

        match header[156] {
            0 | b'0' => {
                let path = pending_path.take().unwrap_or(name);
                let relative = relative_member_path(root, &path, false)?;
                if members.contains_key(&relative)
                    || directories.contains(&relative)
                    || has_file_ancestor(&members, &relative)
                    || map_has_descendant(&members, &relative)
                    || set_has_descendant(&directories, &relative)
                {
                    return Err(CrateArchiveWitnessError::InvalidArchive);
                }
                source_bytes = source_bytes
                    .checked_add(data.len())
                    .ok_or(CrateArchiveWitnessError::Bounds)?;
                if source_bytes > MAX_EXTRACTED_TOTAL_BYTES {
                    return Err(CrateArchiveWitnessError::Bounds);
                }
                let digest = sha256_checked(data, checkpoint)?;
                let additional = relative
                    .capacity()
                    .checked_add(32)
                    .and_then(|value| value.checked_add(MEMBER_MAP_NODE_ACCOUNTING_BYTES))
                    .ok_or(CrateArchiveWitnessError::Bounds)?;
                working_bytes = checked_working_add(working_bytes, additional, max_working_bytes)?;
                members.insert(relative, digest);
            }
            b'5' => {
                if size != 0 {
                    return Err(CrateArchiveWitnessError::InvalidArchive);
                }
                let path = pending_path.take().unwrap_or(name);
                let relative = relative_member_path(root, &path, true)?;
                if members.contains_key(&relative)
                    || directories.contains(&relative)
                    || has_file_ancestor(&members, &relative)
                {
                    return Err(CrateArchiveWitnessError::InvalidArchive);
                }
                let additional = relative
                    .capacity()
                    .checked_add(MEMBER_MAP_NODE_ACCOUNTING_BYTES)
                    .ok_or(CrateArchiveWitnessError::Bounds)?;
                working_bytes = checked_working_add(working_bytes, additional, max_working_bytes)?;
                directories.insert(relative);
            }
            b'x' => {
                if pending_path.is_some() {
                    return Err(CrateArchiveWitnessError::InvalidArchive);
                }
                validate_pax_path_records(data, checkpoint)?;
                pending_path = pax_path(data).map_err(map_parent_error)?;
                if pending_path.as_deref().is_some_and(|path| {
                    relative_member_path(root, path, false).is_err()
                        && relative_member_path(root, path, true).is_err()
                }) {
                    return Err(CrateArchiveWitnessError::InvalidArchive);
                }
            }
            // Global path attributes would override unrelated later members;
            // reject them instead of extending authority beyond one header.
            b'g' => return Err(CrateArchiveWitnessError::InvalidArchive),
            b'L' => {
                if pending_path.is_some() {
                    return Err(CrateArchiveWitnessError::InvalidArchive);
                }
                let end = data
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(data.len());
                if end > MAX_MEMBER_PATH_BYTES {
                    return Err(CrateArchiveWitnessError::Bounds);
                }
                if data[end..].iter().any(|byte| *byte != 0) {
                    return Err(CrateArchiveWitnessError::InvalidArchive);
                }
                let path = std::str::from_utf8(&data[..end])
                    .map_err(|_| CrateArchiveWitnessError::InvalidArchive)?
                    .to_owned();
                relative_member_path(root, &path, false)?;
                pending_path = Some(path);
            }
            // Link, sparse, device, and all other special member types are
            // rejected. Their names cannot establish source-byte authority.
            _ => return Err(CrateArchiveWitnessError::InvalidArchive),
        }
    }
    if members.is_empty() {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    Ok(members)
}

fn relative_member_path(
    root: &str,
    path: &str,
    directory: bool,
) -> Result<String, CrateArchiveWitnessError> {
    if path.len() > MAX_MEMBER_PATH_BYTES
        || path.starts_with('/')
        || path.contains(['\\', '\0'])
        || (path.len() >= 2
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':')
        || path.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    let path = if directory {
        path.strip_suffix('/').unwrap_or(path)
    } else {
        path
    };
    if path == root && directory {
        return Ok(String::new());
    }
    let prefix = format!("{root}/");
    let relative = path
        .strip_prefix(&prefix)
        .ok_or(CrateArchiveWitnessError::InvalidArchive)?;
    if !valid_relative_path(relative) {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    Ok(relative.to_owned())
}

fn witness_tar_name(header: &[u8]) -> Result<String, CrateArchiveWitnessError> {
    if header.len() != TAR_BLOCK_BYTES {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    let name = strict_tar_field(&header[..100])?;
    if name.is_empty() {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    match (&header[257..263], &header[263..265]) {
        (b"ustar\0", b"00") => {
            let prefix = strict_tar_field(&header[345..500])?;
            if prefix.is_empty() {
                Ok(name)
            } else {
                Ok(format!("{prefix}/{name}"))
            }
        }
        // GNU reuses the USTAR prefix area for metadata. It must never be
        // interpreted as part of the member path.
        (b"ustar ", b" \0") => Ok(name),
        // V7 and unknown variants have no verified field interpretation here.
        _ => Err(CrateArchiveWitnessError::InvalidArchive),
    }
}

fn strict_tar_field(field: &[u8]) -> Result<String, CrateArchiveWitnessError> {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    if field[end..].iter().any(|byte| *byte != 0) {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    std::str::from_utf8(&field[..end])
        .map(str::to_owned)
        .map_err(|_| CrateArchiveWitnessError::InvalidArchive)
}

fn strict_tar_octal(field: &[u8]) -> Result<usize, CrateArchiveWitnessError> {
    let digit_end = field
        .iter()
        .position(|byte| *byte == 0 || *byte == b' ')
        .unwrap_or(field.len());
    if field[digit_end..]
        .iter()
        .any(|byte| *byte != 0 && *byte != b' ')
    {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    if digit_end == 0 {
        return if field.iter().all(|byte| *byte == 0 || *byte == b' ') {
            Ok(0)
        } else {
            Err(CrateArchiveWitnessError::InvalidArchive)
        };
    }
    if !field[..digit_end]
        .iter()
        .all(|byte| (b'0'..=b'7').contains(byte))
    {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    let digits = std::str::from_utf8(&field[..digit_end])
        .map_err(|_| CrateArchiveWitnessError::InvalidArchive)?;
    usize::from_str_radix(digits, 8).map_err(|_| CrateArchiveWitnessError::Bounds)
}

fn has_file_ancestor(files: &BTreeMap<String, [u8; 32]>, path: &str) -> bool {
    let mut boundary = 0;
    while let Some(offset) = path[boundary..].find('/') {
        boundary += offset;
        if files.contains_key(&path[..boundary]) {
            return true;
        }
        boundary += 1;
    }
    false
}

fn validate_pax_path_records(
    bytes: &[u8],
    checkpoint: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), CrateArchiveWitnessError> {
    let mut offset = 0usize;
    let mut saw_path = false;
    while offset < bytes.len() {
        checkpoint().map_err(CrateArchiveWitnessError::Interrupted)?;
        let Some(space) = bytes[offset..].iter().position(|byte| *byte == b' ') else {
            return Err(CrateArchiveWitnessError::InvalidArchive);
        };
        let length = std::str::from_utf8(&bytes[offset..offset + space])
            .map_err(|_| CrateArchiveWitnessError::InvalidArchive)?
            .parse::<usize>()
            .map_err(|_| CrateArchiveWitnessError::InvalidArchive)?;
        let end = offset
            .checked_add(length)
            .ok_or(CrateArchiveWitnessError::Bounds)?;
        if length == 0 || end > bytes.len() || bytes[end - 1] != b'\n' {
            return Err(CrateArchiveWitnessError::InvalidArchive);
        }
        let record = &bytes[offset + space + 1..end - 1];
        let Some(equals) = record.iter().position(|byte| *byte == b'=') else {
            return Err(CrateArchiveWitnessError::InvalidArchive);
        };
        if &record[..equals] != b"path" || saw_path {
            return Err(CrateArchiveWitnessError::InvalidArchive);
        }
        let value = &record[equals + 1..];
        if value.len() > MAX_MEMBER_PATH_BYTES || std::str::from_utf8(value).is_err() {
            return Err(CrateArchiveWitnessError::InvalidArchive);
        }
        saw_path = true;
        offset = end;
    }
    if !saw_path {
        return Err(CrateArchiveWitnessError::InvalidArchive);
    }
    Ok(())
}

fn map_has_descendant(paths: &BTreeMap<String, [u8; 32]>, path: &str) -> bool {
    let prefix = format!("{path}/");
    paths
        .range(prefix.clone()..)
        .next()
        .is_some_and(|(candidate, _)| candidate.starts_with(&prefix))
}

fn set_has_descendant(paths: &BTreeSet<String>, path: &str) -> bool {
    let prefix = format!("{path}/");
    paths
        .range(prefix.clone()..)
        .next()
        .is_some_and(|candidate| candidate.starts_with(&prefix))
}

fn checked_working_add(
    current: usize,
    additional: usize,
    limit: usize,
) -> Result<usize, CrateArchiveWitnessError> {
    let next = current
        .checked_add(additional)
        .ok_or(CrateArchiveWitnessError::Bounds)?;
    if next > limit {
        return Err(CrateArchiveWitnessError::Bounds);
    }
    Ok(next)
}

fn witness_hash(
    root: &str,
    archive_sha256: [u8; 32],
    members: &BTreeMap<String, [u8; 32]>,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(WITNESS_HASH_DOMAIN);
    hash_len_prefixed(&mut hasher, root.as_bytes());
    hasher.update(&archive_sha256);
    hasher.update(&(members.len() as u64).to_be_bytes());
    for (path, digest) in members {
        hash_len_prefixed(&mut hasher, path.as_bytes());
        hasher.update(digest);
    }
    *hasher.finalize().as_bytes()
}

fn hash_len_prefixed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn sha256_checked(
    bytes: &[u8],
    checkpoint: &mut impl FnMut() -> Result<(), String>,
) -> Result<[u8; 32], CrateArchiveWitnessError> {
    const CHUNK_BYTES: usize = 64 * 1024;
    let mut hasher = Sha256::new();
    for chunk in bytes.chunks(CHUNK_BYTES) {
        checkpoint().map_err(CrateArchiveWitnessError::Interrupted)?;
        hasher.update(chunk);
    }
    Ok(hasher.finalize().into())
}

fn map_parent_error(error: super::RegistryAddError) -> CrateArchiveWitnessError {
    match error {
        super::RegistryAddError::Acquisition(
            backend_engine::registry::AcquisitionError::Bounds,
        ) => CrateArchiveWitnessError::Bounds,
        _ => CrateArchiveWitnessError::InvalidArchive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;

    fn tar_header(path: &str, kind: u8, payload: &[u8]) -> [u8; TAR_BLOCK_BYTES] {
        let mut header = [0_u8; TAR_BLOCK_BYTES];
        header[..path.len()].copy_from_slice(path.as_bytes());
        header[100..108].copy_from_slice(b"0000644\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        let size = format!("{:011o}\0", payload.len());
        header[124..136].copy_from_slice(size.as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[148..156].fill(b' ');
        header[156] = kind;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        update_tar_checksum(&mut header);
        header
    }

    fn update_tar_checksum(header: &mut [u8; TAR_BLOCK_BYTES]) {
        header[148..156].fill(b' ');
        let checksum = header.iter().map(|byte| usize::from(*byte)).sum::<usize>();
        let checksum = format!("{checksum:06o}\0 ");
        header[148..156].copy_from_slice(checksum.as_bytes());
    }

    fn first_tar_header(bytes: &mut [u8]) -> &mut [u8; TAR_BLOCK_BYTES] {
        (&mut bytes[..TAR_BLOCK_BYTES])
            .try_into()
            .expect("tar header block")
    }

    fn tar(entries: &[(&str, u8, &[u8])]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for (path, kind, payload) in entries {
            bytes.extend_from_slice(&tar_header(path, *kind, payload));
            bytes.extend_from_slice(payload);
            let padded = payload.len().div_ceil(TAR_BLOCK_BYTES) * TAR_BLOCK_BYTES;
            bytes.resize(bytes.len() + padded.saturating_sub(payload.len()), 0);
        }
        bytes.resize(bytes.len() + TAR_BLOCK_BYTES * 2, 0);
        bytes
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).expect("write tar");
        encoder.finish().expect("finish gzip")
    }

    fn pax_record(key: &str, value: &str) -> Vec<u8> {
        let body = format!("{key}={value}\n");
        let mut length = body.len() + 2;
        loop {
            let next = body.len() + length.to_string().len() + 1;
            if next == length {
                return format!("{length} {body}").into_bytes();
            }
            length = next;
        }
    }

    fn capture(
        archive: &[u8],
        expected: [u8; 32],
    ) -> Result<CrateArchiveWitness, CrateArchiveWitnessError> {
        let mut checkpoint = || Ok(());
        CrateArchiveWitness::capture("equivalent", "1.0.2", archive, expected, &mut checkpoint)
    }

    #[test]
    fn witness_binds_exact_archive_and_relative_file_bytes() {
        let archive = gzip(&tar(&[
            ("equivalent-1.0.2/src/lib.rs", b'0', b"pub fn same() {}\n"),
            ("equivalent-1.0.2/README.md", b'0', b"readme\n"),
        ]));
        let expected = sha256(&archive);
        let witness = capture(&archive, expected).expect("valid exact crate archive");
        assert_eq!(witness.archive_sha256(), &expected);
        assert!(witness.matches("src/lib.rs", b"pub fn same() {}\n"));
        assert!(witness.matches("README.md", b"readme\n"));
        let mut cancelled = || Err("budget ended".to_owned());
        assert_eq!(
            witness
                .matches_checked("README.md", b"readme\n", &mut cancelled)
                .unwrap_err(),
            CrateArchiveWitnessError::Interrupted("budget ended".to_owned())
        );
        assert!(!witness.matches("src/lib.rs", b"pub fn changed() {}\n"));
        assert!(!witness.matches("../src/lib.rs", b"pub fn same() {}\n"));
        assert_eq!(witness.members().len(), 2);
        assert!(witness.retained_bytes() > 0);
        assert_eq!(
            capture(&archive, expected)
                .expect("repeat witness")
                .witness_hash(),
            witness.witness_hash(),
            "same authenticated bytes yield one deterministic witness"
        );
    }

    #[test]
    fn witness_validates_committed_equivalent_archive_and_known_member_digest() {
        const ARCHIVE_SHA256: [u8; 32] = [
            0x87, 0x7a, 0x4a, 0xce, 0x87, 0x13, 0xb0, 0xbc, 0xf2, 0xa4, 0xe7, 0xee, 0xc8, 0x25,
            0x29, 0xc0, 0x29, 0xf1, 0xd0, 0x61, 0x98, 0x86, 0xd1, 0x81, 0x45, 0xfe, 0xa9, 0x6c,
            0x3f, 0xfe, 0x5c, 0x0f,
        ];
        const SRC_LIB_SHA256: [u8; 32] = [
            0x1d, 0xd8, 0x43, 0x63, 0xf5, 0x61, 0xb3, 0x0b, 0x1d, 0xa7, 0x13, 0x48, 0x6c, 0x6b,
            0x58, 0x39, 0x00, 0x35, 0x3e, 0x62, 0xc5, 0x69, 0xd7, 0xba, 0x1d, 0xd8, 0x4e, 0xb2,
            0xc0, 0x4f, 0x1a, 0x14,
        ];
        let archive = include_bytes!("fixtures/equivalent-1.0.2.crate");
        let mut checkpoint = || Ok(());
        let witness = CrateArchiveWitness::capture(
            "equivalent",
            "1.0.2",
            archive,
            ARCHIVE_SHA256,
            &mut checkpoint,
        )
        .expect("committed real Cargo archive matches the locked checksum");
        assert_eq!(witness.archive_sha256(), &ARCHIVE_SHA256);
        assert_eq!(witness.member_digest("src/lib.rs"), Some(&SRC_LIB_SHA256));
    }

    #[test]
    fn witness_decodes_gnu_metadata_and_rejects_lossy_or_unknown_headers() {
        let path = "equivalent-1.0.2/src/lib.rs";
        let mut gnu = tar(&[(path, b'0', b"source")]);
        let header = first_tar_header(&mut gnu);
        header[257..263].copy_from_slice(b"ustar ");
        header[263..265].copy_from_slice(b" \0");
        header[345..500].fill(0xff);
        update_tar_checksum(header);
        let archive = gzip(&gnu);
        let witness = capture(&archive, sha256(&archive)).expect("GNU metadata is not a path");
        assert!(witness.matches("src/lib.rs", b"source"));

        let mut non_utf8 = tar(&[(path, b'0', b"source")]);
        non_utf8[0] = 0xff;
        let header = first_tar_header(&mut non_utf8);
        update_tar_checksum(header);
        let archive = gzip(&non_utf8);
        assert_eq!(
            capture(&archive, sha256(&archive)).unwrap_err(),
            CrateArchiveWitnessError::InvalidArchive
        );

        let mut non_ascii_size = tar(&[(path, b'0', b"")]);
        let header = first_tar_header(&mut non_ascii_size);
        header[124] = 0xff;
        update_tar_checksum(header);
        let archive = gzip(&non_ascii_size);
        assert_eq!(
            capture(&archive, sha256(&archive)).unwrap_err(),
            CrateArchiveWitnessError::InvalidArchive,
            "invalid size bytes cannot collapse to tar_octal's empty-field zero"
        );

        let mut unknown = tar(&[(path, b'0', b"source")]);
        let header = first_tar_header(&mut unknown);
        header[257..263].copy_from_slice(b"bogus!");
        update_tar_checksum(header);
        let archive = gzip(&unknown);
        assert_eq!(
            capture(&archive, sha256(&archive)).unwrap_err(),
            CrateArchiveWitnessError::InvalidArchive
        );
    }

    #[test]
    fn witness_rejects_digest_mismatch_wrong_root_paths_links_duplicates_and_trailing_payload() {
        let good = gzip(&tar(&[("equivalent-1.0.2/src/lib.rs", b'0', b"ok")]));
        assert_eq!(
            capture(&good, [0; 32]).unwrap_err(),
            CrateArchiveWitnessError::ArchiveDigestMismatch
        );
        for (path, kind) in [
            ("other-1.0.2/src/lib.rs", b'0'),
            ("equivalent-1.0.2/../escape.rs", b'0'),
            ("/equivalent-1.0.2/src/lib.rs", b'0'),
            ("equivalent-1.0.2/src/lib.rs", b'2'),
        ] {
            let archive = gzip(&tar(&[(path, kind, b"payload")]));
            assert_eq!(
                capture(&archive, sha256(&archive)).unwrap_err(),
                CrateArchiveWitnessError::InvalidArchive,
                "rejected {path} member kind {}",
                char::from(kind)
            );
        }
        let duplicate = gzip(&tar(&[
            ("equivalent-1.0.2/src/lib.rs", b'0', b"first"),
            ("equivalent-1.0.2/src/lib.rs", b'0', b"second"),
        ]));
        assert_eq!(
            capture(&duplicate, sha256(&duplicate)).unwrap_err(),
            CrateArchiveWitnessError::InvalidArchive
        );

        let mut trailing = good.clone();
        trailing.extend_from_slice(b"nonzero trailing bytes");
        assert_eq!(
            capture(&trailing, sha256(&trailing)).unwrap_err(),
            CrateArchiveWitnessError::InvalidArchive
        );
    }

    #[test]
    fn witness_rejects_truncated_tar_and_propagates_cancellation() {
        let tar = tar(&[("equivalent-1.0.2/src/lib.rs", b'0', b"ok")]);
        let truncated = gzip(&tar[..tar.len() - TAR_BLOCK_BYTES]);
        assert_eq!(
            capture(&truncated, sha256(&truncated)).unwrap_err(),
            CrateArchiveWitnessError::InvalidArchive
        );

        let archive = gzip(&tar);
        let expected = sha256(&archive);
        let mut calls = 0;
        let mut checkpoint = || {
            calls += 1;
            if calls > 1 {
                Err("owner cancelled".to_owned())
            } else {
                Ok(())
            }
        };
        assert_eq!(
            CrateArchiveWitness::capture(
                "equivalent",
                "1.0.2",
                &archive,
                expected,
                &mut checkpoint,
            )
            .unwrap_err(),
            CrateArchiveWitnessError::Interrupted("owner cancelled".to_owned())
        );
    }

    #[test]
    fn witness_accepts_only_path_pax_extensions() {
        let path = "equivalent-1.0.2/src/lib.rs";
        let pax = pax_record("path", path);
        let valid = gzip(&tar(&[
            ("PaxHeaders/entry", b'x', &pax),
            ("placeholder", b'0', b"source"),
        ]));
        let witness = capture(&valid, sha256(&valid)).expect("path-only PAX is supported");
        assert!(witness.matches("src/lib.rs", b"source"));

        let size = pax_record("size", "6");
        let ambiguous = gzip(&tar(&[
            ("PaxHeaders/entry", b'x', &size),
            (path, b'0', b"source"),
        ]));
        assert_eq!(
            capture(&ambiguous, sha256(&ambiguous)).unwrap_err(),
            CrateArchiveWitnessError::InvalidArchive,
            "PAX size could change which bytes Cargo extracts"
        );
    }

    #[test]
    fn witness_rejects_file_path_prefix_collisions_in_either_order() {
        for entries in [
            vec![
                ("equivalent-1.0.2/src", b'0', b"file".as_slice()),
                ("equivalent-1.0.2/src/lib.rs", b'0', b"child".as_slice()),
            ],
            vec![
                (
                    "equivalent-1.0.2/src/lib.rs/child",
                    b'0',
                    b"grandchild".as_slice(),
                ),
                ("equivalent-1.0.2/src/lib.rs", b'0', b"parent".as_slice()),
            ],
            vec![
                ("equivalent-1.0.2/src/lib.rs/child", b'5', b"".as_slice()),
                ("equivalent-1.0.2/src/lib.rs", b'0', b"parent".as_slice()),
            ],
        ] {
            let archive = gzip(&tar(&entries));
            assert_eq!(
                capture(&archive, sha256(&archive)).unwrap_err(),
                CrateArchiveWitnessError::InvalidArchive
            );
        }
    }

    #[test]
    fn parser_enforces_working_budget_before_member_map_growth() {
        let names: Vec<String> = (0..64)
            .map(|index| format!("equivalent-1.0.2/src/file{index:03}"))
            .collect();
        let borrowed: Vec<(&str, u8, &[u8])> = names
            .iter()
            .map(|name| (name.as_str(), b'0', b"".as_slice()))
            .collect();
        let tar_bytes = tar(&borrowed);
        let first_relative = relative_member_path("equivalent-1.0.2", &names[0], false)
            .expect("relative first path");
        let working_base = tar_bytes
            .capacity()
            .checked_add(WITNESS_BASE_ACCOUNTING_BYTES)
            .and_then(|bytes| bytes.checked_add(PARSER_SCRATCH_ACCOUNTING_BYTES))
            .expect("working base");
        let one_member = first_relative.capacity() + 32 + MEMBER_MAP_NODE_ACCOUNTING_BYTES;
        let limit = working_base + one_member;
        let mut checkpoint = || Ok(());
        assert_eq!(
            parse_tar(
                "equivalent-1.0.2",
                &tar_bytes,
                tar_bytes.capacity(),
                0,
                limit,
                &mut checkpoint,
            )
            .unwrap_err(),
            CrateArchiveWitnessError::Bounds,
            "the second indexed member must be rejected before insertion"
        );
    }
}
