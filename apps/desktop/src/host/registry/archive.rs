//! Unpacks one registry archive (`.crate`: a gzip'd tar whose one top
//! directory is `<name>-<version>`) into this app's directory, checked
//! against the checksum the registry index published for it.
//!
//! The tree lands at `<unpacked>/<stem>/<stem>`: the outer directory holds a
//! generated one-member Cargo workspace, so cargo resolving the release never
//! walks up into a workspace the directory happens to sit in, and the
//! package's own files stay byte for byte what the registry published.
//! Only regular files at confined relative paths under `<stem>/` are written;
//! links and anything else are skipped. The work happens in a temporary
//! sibling that is renamed into place, so a tree is either whole or absent.

use super::{Release, SourceError};
use sha2::{Digest as _, Sha256};
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

/// The largest archive read (crates.io itself caps uploads at 10 MiB).
const MAX_ARCHIVE: u64 = 64 << 20;
/// The largest unpacked size admitted.
const MAX_EXPANDED: u64 = 512 << 20;
const BLOCK: usize = 512;

/// Unpacks `archive` for `release` under `unpacked`; returns the package root.
/// A tree already there is returned as it is.
///
/// # Errors
/// [`SourceError::Integrity`] when the archive is not the published one,
/// [`SourceError::Archive`] when it is not a well-formed crate archive,
/// [`SourceError::Io`] when the file system refuses.
pub(super) fn unpack(
    archive: &Path,
    checksum: Option<&str>,
    release: &Release,
    unpacked: &Path,
) -> Result<PathBuf, SourceError> {
    let stem = release.stem();
    let outer = unpacked.join(&stem);
    let root = outer.join(&stem);
    if root.join("Cargo.toml").is_file() {
        return Ok(root);
    }
    let io = |error: std::io::Error| SourceError::Io {
        release: release.clone(),
        reason: error.to_string(),
    };
    let malformed = |reason: String| SourceError::Archive {
        release: release.clone(),
        reason,
    };
    let mut bytes = Vec::new();
    std::fs::File::open(archive)
        .map_err(io)?
        .take(MAX_ARCHIVE + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > MAX_ARCHIVE {
        return Err(malformed(format!(
            "it is larger than {} MiB",
            MAX_ARCHIVE >> 20
        )));
    }
    let actual = hex(&Sha256::digest(&bytes));
    // Cargo checked the archive when it downloaded it; the registry index on
    // this machine says what it must be, when the index still lists it.
    if let Some(expected) = checksum
        && !expected.eq_ignore_ascii_case(&actual)
    {
        return Err(SourceError::Integrity {
            release: release.clone(),
            expected: expected.to_owned(),
            actual,
        });
    }
    let mut tar = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_slice())
        .take(MAX_EXPANDED + 1)
        .read_to_end(&mut tar)
        .map_err(|error| malformed(format!("gzip: {error}")))?;
    if tar.len() as u64 > MAX_EXPANDED {
        return Err(malformed(format!(
            "it unpacks to more than {} MiB",
            MAX_EXPANDED >> 20
        )));
    }
    let files = entries(&tar, &stem).map_err(malformed)?;
    if !files
        .iter()
        .any(|(path, _)| path == Path::new("Cargo.toml"))
    {
        return Err(malformed(
            "it has no Cargo.toml under its top directory".to_owned(),
        ));
    }
    private_dir(unpacked).map_err(io)?;
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let staging = unpacked.join(format!(".{stem}.{}.{nonce}.tmp", std::process::id()));
    let written = write_tree(&staging, &stem, &files).map_err(io);
    if let Err(error) = written {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
    }
    if let Err(error) = std::fs::rename(&staging, &outer) {
        let _ = std::fs::remove_dir_all(&staging);
        // Another unpack of the same release won the rename: its tree is whole.
        if !root.join("Cargo.toml").is_file() {
            return Err(io(error));
        }
    }
    Ok(root)
}

/// Writes the package's files under `staging/<stem>` and the workspace
/// boundary beside them.
fn write_tree(staging: &Path, stem: &str, files: &[(PathBuf, &[u8])]) -> std::io::Result<()> {
    private_dir(staging)?;
    let package = staging.join(stem);
    for (path, bytes) in files {
        let target = package.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, bytes)?;
    }
    let manifest = files
        .iter()
        .find(|(path, _)| path == Path::new("Cargo.toml"))
        .map_or(&[][..], |(_, bytes)| *bytes);
    std::fs::write(
        staging.join("Cargo.toml"),
        boundary(stem, &String::from_utf8_lossy(manifest)),
    )
}

/// The generated workspace that bounds cargo at the release: one member, the
/// resolver the package's edition implies unless it names one.
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

/// Every regular file under `<stem>/` in `tar`, by its path below `<stem>`.
fn entries<'a>(tar: &'a [u8], stem: &str) -> Result<Vec<(PathBuf, &'a [u8])>, String> {
    let mut files = Vec::new();
    let mut at = 0;
    let mut long_name: Option<String> = None;
    while at + BLOCK <= tar.len() {
        let header = &tar[at..at + BLOCK];
        if header.iter().all(|byte| *byte == 0) {
            break;
        }
        let size = octal(&header[124..136])
            .ok_or_else(|| format!("entry at byte {at} has no readable size"))?;
        let start = at + BLOCK;
        let end = start
            .checked_add(size)
            .filter(|end| *end <= tar.len())
            .ok_or_else(|| format!("entry at byte {at} runs past the archive"))?;
        let data = &tar[start..end];
        at = start + size.div_ceil(BLOCK) * BLOCK;
        let name = long_name.take().unwrap_or_else(|| {
            let name = text(&header[0..100]);
            let prefix = if &header[257..262] == b"ustar" {
                text(&header[345..500])
            } else {
                String::new()
            };
            if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            }
        });
        match header[156] {
            // A PAX extended header: its `path` names the next entry.
            b'x' => long_name = pax_path(data),
            // A GNU long name: the data is the next entry's name.
            b'L' => long_name = Some(text(data)),
            b'0' | 0 | b'7' => {
                if let Some(path) = confined(&name, stem)? {
                    files.push((path, data));
                }
            }
            // Directories are implied by files; links, devices and global
            // headers are not written.
            _ => {}
        }
    }
    Ok(files)
}

/// `name` below `stem/`, refusing anything that would leave it.
fn confined(name: &str, stem: &str) -> Result<Option<PathBuf>, String> {
    let path = Path::new(name);
    let mut parts = path.components();
    match parts.next() {
        Some(Component::Normal(top)) if top == stem => {}
        _ => return Err(format!("`{name}` is not under `{stem}/`")),
    }
    let mut below = PathBuf::new();
    for part in parts {
        match part {
            Component::Normal(part) => below.push(part),
            Component::CurDir => {}
            _ => return Err(format!("`{name}` leaves the package")),
        }
    }
    Ok((!below.as_os_str().is_empty()).then_some(below))
}

fn pax_path(data: &[u8]) -> Option<String> {
    let mut rest = data;
    let mut path = None;
    while !rest.is_empty() {
        let space = rest.iter().position(|byte| *byte == b' ')?;
        let length = std::str::from_utf8(&rest[..space])
            .ok()?
            .parse::<usize>()
            .ok()?;
        let record = rest.get(space + 1..length)?;
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(value) = record.strip_prefix(b"path=") {
            path = Some(String::from_utf8_lossy(value).into_owned());
        }
        rest = rest.get(length..)?;
    }
    path
}

fn text(field: &[u8]) -> String {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

fn octal(field: &[u8]) -> Option<usize> {
    let digits = text(field);
    let digits = digits.trim_matches(|c: char| c == ' ' || c == '\0');
    if digits.is_empty() {
        Some(0)
    } else {
        usize::from_str_radix(digits, 8).ok()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}
