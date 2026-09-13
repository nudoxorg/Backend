use crate::{
    ArchiveChecksum, MAX_ARCHIVE_BYTES, MAX_PAGE_BYTES, RegistryError, RegistryTransport,
    TransportFault, TransportRequest, VerifiedArchive,
};
use serde::Deserialize;
use server_index_catalog::{
    FeedCheckpoint, FeedCursor, FeedIdentity, FeedSnapshotId, FeedValidator,
};
use server_index_vocabulary::PackageVersion;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};

/// Maximum source entries committed in one stable sparse-index snapshot chunk.
pub const SPARSE_CHUNK_ROWS: usize = server_index_ingest::MAX_RECONCILIATION_ROWS;

/// A bounded page fetched from a real registry listing.
#[derive(Clone, Debug)]
pub(crate) struct RegistryPage {
    pub(crate) next_cursor: FeedCursor,
    pub(crate) validator: Option<FeedValidator>,
    pub(crate) snapshot: FeedSnapshotId,
    pub(crate) next_offset: u64,
    pub(crate) complete: bool,
    pub(crate) entries: Vec<SparseEntry>,
}

#[derive(Clone, Debug)]
pub(crate) struct SparseEntry {
    pub(crate) package: String,
    pub(crate) version: String,
    pub(crate) checksum: ArchiveChecksum,
    pub(crate) active: bool,
}

/// Result of a conditional feed poll.
#[derive(Clone, Debug)]
pub(crate) enum PollResult {
    /// The source returned 304 and no source/cursor state changed.
    NotModified,
    /// A completely verified bounded page is ready for materialization.
    Page(RegistryPage),
}

/// Real crates.io sparse-index adapter for one explicitly named Cargo package.
///
/// The sparse index is authoritative registry metadata, unlike the mutable search API. One poll
/// reads the same package-index resource every time, so the persisted ETag is scoped to exactly
/// that request. The complete, bounded version set is a reconciliation snapshot, not a claim of
/// a global crates.io change cursor.
#[derive(Debug)]
pub struct CratesIoAdapter<T> {
    transport: T,
    index_url: String,
    download_url: String,
    package: String,
    feed: FeedIdentity,
}

impl<T> CratesIoAdapter<T> {
    /// Targets one named package in the public sparse index and crates.io archive service.
    pub fn new(transport: T, package: &str) -> Result<Self, RegistryError> {
        Self::with_bases(
            transport,
            "https://index.crates.io",
            "https://static.crates.io",
            package,
        )
    }

    /// Builds a named sparse follower with caller-selected origins for deterministic local tests.
    pub fn with_bases(
        transport: T,
        index_url: &str,
        download_url: &str,
        package: &str,
    ) -> Result<Self, RegistryError> {
        let index_url = index_url.trim_end_matches('/');
        let download_url = download_url.trim_end_matches('/');
        if (!index_url.starts_with("https://") && !index_url.starts_with("http://"))
            || (!download_url.starts_with("https://") && !download_url.starts_with("http://"))
            || !valid_crate_name(package)
        {
            return Err(RegistryError::Protocol);
        }
        Ok(Self {
            transport,
            index_url: index_url.to_owned(),
            download_url: download_url.to_owned(),
            package: package.to_owned(),
            feed: FeedIdentity::new(format!("crates.io/sparse/{package}"))
                .map_err(|_| RegistryError::Protocol)?,
        })
    }

    /// Returns the independently durable feed identity.
    pub fn feed(&self) -> &FeedIdentity {
        &self.feed
    }

    /// Returns the named Cargo package covered by this bounded snapshot follower.
    pub fn package(&self) -> &str {
        &self.package
    }
}

impl<T: RegistryTransport> CratesIoAdapter<T> {
    /// Admits one deterministic bounded metadata chunk; archives stay outside this phase so
    /// unchanged checksums can avoid a second network request.
    pub(crate) fn poll(
        &mut self,
        checkpoint: &FeedCheckpoint,
        cancelled: &AtomicBool,
    ) -> Result<PollResult, RegistryError> {
        if checkpoint.feed != self.feed {
            return Err(RegistryError::Cursor);
        }
        if checkpoint
            .cursor
            .as_ref()
            .is_some_and(|cursor| cursor.as_str() != self.package)
        {
            return Err(RegistryError::Cursor);
        }
        let listing_url = format!("{}/{}", self.index_url, sparse_path(&self.package));
        let conditional = checkpoint.snapshot.is_none() && checkpoint.validator.is_some();
        let mut body = Vec::new();
        let response = self.transport.get(
            TransportRequest {
                url: &listing_url,
                if_none_match: conditional
                    .then_some(())
                    .and_then(|()| checkpoint.validator.as_ref().map(FeedValidator::as_str)),
                maximum_bytes: MAX_PAGE_BYTES,
            },
            &mut body,
            cancelled,
        )?;
        if response.status == 304 {
            return if conditional {
                Ok(PollResult::NotModified)
            } else {
                Err(RegistryError::Status { status: 304 })
            };
        }
        if response.status != 200 {
            return Err(RegistryError::Status {
                status: response.status,
            });
        }
        let listing = sparse_entries(&body, &self.package)?;
        let snapshot = FeedSnapshotId::from_sha256(Sha256::digest(&body).into());
        let start = if checkpoint.snapshot.as_ref() == Some(&snapshot) {
            usize::try_from(checkpoint.offset).map_err(|_| RegistryError::Cursor)?
        } else {
            0
        };
        if start > listing.len() {
            return Err(RegistryError::Cursor);
        }
        let end = start.saturating_add(SPARSE_CHUNK_ROWS).min(listing.len());
        let complete = end == listing.len();
        let mut entries = Vec::with_capacity(end - start);
        for row in listing.into_iter().skip(start).take(end - start) {
            if cancelled.load(Ordering::Acquire) {
                return Err(RegistryError::Transport(TransportFault::Cancelled));
            }
            entries.push(row);
        }
        Ok(PollResult::Page(RegistryPage {
            next_cursor: FeedCursor::new(self.package.clone())
                .map_err(|_| RegistryError::Cursor)?,
            validator: response
                .etag
                .map(FeedValidator::new)
                .transpose()
                .map_err(|_| RegistryError::Protocol)?,
            snapshot,
            next_offset: u64::try_from(end).map_err(|_| RegistryError::Cursor)?,
            complete,
            entries,
        }))
    }

    pub(crate) fn fetch_archive<'entry>(
        &mut self,
        entry: &'entry SparseEntry,
        cancelled: &AtomicBool,
    ) -> Result<VerifiedArchive<'entry>, RegistryError> {
        let archive_url = format!(
            "{}/crates/{}/{}-{}.crate",
            self.download_url,
            component(&entry.package),
            component(&entry.package),
            component(&entry.version)
        );
        let mut bytes = Vec::new();
        let response = self.transport.get(
            TransportRequest {
                url: &archive_url,
                if_none_match: None,
                maximum_bytes: MAX_ARCHIVE_BYTES,
            },
            &mut bytes,
            cancelled,
        )?;
        if response.status != 200 {
            return Err(RegistryError::Status {
                status: response.status,
            });
        }
        let actual = ArchiveChecksum::from_bytes(Sha256::digest(&bytes).into());
        if actual != entry.checksum {
            return Err(RegistryError::ChecksumMismatch {
                expected: entry.checksum,
                actual,
            });
        }
        Ok(VerifiedArchive {
            package: &entry.package,
            version: &entry.version,
            checksum: entry.checksum,
            bytes,
        })
    }
}

fn component(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            output.push(char::from(byte));
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

fn valid_crate_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}
fn sparse_path(name: &str) -> String {
    match name.len() {
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{}", &name[..1], name),
        _ => format!("{}/{}/{}", &name[..2], &name[2..4], name),
    }
}
fn sparse_entries(body: &[u8], expected: &str) -> Result<Vec<SparseEntry>, RegistryError> {
    let text = core::str::from_utf8(body).map_err(|_| RegistryError::Protocol)?;
    let mut entries = text
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let row: SparseRow = serde_json::from_str(line).map_err(|_| RegistryError::Protocol)?;
            if row.name != expected {
                return Err(RegistryError::Protocol);
            }
            PackageVersion::new(&row.vers).map_err(|_| RegistryError::Coordinate)?;
            Ok(SparseEntry {
                package: row.name,
                version: row.vers,
                checksum: ArchiveChecksum::parse_hex(&row.cksum)?,
                active: !row.yanked,
            })
        })
        .collect::<Result<Vec<_>, RegistryError>>()?;
    entries.sort_by(|left, right| left.version.cmp(&right.version));
    if entries
        .windows(2)
        .any(|pair| pair[0].version == pair[1].version)
    {
        return Err(RegistryError::Protocol);
    }
    Ok(entries)
}
#[cfg(test)]
pub(crate) fn hex_digest(checksum: ArchiveChecksum) -> String {
    checksum
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
#[derive(Deserialize)]
struct SparseRow {
    name: String,
    vers: String,
    cksum: String,
    #[serde(default)]
    yanked: bool,
}
