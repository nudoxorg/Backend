//! Bounded verification and streaming extraction for a local Cargo archive.
//!
//! The archive is hashed before extraction, then extracted into a private
//! sibling directory. Its files are synced and recorded in a small provenance
//! receipt before the directory is published. A checksum-specific target
//! keeps an older verified tree intact while a changed archive is staged.

use super::{Release, SourceError};
use flate2::read::GzDecoder;
use sha2::{Digest as _, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Component, Path, PathBuf};

/// The largest compressed archive admitted from Cargo's local cache.
const MAX_ARCHIVE: u64 = 64 << 20;
/// The largest expanded tar stream admitted, including headers and padding.
const MAX_EXPANDED: u64 = 512 << 20;
/// A bound on a single file, path and archive entry count.
const MAX_FILE: u64 = 64 << 20;
const MAX_FILES: usize = 100_000;
const MAX_PATH_BYTES: usize = 1_024;
const MAX_DEPTH: usize = 64;
const MAX_METADATA: u64 = 64 << 10;
const MAX_MANIFEST: u64 = 16 << 20;
const BLOCK: usize = 512;
const BUFFER: usize = 32 << 10;

/// Unpacks `archive` for `release` under the effective authority's private
/// cache. It never publishes a partial tree or removes a verified tree before
/// a replacement has been fully checked.
pub(super) fn unpack(
    archive: &Path,
    checksum: &str,
    authority: &str,
    release: &Release,
    unpacked: &Path,
) -> Result<PathBuf, SourceError> {
    let io_error = |error: io::Error| SourceError::Io {
        release: release.clone(),
        reason: error.to_string(),
    };
    let malformed = |reason: String| SourceError::Archive {
        release: release.clone(),
        reason,
    };
    if !is_sha256(checksum) {
        return Err(SourceError::UnverifiedArchive(release.clone()));
    }

    let checksum = checksum.to_ascii_lowercase();
    let stem = release.stem();
    let authority_root = unpacked.join(authority);
    let release_root = authority_root.join(&stem);
    let outer = release_root.join(&checksum);
    let root = outer.join(&stem);
    if cache_matches(&root, authority, release, &checksum) {
        return Ok(root);
    }

    let mut file = File::open(archive).map_err(io_error)?;
    let actual = archive_hash(&mut file).map_err(|error| {
        if error.kind() == io::ErrorKind::InvalidData {
            malformed(error.to_string())
        } else {
            io_error(error)
        }
    })?;
    if !checksum.eq_ignore_ascii_case(&actual) {
        return Err(SourceError::Integrity {
            release: release.clone(),
            expected: checksum,
            actual,
        });
    }
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;

    // `unpacked` is the private application child beneath the owner's private
    // data directory. Do not create cache state until this exact archive has
    // passed its published checksum.
    backend_platform::durable::ensure_private_child_directory(unpacked).map_err(io_error)?;
    ensure_private_directory(&authority_root).map_err(io_error)?;
    ensure_private_directory(&release_root).map_err(io_error)?;
    recover_verified_backup(&release_root, &outer, authority, release, &checksum)
        .map_err(io_error)?;
    if cache_matches(&root, authority, release, &checksum) {
        return Ok(root);
    }

    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = release_root.join(format!(".stage.{}.{nonce}", std::process::id()));
    create_private_directory(&staging).map_err(io_error)?;
    let staged = (|| {
        let package = staging.join(&stem);
        create_private_directory(&package).map_err(io_error)?;
        let mut decoder = Expanded::new(GzDecoder::new(&mut file));
        extract_tar(&mut decoder, &package, &stem).map_err(malformed)?;
        let mut tail = [0; BUFFER];
        while decoder
            .read(&mut tail)
            .map_err(|error| malformed(format!("gzip: {error}")))?
            != 0
        {}
        drop(decoder);
        let actual_after = archive_hash(&mut file).map_err(io_error)?;
        if !checksum.eq_ignore_ascii_case(&actual_after) {
            return Err(SourceError::Integrity {
                release: release.clone(),
                expected: checksum.clone(),
                actual: actual_after,
            });
        }
        let manifest = read_bounded(&package.join("Cargo.toml"), MAX_MANIFEST).map_err(io_error)?;
        let manifest = std::str::from_utf8(&manifest)
            .map_err(|_| malformed("the crate manifest is not UTF-8".to_owned()))?;
        write_synced(
            &staging.join("Cargo.toml"),
            boundary(&stem, manifest).as_bytes(),
        )
        .map_err(io_error)?;
        let tree_hash = tree_digest(&package).ok_or_else(|| {
            malformed("the extracted tree exceeds its integrity bounds".to_owned())
        })?;
        write_synced(
            &staging.join("provenance"),
            provenance(authority, release, &checksum, &tree_hash).as_bytes(),
        )
        .map_err(io_error)?;
        sync_directories(&staging).map_err(io_error)?;
        sync_directory(&staging).map_err(io_error)?;
        Ok::<(), SourceError>(())
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    // All archive and tree facts are verified before the existing target is
    // moved. If publication fails, put that prior target back in place.
    let backup = if path_exists(&outer) {
        let backup = release_root.join(format!(".{checksum}.old.{}.{}", std::process::id(), nonce));
        fs::rename(&outer, &backup).map_err(io_error)?;
        Some(backup)
    } else {
        None
    };
    if let Err(error) = fs::rename(&staging, &outer) {
        if let Some(backup) = &backup {
            let _ = fs::rename(backup, &outer);
        }
        let _ = fs::remove_dir_all(&staging);
        if cache_matches(&root, authority, release, &checksum) {
            return Ok(root);
        }
        return Err(io_error(error));
    }
    sync_directory(&release_root).map_err(io_error)?;
    if let Some(backup) = backup {
        remove_path(&backup).map_err(io_error)?;
        sync_directory(&release_root).map_err(io_error)?;
    }
    Ok(root)
}

/// Whether this tree belongs to the exact authority, release and archive
/// checksum and still has the extracted bytes recorded at publication.
pub(super) fn cache_matches(
    root: &Path,
    authority: &str,
    release: &Release,
    checksum: &str,
) -> bool {
    if !root.join("Cargo.toml").is_file() {
        return false;
    }
    let Some(parent) = root.parent() else {
        return false;
    };
    let Some(tree_hash) = tree_digest(root) else {
        return false;
    };
    read_bounded(&parent.join("provenance"), 1_024)
        .ok()
        .is_some_and(|contents| {
            contents == provenance(authority, release, checksum, &tree_hash).as_bytes()
        })
}

fn provenance(authority: &str, release: &Release, checksum: &str, tree_hash: &str) -> String {
    format!(
        "authority={authority}\nrelease={}\narchive-sha256={}\ntree-sha256={tree_hash}\n",
        release.stem(),
        checksum.to_ascii_lowercase()
    )
}

fn archive_hash(file: &mut File) -> io::Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0; BUFFER];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        total = total.checked_add(read as u64).ok_or_else(bounds_error)?;
        if total > MAX_ARCHIVE {
            return Err(bounds_error());
        }
        digest.update(&buffer[..read]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(hex(&digest.finalize()))
}

/// A read wrapper that counts gzip-expanded bytes, including tar metadata.
struct Expanded<R> {
    inner: R,
    read: u64,
}

impl<R> Expanded<R> {
    fn new(inner: R) -> Self {
        Self { inner, read: 0 }
    }
}

impl<R: io::Read> io::Read for Expanded<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let remaining = MAX_EXPANDED.saturating_sub(self.read).saturating_add(1);
        let limit = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = self.inner.read(&mut buffer[..limit])?;
        self.read = self
            .read
            .checked_add(read as u64)
            .ok_or_else(bounds_error)?;
        if self.read > MAX_EXPANDED {
            return Err(bounds_error());
        }
        Ok(read)
    }
}

/// Stream-extracts regular files from the one `<name>-<version>/` Cargo crate
/// root. Every entry and payload is bounded before it can consume unbounded
/// memory or disk; links and special files are rejected.
fn extract_tar<R: io::Read>(reader: &mut R, package: &Path, stem: &str) -> Result<(), String> {
    let mut entries = 0usize;
    let mut files = 0usize;
    let mut total = 0u64;
    let mut pending_path = None;
    let mut has_manifest = false;
    loop {
        let mut header = [0; BLOCK];
        reader
            .read_exact(&mut header)
            .map_err(|error| format!("tar header: {error}"))?;
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        entries = entries
            .checked_add(1)
            .ok_or_else(|| "too many archive entries".to_owned())?;
        if entries > MAX_FILES {
            return Err("too many archive entries".to_owned());
        }
        validate_tar_checksum(&header)?;
        let raw_name = tar_name(&header)?;
        let size = tar_octal(&header[124..136])
            .ok_or_else(|| "archive entry has no readable size".to_owned())?;
        let kind = header[156];
        if matches!(kind, b'x' | b'L' | b'g') {
            if size > MAX_METADATA {
                return Err("archive path metadata is too large".to_owned());
            }
            let metadata = read_payload(reader, size)?;
            skip_padding(reader, size)?;
            if kind == b'x' {
                pending_path = pax_path(&metadata)?;
            } else if kind == b'L' {
                pending_path = Some(tar_text(&metadata)?);
            } else if pax_path(&metadata)?.is_some() {
                return Err("global archive metadata cannot rename crate files".to_owned());
            }
            continue;
        }
        let name = pending_path.take().unwrap_or(raw_name);
        let path = confined(&name, stem)?;
        let depth = path.components().count();
        if depth > MAX_DEPTH || path.as_os_str().len() > MAX_PATH_BYTES {
            return Err("archive path exceeds its bounds".to_owned());
        }
        match kind {
            0 | b'0' | b'7' => {
                if path.as_os_str().is_empty() || size > MAX_FILE {
                    return Err("archive file exceeds its bounds".to_owned());
                }
                files = files
                    .checked_add(1)
                    .ok_or_else(|| "too many archive files".to_owned())?;
                total = total
                    .checked_add(size)
                    .ok_or_else(|| "archive is too large".to_owned())?;
                if files > MAX_FILES || total > MAX_EXPANDED {
                    return Err("archive files exceed their bounds".to_owned());
                }
                let target = package.join(&path);
                if let Some(parent) = target.parent() {
                    ensure_private_tree_directory(package, parent)
                        .map_err(|error| error.to_string())?;
                }
                let mut output = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&target)
                    .map_err(|error| format!("create archive file: {error}"))?;
                copy_exact(reader, &mut output, size)?;
                output
                    .sync_all()
                    .map_err(|error| format!("sync archive file: {error}"))?;
                has_manifest |= path == Path::new("Cargo.toml");
                skip_padding(reader, size)?;
            }
            b'5' => {
                if size != 0 {
                    skip_exact(reader, size)?;
                    skip_padding(reader, size)?;
                }
                if !path.as_os_str().is_empty() {
                    ensure_private_tree_directory(package, &package.join(path))
                        .map_err(|error| error.to_string())?;
                }
            }
            _ => return Err("Cargo archive contains a link or special file".to_owned()),
        }
    }
    if !has_manifest {
        return Err("archive has no Cargo.toml under its package root".to_owned());
    }
    Ok(())
}

fn tar_name(header: &[u8; BLOCK]) -> Result<String, String> {
    let name = tar_text(&header[..100])?;
    let prefix = if &header[257..262] == b"ustar" {
        tar_text(&header[345..500])?
    } else {
        String::new()
    };
    if prefix.is_empty() {
        Ok(name)
    } else if name.is_empty() {
        Ok(prefix)
    } else {
        Ok(format!("{prefix}/{name}"))
    }
}

fn tar_text(bytes: &[u8]) -> Result<String, String> {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    std::str::from_utf8(&bytes[..end])
        .map(str::to_owned)
        .map_err(|_| "archive path is not UTF-8".to_owned())
}

fn tar_octal(bytes: &[u8]) -> Option<u64> {
    let mut value = 0u64;
    let mut digits = 0u8;
    for byte in bytes.iter().copied() {
        if byte == 0 || (byte == b' ' && digits > 0) {
            break;
        }
        if byte == b' ' {
            continue;
        }
        if !(b'0'..=b'7').contains(&byte) {
            return None;
        }
        value = value.checked_mul(8)?.checked_add(u64::from(byte - b'0'))?;
        digits = digits.saturating_add(1);
    }
    Some(value)
}

fn validate_tar_checksum(header: &[u8; BLOCK]) -> Result<(), String> {
    let expected =
        tar_octal(&header[148..156]).ok_or_else(|| "archive header has no checksum".to_owned())?;
    let actual = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if (148..156).contains(&index) {
                u64::from(b' ')
            } else {
                u64::from(*byte)
            }
        })
        .sum::<u64>();
    if actual == expected {
        Ok(())
    } else {
        Err("archive header checksum does not match".to_owned())
    }
}

fn confined(name: &str, stem: &str) -> Result<PathBuf, String> {
    if name.contains('\\') || name.starts_with('/') {
        return Err(format!("`{name}` is not a confined crate path"));
    }
    let mut parts = Path::new(name).components().peekable();
    while matches!(parts.peek(), Some(Component::CurDir)) {
        parts.next();
    }
    match parts.next() {
        Some(Component::Normal(top)) if top == stem => {}
        _ => return Err(format!("`{name}` is not under `{stem}/`")),
    }
    let mut below = PathBuf::new();
    for part in parts {
        match part {
            Component::Normal(part) => below.push(part),
            Component::CurDir => {}
            _ => return Err(format!("`{name}` leaves the package root")),
        }
    }
    Ok(below)
}

fn pax_path(bytes: &[u8]) -> Result<Option<String>, String> {
    let mut at = 0usize;
    let mut path = None;
    while at < bytes.len() {
        let rest = &bytes[at..];
        let space = rest
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or_else(|| "malformed PAX metadata".to_owned())?;
        let length = std::str::from_utf8(&rest[..space])
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| "malformed PAX metadata".to_owned())?;
        let end = at
            .checked_add(length)
            .ok_or_else(|| "PAX metadata overflow".to_owned())?;
        if length == 0 || end > bytes.len() || bytes[end - 1] != b'\n' {
            return Err("malformed PAX metadata".to_owned());
        }
        let value = &bytes[at + space + 1..end - 1];
        if let Some(value) = value.strip_prefix(b"path=") {
            path = Some(
                std::str::from_utf8(value)
                    .map_err(|_| "PAX path is not UTF-8".to_owned())?
                    .to_owned(),
            );
        }
        at = end;
    }
    Ok(path)
}

fn read_payload(reader: &mut impl io::Read, size: u64) -> Result<Vec<u8>, String> {
    let size = usize::try_from(size).map_err(|_| "archive metadata is too large".to_owned())?;
    let mut payload = vec![0; size];
    reader
        .read_exact(&mut payload)
        .map_err(|error| format!("archive metadata: {error}"))?;
    Ok(payload)
}

fn copy_exact(
    reader: &mut impl io::Read,
    writer: &mut impl io::Write,
    mut size: u64,
) -> Result<(), String> {
    let mut buffer = [0; BUFFER];
    while size > 0 {
        let limit = usize::try_from(size.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = reader
            .read(&mut buffer[..limit])
            .map_err(|error| format!("read archive file: {error}"))?;
        if read == 0 {
            return Err("archive file ended early".to_owned());
        }
        writer
            .write_all(&buffer[..read])
            .map_err(|error| format!("write archive file: {error}"))?;
        size -= read as u64;
    }
    Ok(())
}

fn skip_exact(reader: &mut impl io::Read, mut size: u64) -> Result<(), String> {
    let mut buffer = [0; BUFFER];
    while size > 0 {
        let limit = usize::try_from(size.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = reader
            .read(&mut buffer[..limit])
            .map_err(|error| format!("skip archive data: {error}"))?;
        if read == 0 {
            return Err("archive entry ended early".to_owned());
        }
        size -= read as u64;
    }
    Ok(())
}

fn skip_padding(reader: &mut impl io::Read, size: u64) -> Result<(), String> {
    let padding = (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64;
    skip_exact(reader, padding)
}

/// The generated workspace that bounds Cargo at the release.
pub(super) fn boundary(stem: &str, manifest: &str) -> String {
    let value = |key: &str| {
        manifest.lines().find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == key).then(|| value.trim().trim_matches('"').to_owned())
        })
    };
    let resolver = value("resolver").unwrap_or_else(|| match value("edition").as_deref() {
        Some("2024") => "3".to_owned(),
        Some("2021") => "2".to_owned(),
        _ => "1".to_owned(),
    });
    format!(
        "# Generated by the desktop app: a registry release unpacked from the local cargo cache.\n\
         # This one-member workspace bounds cargo at the release; the package's files are untouched.\n\
         [workspace]\nmembers = [\"{stem}\"]\nresolver = \"{resolver}\"\n"
    )
}

/// Hashes files and directories in sorted traversal order with bounded depth,
/// entry count, paths, file sizes and a fixed read buffer.
fn tree_digest(root: &Path) -> Option<String> {
    fn visit(
        root: &Path,
        directory: &Path,
        depth: usize,
        entries_seen: &mut usize,
        bytes_seen: &mut u64,
        digest: &mut Sha256,
    ) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        let mut children = Vec::new();
        for entry in fs::read_dir(directory).ok()? {
            let entry = entry.ok()?;
            *entries_seen = entries_seen.checked_add(1)?;
            if *entries_seen > MAX_FILES {
                return None;
            }
            children.push(entry);
        }
        children.sort_by_key(fs::DirEntry::file_name);
        for entry in children {
            let path = entry.path();
            let relative = path.strip_prefix(root).ok()?;
            let name = relative.to_str()?;
            if name.len() > MAX_PATH_BYTES {
                return None;
            }
            digest.update((name.len() as u64).to_be_bytes());
            digest.update(name.as_bytes());
            let kind = entry.file_type().ok()?;
            if kind.is_dir() {
                digest.update([b'd']);
                visit(root, &path, depth + 1, entries_seen, bytes_seen, digest)?;
            } else if kind.is_file() {
                digest.update([b'f']);
                let expected = entry.metadata().ok()?.len();
                if expected > MAX_FILE {
                    return None;
                }
                *bytes_seen = bytes_seen.checked_add(expected)?;
                if *bytes_seen > MAX_EXPANDED {
                    return None;
                }
                digest.update(expected.to_be_bytes());
                let mut file = File::open(&path).ok()?;
                let mut buffer = [0; BUFFER];
                let mut read_total = 0u64;
                loop {
                    let read = file.read(&mut buffer).ok()?;
                    if read == 0 {
                        break;
                    }
                    read_total = read_total.checked_add(read as u64)?;
                    if read_total > expected || read_total > MAX_FILE {
                        return None;
                    }
                    digest.update(&buffer[..read]);
                }
                if read_total != expected {
                    return None;
                }
            } else {
                return None;
            }
        }
        Some(())
    }

    let mut digest = Sha256::new();
    let mut entries = 0;
    let mut bytes = 0;
    visit(root, root, 0, &mut entries, &mut bytes, &mut digest)?;
    Some(hex(&digest.finalize()))
}

fn read_bounded(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(bounds_error());
    }
    Ok(bytes)
}

fn write_synced(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn ensure_private_tree_directory(root: &Path, target: &Path) -> io::Result<()> {
    if !target.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory leaves staging root",
        ));
    }
    if target == root {
        return Ok(());
    }
    let parent = target.parent().ok_or_else(|| io::ErrorKind::InvalidInput)?;
    ensure_private_tree_directory(root, parent)?;
    match backend_platform::durable::ensure_private_directory(target) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            backend_platform::durable::ensure_private_directory(target)
        }
        Err(error) => Err(error),
    }
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    backend_platform::durable::ensure_private_directory(path)
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

fn sync_directories(root: &Path) -> io::Result<()> {
    fn visit(root: &Path, depth: usize, entries: &mut usize) -> io::Result<()> {
        if depth > MAX_DEPTH {
            return Err(bounds_error());
        }
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            *entries = entries.checked_add(1).ok_or_else(bounds_error)?;
            if *entries > MAX_FILES {
                return Err(bounds_error());
            }
            if entry.file_type()?.is_dir() {
                visit(&entry.path(), depth + 1, entries)?;
            }
        }
        sync_directory(root)
    }
    let mut entries = 0;
    visit(root, 0, &mut entries)
}

fn sync_directory(path: &Path) -> io::Result<()> {
    backend_platform::durability::open_directory(path)?.sync_all()
}

fn recover_verified_backup(
    release_root: &Path,
    target: &Path,
    authority: &str,
    release: &Release,
    checksum: &str,
) -> io::Result<()> {
    if path_exists(target) {
        return Ok(());
    }
    let prefix = format!(".{checksum}.old.");
    let mut candidates = Vec::new();
    for entry in fs::read_dir(release_root)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            candidates.push(entry.path());
            if candidates.len() > MAX_FILES {
                return Err(bounds_error());
            }
        }
    }
    candidates.sort();
    for backup in candidates {
        if cache_matches(&backup.join(release.stem()), authority, release, checksum) {
            fs::rename(backup, target)?;
            sync_directory(release_root)?;
            break;
        }
    }
    Ok(())
}

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn remove_path(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path)
    } else {
        fs::remove_dir_all(path)
    }
}

pub(super) fn is_sha256(checksum: &str) -> bool {
    checksum.len() == 64 && checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn bounds_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "archive exceeds its bounds")
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
