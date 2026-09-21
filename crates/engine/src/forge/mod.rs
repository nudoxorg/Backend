//! First-class acquisition of source trees from code forges.
//!
//! A forge source is a repository coordinate plus an explicit typed revision.
//! The coordinate is credential-free and immutable identities are derived only
//! after the remote owner has resolved the requested tag/branch/commit to an
//! exact commit and tree.  Source bytes are admitted by the shared
//! [`acquisition::ContentAddressedStore`], and the resulting file rows use the
//! same [`acquisition::TreeManifest`] used by registry and local sources.
//!
//! The module deliberately keeps the network seam small.  [`ForgeTransport`]
//! is suitable for loopback fixtures and remote delegation, while
//! [`HttpForgeTransport`] provides bounded HTTPS metadata/archive requests for
//! the public GitHub, GitLab, and Codeberg APIs.  A generic HTTPS Git source is
//! acquired through a supplied transport or the safe process adapter; command
//! arguments are passed directly to `git` and are never interpreted by a
//! shell.

use crate::acquisition::{
    AcquisitionDelta, ArchiveBudget, ArchiveManifestBuilder, ContentAddressedStore,
    ContentStoreError, DeltaChange, ManifestEntry, RawArchiveObjectId, SourceSnapshot,
    TreeManifest,
};
use crate::journal::{HashChainJournal, JournalCodec, JournalDomain, JournalError, JournalLimits};
use backend_library::{
    DependencyAuthority, DependencyEvidence, DependencyFacts, DependencyScope,
    PackageDependencyRecord, PackageDependencyTarget, PackageReference, ProductText,
    RegistryEcosystem,
};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fmt, fs,
    io::{self, Cursor, Read, Seek, SeekFrom},
    path::PathBuf,
    process::{Child, ChildStdout, Command, Stdio},
    str::FromStr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const ID_BYTES: usize = 32;
const MAX_COORDINATE_BYTES: usize = 2_048;
const MAX_REF_BYTES: usize = 512;
const MAX_OWNER_BYTES: usize = 512;
const MAX_REPOSITORY_BYTES: usize = 256;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 2 * 1024 * 1024;
const MAX_README_BYTES: usize = 256 * 1024;
const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;
const GIT_HASH_BYTES: usize = 20;

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

fn digest_fields(domain: &[u8], fields: &[&[u8]]) -> [u8; ID_BYTES] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    for field in fields {
        hasher.update(&(field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    *hasher.finalize().as_bytes()
}

fn resolution_cursor(resolution: &ForgeResolution) -> [u8; ID_BYTES] {
    let commit = resolution.commit.as_hex();
    let tree = resolution.tree.as_ref().map(ForgeObjectId::as_hex);
    digest_fields(
        b"nudox.forge.cursor.v1\0",
        &[
            commit.as_bytes(),
            tree.as_deref().unwrap_or_default().as_bytes(),
            resolution
                .authority
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
            resolution
                .validator
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        ],
    )
}

fn metadata_frontier(
    metadata: &ForgeRepositoryMetadata,
    manifests: &[ForgePackageManifest],
) -> [u8; ID_BYTES] {
    let metadata = serde_json::to_vec(metadata).unwrap_or_default();
    let manifests = serde_json::to_vec(manifests).unwrap_or_default();
    digest_fields(b"nudox.forge.facts.v1\0", &[&metadata, &manifests])
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        result.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    result
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() % 2 != 0 {
        return None;
    }
    let bytes = value.as_bytes();
    let mut result = Vec::with_capacity(bytes.len() / 2);
    let nibble = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    };
    let mut index = 0;
    while index < bytes.len() {
        result.push((nibble(bytes[index])? << 4) | nibble(bytes[index + 1])?);
        index += 2;
    }
    Some(result)
}

fn text(value: &str, maximum: usize) -> Result<Arc<str>, ForgeCoordinateError> {
    if value.is_empty()
        || value.len() > maximum
        || value.trim() != value
        || value
            .bytes()
            .any(|byte| byte == 0 || byte == b'\n' || byte == b'\r')
    {
        return Err(ForgeCoordinateError::NonCanonicalText);
    }
    Ok(Arc::from(value))
}

fn path_part(value: &str, maximum: usize) -> Result<Arc<str>, ForgeCoordinateError> {
    let value = text(value, maximum)?;
    if value
        .split('/')
        .any(|part| part.is_empty() || part == "." || part == ".." || part.contains('\\'))
    {
        return Err(ForgeCoordinateError::UnsafePath);
    }
    Ok(value)
}

mod archive;
mod facts;
mod identity;
mod manifest;
mod model;
mod policy;
mod protocol;
mod service;
#[cfg(test)]
mod tests;
mod transport;

pub use facts::{ForgeFact, ForgeRepositoryMetadata, ForgeResolution, ForgeUnavailableReason};
pub use identity::{
    ForgeCoordinate, ForgeCoordinateError, ForgeHashAlgorithm, ForgeObjectId, ForgeProvider,
    ForgeRefName, ForgeRevision,
};
pub use model::{
    ForgeAcquisitionOutcome, ForgeAcquisitionResult, ForgePackageManifest, ForgeReceipt,
    ForgeRejectReason,
};
pub use policy::{ForgeAcquisitionLimits, ForgeAcquisitionPolicy};
pub use protocol::{
    ForgeDelegatedObject, ForgeDelegationRequest, ForgeProtocolError, verify_delegated_object,
};
pub use service::{ForgeAcquisitionError, ForgeAcquisitionService};
pub use transport::{
    ForgeArchive, ForgeArchiveFormat, ForgeAuthToken, ForgeTransport, ForgeTransportError,
    GitCommandTransport, HttpForgeTransport,
};
