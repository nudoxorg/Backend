//! Bounded reads beneath a pinned local-project directory.

use backend_platform::directory::DirectoryCapability;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

/// Opens a package-relative regular file through a held directory capability.
/// Every intermediate directory and the final file are opened without
/// following symlinks or reparse points.
pub(crate) fn open_relative_source(
    root: &DirectoryCapability,
    relative: &Path,
) -> io::Result<File> {
    let spelling = relative
        .to_str()
        .ok_or_else(|| invalid("source path is not UTF-8"))?;
    if spelling.starts_with('/') || spelling.starts_with('\\') || spelling.contains('\0') {
        return Err(invalid("source path must be package-relative"));
    }
    let normalized = spelling.replace('\\', "/");
    let mut parts = Vec::<&str>::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return Err(invalid("source path escapes its package"));
                }
            }
            component => parts.push(component),
        }
    }
    let (name, parents) = parts
        .split_last()
        .ok_or_else(|| invalid("source path has no file name"))?;
    let mut directory = root.clone();
    for parent in parents {
        directory = directory.open_dir(parent)?;
    }
    directory.open_file_read(name)
}

/// Reads one package-relative UTF-8 file, bounded by `maximum_bytes`.
/// The returned path is only an editor/display hint. File authority comes
/// from the held descriptor used for the read, never from that pathname.
pub(crate) fn read_text_under(
    package_root: &Path,
    path: &Path,
    maximum_bytes: u64,
) -> io::Result<(String, Option<PathBuf>)> {
    let (bytes, editor_hint) = read_bytes_under(package_root, path, maximum_bytes)?;
    let text = String::from_utf8(bytes).map_err(|_| invalid("source file is not UTF-8"))?;
    Ok((text, editor_hint))
}

/// Reads bounded bytes beneath the pinned package root. Callers that render
/// lossy text, such as a README, can preserve their decoding policy without
/// reopening the path.
pub(crate) fn read_bytes_under(
    package_root: &Path,
    path: &Path,
    maximum_bytes: u64,
) -> io::Result<(Vec<u8>, Option<PathBuf>)> {
    let relative = path.strip_prefix(package_root).unwrap_or(path);
    let root = DirectoryCapability::open_read_only_source(package_root)?;
    let file = open_relative_source(&root, relative)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > maximum_bytes {
        return Err(invalid(
            "source file exceeds the read bound or is not regular",
        ));
    }
    let reserve = usize::try_from(metadata.len()).unwrap_or(0);
    let mut bytes =
        Vec::with_capacity(reserve.min(usize::try_from(maximum_bytes).unwrap_or(usize::MAX)));
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum_bytes {
        return Err(invalid("source file exceeds the read bound"));
    }
    let editor_hint = editor_hint_for_opened_file(package_root, relative, &metadata);
    Ok((bytes, editor_hint))
}

/// Produces a canonical editor/display hint only when the current pathname
/// still names the object opened through the directory capability.
pub(crate) fn editor_hint_for_opened_file(
    package_root: &Path,
    relative: &Path,
    opened_metadata: &std::fs::Metadata,
) -> Option<PathBuf> {
    let canonical_root = package_root.canonicalize().ok()?;
    let candidate = package_root.join(relative).canonicalize().ok()?;
    if !candidate.starts_with(canonical_root) {
        return None;
    }
    let path_metadata = std::fs::metadata(&candidate).ok()?;
    same_file(opened_metadata, &path_metadata).then_some(candidate)
}

#[cfg(unix)]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(windows)]
fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;
    left.volume_serial_number() == right.volume_serial_number()
        && left.file_index() == right.file_index()
}

#[cfg(not(any(unix, windows)))]
fn same_file(_: &std::fs::Metadata, _: &std::fs::Metadata) -> bool {
    false
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
