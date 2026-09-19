//! Product composition for the durable registry acquisition effect.
//!
//! This module deliberately contains no registry state machine of its own.
//! [`RegistryOwner`] remains the only owner of feed cursors, archive
//! verification, immutable objects, and recovery; this is the small adapter
//! that lets a product `Add` request select that existing effect.

use crate::process::RegistryConfig;
use backend_engine::registry::{
    AcquisitionError, AcquisitionOutcome, EcosystemAdapter, HttpRegistryTransport,
    PackageCoordinate, RegistryOwner, admit_registry_coordinate,
};
use flate2::read::{DeflateDecoder, GzDecoder};
use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_TARGET_POLL_PAGES: usize = 256;

/// Durable registry state attached to one local owner loop.
pub(super) struct RegistryGateway {
    owner: RegistryOwner,
    config: RegistryConfig,
    workspace_root: PathBuf,
}

/// Typed terminal state returned while satisfying a remote package add.
#[derive(Debug)]
pub(super) enum RegistryAddError {
    /// No endpoint was configured for a remote coordinate.
    NotConfigured,
    /// The endpoint policy explicitly forbids network effects.
    Offline,
    /// The endpoint could not be reached within its configured deadline.
    Unavailable,
    /// The endpoint asked the caller to retry later.
    RetryAfter(Duration),
    /// The feed completed without publishing the requested coordinate.
    NotFound,
    /// The verified bytes are not a supported bounded source archive.
    UnsupportedArchive,
    /// A typed registry acquisition or persistence failure.
    Acquisition(AcquisitionError),
}

impl fmt::Display for RegistryAddError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotConfigured => formatter.write_str("registry is not configured"),
            Self::Offline => formatter.write_str("registry acquisition is offline"),
            Self::Unavailable => formatter.write_str("registry is unavailable"),
            Self::RetryAfter(delay) => {
                write!(formatter, "registry requested retry after {delay:?}")
            }
            Self::NotFound => formatter.write_str("registry package was not found"),
            Self::UnsupportedArchive => {
                formatter.write_str("registry archive is unsupported or contains no source files")
            }
            Self::Acquisition(error) => error.fmt(formatter),
        }
    }
}

impl RegistryGateway {
    /// Projects the complete recovered local catalog without network I/O.
    pub(super) fn catalog(&self) -> Result<Vec<backend_engine::RegistryPackageRecord>, String> {
        self.owner
            .published_packages()
            .map(|published| {
                let admitted = admit_registry_coordinate(&published.coordinate)
                    .map_err(|_| backend_engine::ProductAdmissionError::PackageReference)?;
                Ok(backend_engine::RegistryPackageRecord {
                    ecosystem: admitted.ecosystem(),
                    coordinate: backend_engine::PackageReference::Purl(
                        published.coordinate.clone(),
                    ),
                    name: backend_engine::ProductText::new(admitted.qualified_name().as_str())?,
                    version: backend_engine::ProductText::new(admitted.version().as_str())?,
                    bytes: published.bytes,
                })
            })
            .collect::<Result<Vec<_>, backend_engine::ProductAdmissionError>>()
            .map_err(|error| error.to_string())
    }

    /// Opens the existing durable owner when a registry endpoint is present.
    pub(super) fn open(
        config: &RegistryConfig,
        root: impl AsRef<Path>,
    ) -> Result<Option<Self>, AcquisitionError> {
        let Some(endpoint) = config.endpoint.clone() else {
            return Ok(None);
        };
        let workspace_root = root.as_ref().to_path_buf();
        let (owner, _) =
            RegistryOwner::open(&workspace_root, endpoint, config.policy, config.limits)?;
        Ok(Some(Self {
            owner,
            config: config.clone(),
            workspace_root,
        }))
    }

    /// Fetches, verifies, and durably publishes one exact remote coordinate.
    pub(super) fn acquire(
        &mut self,
        coordinate: &PackageCoordinate,
    ) -> Result<Vec<u8>, RegistryAddError> {
        if self.config.endpoint.is_none() {
            return Err(RegistryAddError::NotConfigured);
        }
        if self.owner.published(coordinate).is_some() {
            return self.ensure_artifact(coordinate);
        }
        let mut transport = self.transport(coordinate)?;
        for _ in 0..MAX_TARGET_POLL_PAGES {
            match self
                .owner
                .poll(&mut transport)
                .map_err(RegistryAddError::Acquisition)?
            {
                AcquisitionOutcome::Offline { .. } => return Err(RegistryAddError::Offline),
                AcquisitionOutcome::Unavailable { .. } => {
                    return Err(RegistryAddError::Unavailable);
                }
                AcquisitionOutcome::RetryAfter { delay, .. } => {
                    return Err(RegistryAddError::RetryAfter(delay));
                }
                AcquisitionOutcome::UpToDate { .. } => return Err(RegistryAddError::NotFound),
                AcquisitionOutcome::Published(receipt) => {
                    if receipt
                        .packages
                        .iter()
                        .any(|package| package.coordinate == *coordinate)
                        || self.owner.published(coordinate).is_some()
                    {
                        return self.ensure_artifact(coordinate);
                    }
                }
            }
        }
        Err(RegistryAddError::Acquisition(AcquisitionError::Bounds))
    }

    pub(super) fn stage_archive(
        &self,
        coordinate: &PackageCoordinate,
        archive: &[u8],
    ) -> Result<StagedProject, RegistryAddError> {
        stage_archive(coordinate, archive, &self.workspace_root)
    }

    fn ensure_artifact(&self, coordinate: &PackageCoordinate) -> Result<Vec<u8>, RegistryAddError> {
        self.owner
            .read_artifact(coordinate)
            .map_err(RegistryAddError::Acquisition)?
            .map(|object| object.bytes().to_vec())
            .ok_or(RegistryAddError::NotFound)
    }

    fn transport(
        &self,
        coordinate: &PackageCoordinate,
    ) -> Result<HttpRegistryTransport, RegistryAddError> {
        let Some(endpoint) = self.config.endpoint.clone() else {
            return Err(RegistryAddError::NotConfigured);
        };
        let admitted =
            admit_registry_coordinate(coordinate).map_err(RegistryAddError::Acquisition)?;
        if endpoint.ecosystem() != admitted.ecosystem() {
            return Err(RegistryAddError::Acquisition(
                AcquisitionError::InvalidConfiguration,
            ));
        }
        if self.config.native {
            let adapter = native_adapter(endpoint, coordinate)?;
            HttpRegistryTransport::for_native(
                adapter,
                self.config.authentication.clone(),
                self.config.limits,
            )
            .map_err(RegistryAddError::Acquisition)
        } else {
            HttpRegistryTransport::new(
                endpoint,
                self.config.authentication.clone(),
                self.config.limits,
            )
            .map_err(RegistryAddError::Acquisition)
        }
    }
}

fn native_adapter(
    endpoint: backend_engine::registry::RegistryEndpoint,
    coordinate: &PackageCoordinate,
) -> Result<EcosystemAdapter, RegistryAddError> {
    let admitted = admit_registry_coordinate(coordinate).map_err(RegistryAddError::Acquisition)?;
    EcosystemAdapter::new(
        endpoint,
        admitted.name().clone(),
        admitted.namespace().cloned(),
    )
    .map_err(RegistryAddError::Acquisition)
}

/// A temporary, sanitized source tree used by the normal product ingester.
pub(super) struct StagedProject {
    path: PathBuf,
}

impl StagedProject {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagedProject {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

const MAX_EXTRACTED_FILES: usize = 100_000;
const MAX_EXTRACTED_FILE_BYTES: usize = 512 * 1024;
const MAX_EXTRACTED_TOTAL_BYTES: usize = 64 * 1024 * 1024;
const TAR_BLOCK_BYTES: usize = 512;

/// Materializes a verified archive beneath a private workspace staging root.
///
/// Only regular files with normalized relative paths are written. Tar/gzip and
/// zip/deflate are accepted because those are the formats emitted by the
/// supported ecosystem registries. Every other byte shape reaches the typed
/// unsupported terminal instead of being published as a label-only package.
pub(super) fn stage_archive(
    _coordinate: &PackageCoordinate,
    archive: &[u8],
    workspace_root: impl AsRef<Path>,
) -> Result<StagedProject, RegistryAddError> {
    if archive.is_empty() {
        return Err(RegistryAddError::UnsupportedArchive);
    }
    let staging_root = workspace_root.as_ref().join("registry-staging");
    fs::create_dir_all(&staging_root)
        .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
    let digest = blake3::hash(archive);
    let directory = staging_root.join(hex(digest.as_bytes()));
    if directory.exists() {
        fs::remove_dir_all(&directory)
            .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
    }
    fs::create_dir(&directory)
        .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
    let mut writer = StageWriter::new(directory.clone());
    let result = extract_archive(archive, &mut writer);
    if result.is_err() {
        let _ = fs::remove_dir_all(&directory);
    }
    result?;
    if writer.source_files == 0 {
        let _ = fs::remove_dir_all(&directory);
        return Err(RegistryAddError::UnsupportedArchive);
    }
    Ok(StagedProject { path: directory })
}

struct StageWriter {
    root: PathBuf,
    paths: BTreeSet<String>,
    files: usize,
    source_files: usize,
    bytes: usize,
}

impl StageWriter {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            paths: BTreeSet::new(),
            files: 0,
            source_files: 0,
            bytes: 0,
        }
    }

    fn directory(raw: &str) -> Result<(), RegistryAddError> {
        let _ = confined_path(raw)?;
        Ok(())
    }

    fn file(&mut self, raw: &str, bytes: &[u8]) -> Result<(), RegistryAddError> {
        let relative = confined_path(raw)?;
        if bytes.len() > MAX_EXTRACTED_FILE_BYTES {
            return Err(RegistryAddError::Acquisition(AcquisitionError::Bounds));
        }
        self.files = self
            .files
            .checked_add(1)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        if self.files > MAX_EXTRACTED_FILES {
            return Err(RegistryAddError::Acquisition(AcquisitionError::Bounds));
        }
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        if self.bytes > MAX_EXTRACTED_TOTAL_BYTES {
            return Err(RegistryAddError::Acquisition(AcquisitionError::Bounds));
        }
        let key = relative.to_string_lossy().into_owned();
        if !self.paths.insert(key.clone()) {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        let path = self.root.join(&relative);
        let parent = path.parent().ok_or(RegistryAddError::UnsupportedArchive)?;
        create_confined_directories(&self.root, parent)?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
        file.write_all(bytes)
            .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
        file.sync_all()
            .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
        if supported_source_name(&key) {
            self.source_files = self.source_files.saturating_add(1);
        }
        Ok(())
    }
}

fn extract_archive(archive: &[u8], writer: &mut StageWriter) -> Result<(), RegistryAddError> {
    if archive.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = GzDecoder::new(Cursor::new(archive));
        let decompressed = read_bounded(&mut decoder, MAX_EXTRACTED_TOTAL_BYTES)?;
        return extract_tar(&decompressed, writer);
    }
    if archive.starts_with(b"PK\x03\x04") || archive.starts_with(b"PK\x05\x06") {
        return extract_zip(archive, writer);
    }
    extract_tar(archive, writer)
}

fn extract_tar(bytes: &[u8], writer: &mut StageWriter) -> Result<(), RegistryAddError> {
    let mut offset = 0usize;
    let mut terminated = false;
    while offset
        .checked_add(TAR_BLOCK_BYTES)
        .is_some_and(|end| end <= bytes.len())
    {
        let header = &bytes[offset..offset + TAR_BLOCK_BYTES];
        if header.iter().all(|byte| *byte == 0) {
            terminated = true;
            break;
        }
        let name = tar_name(header)?;
        let size = tar_octal(&header[124..136])?;
        let data_start = offset
            .checked_add(TAR_BLOCK_BYTES)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        let data_end = data_start
            .checked_add(size)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        if data_end > bytes.len() {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        match header[156] {
            0 | b'0' => writer.file(&name, &bytes[data_start..data_end])?,
            b'5' => StageWriter::directory(&name)?,
            // Links and special nodes are never materialized.
            _ => return Err(RegistryAddError::UnsupportedArchive),
        }
        let padded = size
            .checked_add(TAR_BLOCK_BYTES - 1)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?
            / TAR_BLOCK_BYTES
            * TAR_BLOCK_BYTES;
        offset = data_start
            .checked_add(padded)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
    }
    if !terminated {
        return Err(RegistryAddError::UnsupportedArchive);
    }
    Ok(())
}

fn tar_name(header: &[u8]) -> Result<String, RegistryAddError> {
    let name = trim_nul(&header[..100]);
    let prefix = trim_nul(&header[345..500]);
    let name = if prefix.is_empty() {
        name.to_owned()
    } else if name.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}/{name}")
    };
    (!name.is_empty())
        .then_some(name)
        .ok_or(RegistryAddError::UnsupportedArchive)
}

fn tar_octal(field: &[u8]) -> Result<usize, RegistryAddError> {
    let value = trim_nul(field).trim();
    if value.is_empty() {
        return Ok(0);
    }
    usize::from_str_radix(value, 8).map_err(|_| RegistryAddError::UnsupportedArchive)
}

fn trim_nul(bytes: &[u8]) -> &str {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end]).unwrap_or("")
}

fn extract_zip(bytes: &[u8], writer: &mut StageWriter) -> Result<(), RegistryAddError> {
    let mut offset = 0usize;
    let mut entries = 0usize;
    while offset.checked_add(4).is_some_and(|end| end <= bytes.len()) {
        let signature = le_u32(bytes, offset)?;
        if signature == 0x0201_4b50 || signature == 0x0605_4b50 {
            break;
        }
        if signature != 0x0403_4b50 {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        if offset.checked_add(30).is_none_or(|end| end > bytes.len()) {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        let flags = le_u16(bytes, offset + 6)?;
        let method = le_u16(bytes, offset + 8)?;
        let compressed = usize::try_from(le_u32(bytes, offset + 18)?)
            .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        let declared = usize::try_from(le_u32(bytes, offset + 22)?)
            .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        let name_len = usize::from(le_u16(bytes, offset + 26)?);
        let extra_len = usize::from(le_u16(bytes, offset + 28)?);
        let name_start = offset + 30;
        let data_start = name_start
            .checked_add(name_len)
            .and_then(|value| value.checked_add(extra_len))
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        let data_end = data_start
            .checked_add(compressed)
            .ok_or(RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
        if data_end > bytes.len() || flags & 0x0001 != 0 || flags & 0x0008 != 0 {
            return Err(RegistryAddError::UnsupportedArchive);
        }
        let name = std::str::from_utf8(
            bytes
                .get(name_start..name_start.saturating_add(name_len))
                .ok_or(RegistryAddError::UnsupportedArchive)?,
        )
        .map_err(|_| RegistryAddError::UnsupportedArchive)?;
        if name.ends_with('/') {
            StageWriter::directory(name)?;
        } else {
            if declared > MAX_EXTRACTED_FILE_BYTES {
                return Err(RegistryAddError::Acquisition(AcquisitionError::Bounds));
            }
            let content = match method {
                0 => bytes[data_start..data_end].to_vec(),
                8 => {
                    let mut decoder =
                        DeflateDecoder::new(Cursor::new(&bytes[data_start..data_end]));
                    read_bounded(&mut decoder, MAX_EXTRACTED_FILE_BYTES)?
                }
                _ => return Err(RegistryAddError::UnsupportedArchive),
            };
            if content.len() != declared {
                return Err(RegistryAddError::UnsupportedArchive);
            }
            writer.file(name, &content)?;
            entries = entries.saturating_add(1);
        }
        offset = data_end;
    }
    (entries > 0)
        .then_some(())
        .ok_or(RegistryAddError::UnsupportedArchive)
}

fn read_bounded(reader: &mut impl Read, maximum: usize) -> Result<Vec<u8>, RegistryAddError> {
    let capacity = maximum.saturating_add(1);
    let mut bytes = Vec::new();
    bytes
        .try_reserve(capacity)
        .map_err(|_| RegistryAddError::Acquisition(AcquisitionError::Bounds))?;
    reader
        .take(u64::try_from(capacity).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|_| RegistryAddError::UnsupportedArchive)?;
    if bytes.len() > maximum {
        return Err(RegistryAddError::Acquisition(AcquisitionError::Bounds));
    }
    Ok(bytes)
}

fn le_u16(bytes: &[u8], offset: usize) -> Result<u16, RegistryAddError> {
    let value = bytes
        .get(offset..offset.saturating_add(2))
        .ok_or(RegistryAddError::UnsupportedArchive)?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn le_u32(bytes: &[u8], offset: usize) -> Result<u32, RegistryAddError> {
    let value = bytes
        .get(offset..offset.saturating_add(4))
        .ok_or(RegistryAddError::UnsupportedArchive)?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

fn confined_path(raw: &str) -> Result<PathBuf, RegistryAddError> {
    if raw.is_empty()
        || raw.starts_with('/')
        || raw.contains(['\\', '\0'])
        || (raw.len() >= 2 && raw.as_bytes()[0].is_ascii_alphabetic() && raw.as_bytes()[1] == b':')
    {
        return Err(RegistryAddError::UnsupportedArchive);
    }
    let mut path = PathBuf::new();
    for component in raw.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(RegistryAddError::UnsupportedArchive),
            value => path.push(value),
        }
    }
    if path.as_os_str().is_empty() {
        return Err(RegistryAddError::UnsupportedArchive);
    }
    Ok(path)
}

fn create_confined_directories(root: &Path, parent: &Path) -> Result<(), RegistryAddError> {
    let relative = parent
        .strip_prefix(root)
        .map_err(|_| RegistryAddError::UnsupportedArchive)?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(RegistryAddError::UnsupportedArchive);
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(RegistryAddError::UnsupportedArchive);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .map_err(|error| RegistryAddError::Acquisition(AcquisitionError::Io(error)))?;
            }
            Err(_) => return Err(RegistryAddError::UnsupportedArchive),
        }
    }
    Ok(())
}

fn supported_source_name(path: &str) -> bool {
    let Some(extension) = Path::new(path).extension().and_then(|value| value.to_str()) else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "rs" | "py"
            | "pyi"
            | "ts"
            | "tsx"
            | "js"
            | "mjs"
            | "cjs"
            | "jsx"
            | "go"
            | "java"
            | "cs"
            | "c"
            | "h"
            | "cc"
            | "cpp"
            | "cxx"
            | "hh"
            | "hpp"
            | "hxx"
            | "m"
            | "mm"
    )
}

fn hex(value: &[u8; 32]) -> String {
    let mut output = String::with_capacity(64);
    for byte in value {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "backend-registry-stage-{}-{id}",
            std::process::id()
        ))
    }

    fn tar_file(name: &str, bytes: &[u8]) -> Vec<u8> {
        let mut header = [0_u8; TAR_BLOCK_BYTES];
        header[..name.len()].copy_from_slice(name.as_bytes());
        let size = format!("{:011o}\0", bytes.len());
        header[124..136].copy_from_slice(size.as_bytes());
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        let mut archive = header.to_vec();
        archive.extend_from_slice(bytes);
        archive.resize(archive.len().div_ceil(TAR_BLOCK_BYTES) * TAR_BLOCK_BYTES, 0);
        archive.resize(archive.len() + TAR_BLOCK_BYTES * 2, 0);
        archive
    }

    #[test]
    fn tar_archive_materializes_source_and_cleans_up() {
        let root = scratch();
        let coordinate = PackageCoordinate::parse("pkg:cargo/demo@1.0.0").expect("coordinate");
        let archive = tar_file("package/src/lib.rs", b"pub fn from_registry() {}");
        let staged = stage_archive(&coordinate, &archive, &root).expect("stage archive");
        let source = fs::read(staged.path().join("package/src/lib.rs")).expect("read source");
        assert_eq!(source, b"pub fn from_registry() {}");
        let path = staged.path().to_path_buf();
        drop(staged);
        assert!(!path.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn archive_path_traversal_is_rejected_before_writing_outside_the_jail() {
        let root = scratch();
        let coordinate = PackageCoordinate::parse("pkg:cargo/demo@1.0.0").expect("coordinate");
        let archive = tar_file("../outside.rs", b"must not escape");
        assert!(matches!(
            stage_archive(&coordinate, &archive, &root),
            Err(RegistryAddError::UnsupportedArchive)
        ));
        assert!(!root.parent().unwrap_or(&root).join("outside.rs").exists());
        let _ = fs::remove_dir_all(root);
    }
}
