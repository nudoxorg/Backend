//! Resolves one canonical package URL beneath one explicit local package-store root.
//!
//! Resolution never searches unrelated directories and never guesses a source entrypoint. The
//! PURL must name a package-relative subpath; canonicalization then proves the selected file stays
//! beneath the resolved package directory before any bytes are read.

use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

use backend_library::interface::{
    PackageEcosystem, PackagePathComponentError, PackageSourceCause, PackageSourceIoFact,
    PackageSourceIoPhase, PackageTextRange, PackageUrl,
};

use crate::application::LocalPackageRootSet;

/// Maximum source bytes read by one package compilation request.
pub const MAX_LOCAL_PACKAGE_SOURCE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_CARGO_REGISTRY_NAMESPACES: usize = 256;

/// Owned source lease whose paths and bytes all came from one successful resolution.
#[derive(Debug)]
pub(crate) struct ResolvedPackageSource {
    pub(crate) package_root: PathBuf,
    pub(crate) source_path: PathBuf,
    pub(crate) relative_source: Box<str>,
    pub(crate) bytes: Box<[u8]>,
}

pub(crate) fn resolve(
    roots: LocalPackageRootSet<'_>,
    package: &PackageUrl,
) -> Result<ResolvedPackageSource, PackageSourceCause> {
    let store = roots
        .select(package.ecosystem)
        .ok_or(PackageSourceCause::RootUnavailable {
            ecosystem: package.ecosystem,
        })?;
    let subpath = package.subpath.ok_or(PackageSourceCause::SubpathRequired {
        ecosystem: package.ecosystem,
    })?;
    let canonical_store = canonicalize(store, PackageSourceIoPhase::CanonicalizeStore)?;
    let package_path = package_directory(&canonical_store, package)?;
    if !package_path.is_dir() {
        return Err(PackageSourceCause::PackageUnavailable {
            path: package_path.into_boxed_path(),
        });
    }
    let canonical_package = canonicalize(&package_path, PackageSourceIoPhase::CanonicalizePackage)?;
    if !canonical_package.starts_with(&canonical_store) {
        return Err(PackageSourceCause::PackageEscapesStore {
            store: canonical_store.into_boxed_path(),
            package: canonical_package.into_boxed_path(),
        });
    }
    let mut source_path = canonical_package.clone();
    push_range_components(&mut source_path, package, subpath)?;
    if !source_path.is_file() {
        return Err(PackageSourceCause::SourceUnavailable {
            path: source_path.into_boxed_path(),
        });
    }
    let canonical_source = canonicalize(&source_path, PackageSourceIoPhase::CanonicalizeSource)?;
    if !canonical_source.starts_with(&canonical_package) {
        return Err(PackageSourceCause::SourceEscapesPackage {
            package: canonical_package.into_boxed_path(),
            source: canonical_source.into_boxed_path(),
        });
    }
    let metadata = fs::metadata(&canonical_source).map_err(|source| PackageSourceCause::Io {
        phase: PackageSourceIoPhase::SourceMetadata,
        path: canonical_source.clone().into_boxed_path(),
        source: io_fact(&source),
    })?;
    if !metadata.is_file() {
        return Err(PackageSourceCause::SourceUnavailable {
            path: canonical_source.into_boxed_path(),
        });
    }
    if metadata.len() > MAX_LOCAL_PACKAGE_SOURCE_BYTES {
        return Err(PackageSourceCause::SourceTooLarge {
            observed: metadata.len(),
            maximum: MAX_LOCAL_PACKAGE_SOURCE_BYTES,
        });
    }
    let file = fs::File::open(&canonical_source).map_err(|source| PackageSourceCause::Io {
        phase: PackageSourceIoPhase::ReadSource,
        path: canonical_source.clone().into_boxed_path(),
        source: io_fact(&source),
    })?;
    let capacity = usize::try_from(metadata.len())
        .unwrap_or(usize::MAX)
        .min(usize::try_from(MAX_LOCAL_PACKAGE_SOURCE_BYTES).unwrap_or(usize::MAX));
    let mut bytes = Vec::with_capacity(capacity);
    file.take(MAX_LOCAL_PACKAGE_SOURCE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| PackageSourceCause::Io {
            phase: PackageSourceIoPhase::ReadSource,
            path: canonical_source.clone().into_boxed_path(),
            source: io_fact(&source),
        })?;
    let observed = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if observed > MAX_LOCAL_PACKAGE_SOURCE_BYTES {
        return Err(PackageSourceCause::SourceTooLarge {
            observed,
            maximum: MAX_LOCAL_PACKAGE_SOURCE_BYTES,
        });
    }
    let relative_source = decoded_relative_path(package, subpath)?.into_boxed_str();
    Ok(ResolvedPackageSource {
        package_root: canonical_package,
        source_path: canonical_source,
        relative_source,
        bytes: bytes.into_boxed_slice(),
    })
}

fn package_directory(root: &Path, package: &PackageUrl) -> Result<PathBuf, PackageSourceCause> {
    match package.ecosystem {
        PackageEcosystem::Cargo => cargo_directory(root, package),
        PackageEcosystem::Npm => {
            let mut path = root.to_path_buf();
            push_package_name(&mut path, package)?;
            Ok(path)
        }
        PackageEcosystem::Pypi => {
            let mut path = root.to_path_buf();
            let name = decoded_cell(package, package.name)?;
            let version = decoded_cell(package, package.version)?;
            path.push(format!("{name}-{version}"));
            Ok(path)
        }
        PackageEcosystem::Golang => {
            let mut path = root.to_path_buf();
            let name = decoded_package_name(package)?;
            let version = decoded_cell(package, package.version)?;
            path.push(format!("{name}@{version}"));
            Ok(path)
        }
        PackageEcosystem::Maven => {
            let mut path = root.to_path_buf();
            if let Some(namespace) = package.namespace {
                for group in package[namespace].split('.') {
                    push_decoded_component(&mut path, group, namespace)?;
                }
            }
            push_range_component(&mut path, package, package.name)?;
            push_range_component(&mut path, package, package.version)?;
            Ok(path)
        }
        PackageEcosystem::Nuget => {
            let mut path = root.to_path_buf();
            push_range_component(&mut path, package, package.name)?;
            push_range_component(&mut path, package, package.version)?;
            Ok(path)
        }
        PackageEcosystem::Generic => {
            let mut path = root.to_path_buf();
            push_package_name(&mut path, package)?;
            push_range_component(&mut path, package, package.version)?;
            Ok(path)
        }
    }
}

fn cargo_directory(root: &Path, package: &PackageUrl) -> Result<PathBuf, PackageSourceCause> {
    let name = decoded_cell(package, package.name)?;
    let version = decoded_cell(package, package.version)?;
    let stem = format!("{name}-{version}");
    let direct = root.join(&stem);
    if direct.is_dir() {
        return Ok(direct);
    }
    let directory = fs::read_dir(root).map_err(|source| PackageSourceCause::Io {
        phase: PackageSourceIoPhase::EnumerateRegistry,
        path: root.to_path_buf().into_boxed_path(),
        source: io_fact(&source),
    })?;
    let mut namespaces = Vec::new();
    for entry in directory {
        if namespaces.len() >= MAX_CARGO_REGISTRY_NAMESPACES {
            return Err(PackageSourceCause::RegistryNamespaceCapacity {
                observed: namespaces.len().saturating_add(1),
                maximum: MAX_CARGO_REGISTRY_NAMESPACES,
            });
        }
        let entry = entry.map_err(|source| PackageSourceCause::Io {
            phase: PackageSourceIoPhase::EnumerateRegistry,
            path: root.to_path_buf().into_boxed_path(),
            source: io_fact(&source),
        })?;
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            namespaces.push(entry.path());
        }
    }
    namespaces.sort();
    let mut matches = namespaces
        .into_iter()
        .map(|namespace| namespace.join(&stem))
        .filter(|candidate| candidate.is_dir());
    let Some(first) = matches.next() else {
        return Ok(direct);
    };
    if let Some(second) = matches.next() {
        return Err(PackageSourceCause::RegistryNamespaceAmbiguous {
            first: first.into_boxed_path(),
            second: second.into_boxed_path(),
        });
    }
    Ok(first)
}

fn push_package_name(path: &mut PathBuf, package: &PackageUrl) -> Result<(), PackageSourceCause> {
    if let Some(namespace) = package.namespace {
        push_range_components(path, package, namespace)?;
    }
    push_range_component(path, package, package.name)
}

fn decoded_package_name(package: &PackageUrl) -> Result<String, PackageSourceCause> {
    let start = package
        .namespace
        .map_or(package.name.start, |value| value.start);
    decoded_cell(
        package,
        PackageTextRange {
            start,
            end: package.name.end,
        },
    )
}

fn push_range_components(
    path: &mut PathBuf,
    package: &PackageUrl,
    range: PackageTextRange,
) -> Result<(), PackageSourceCause> {
    for component in package[range].split('/') {
        push_decoded_component(path, component, range)?;
    }
    Ok(())
}

fn push_range_component(
    path: &mut PathBuf,
    package: &PackageUrl,
    range: PackageTextRange,
) -> Result<(), PackageSourceCause> {
    let decoded = decoded_cell(package, range)?;
    validate_component(&decoded, range)?;
    path.push(decoded);
    Ok(())
}

fn push_decoded_component(
    path: &mut PathBuf,
    component: &str,
    range: PackageTextRange,
) -> Result<(), PackageSourceCause> {
    let decoded = percent_decode(component, range)?;
    validate_component(&decoded, range)?;
    path.push(decoded);
    Ok(())
}

fn decoded_cell(
    package: &PackageUrl,
    range: PackageTextRange,
) -> Result<String, PackageSourceCause> {
    percent_decode(&package[range], range)
}

fn decoded_relative_path(
    package: &PackageUrl,
    range: PackageTextRange,
) -> Result<String, PackageSourceCause> {
    let mut normalized = String::with_capacity(usize::from(range.end - range.start));
    for (ordinal, component) in package[range].split('/').enumerate() {
        let decoded = percent_decode(component, range)?;
        validate_component(&decoded, range)?;
        if ordinal != 0 {
            normalized.push('/');
        }
        normalized.push_str(&decoded);
    }
    Ok(normalized)
}

fn validate_component(component: &str, range: PackageTextRange) -> Result<(), PackageSourceCause> {
    if component.is_empty() || matches!(component, "." | "..") {
        return Err(PackageSourceCause::InvalidComponent {
            range,
            cause: PackagePathComponentError::Traversal,
        });
    }
    if component
        .bytes()
        .any(|byte| matches!(byte, 0 | b'/' | b'\\'))
    {
        return Err(PackageSourceCause::InvalidComponent {
            range,
            cause: PackagePathComponentError::EncodedSeparator,
        });
    }
    Ok(())
}

fn percent_decode(value: &str, range: PackageTextRange) -> Result<String, PackageSourceCause> {
    if !value.as_bytes().contains(&b'%') {
        return Ok(value.to_owned());
    }
    let mut output = Vec::with_capacity(value.len());
    let mut offset = 0;
    while offset < value.len() {
        if value.as_bytes()[offset] == b'%' {
            let high = hex(value.as_bytes()[offset + 1]);
            let low = hex(value.as_bytes()[offset + 2]);
            output.push((high << 4) | low);
            offset += 3;
        } else {
            output.push(value.as_bytes()[offset]);
            offset += 1;
        }
    }
    String::from_utf8(output).map_err(|_| PackageSourceCause::InvalidComponent {
        range,
        cause: PackagePathComponentError::InvalidUtf8,
    })
}

const fn hex(value: u8) -> u8 {
    match value {
        b'0'..=b'9' => value - b'0',
        b'A'..=b'F' => value - b'A' + 10,
        _ => 0,
    }
}

fn canonicalize(path: &Path, phase: PackageSourceIoPhase) -> Result<PathBuf, PackageSourceCause> {
    fs::canonicalize(path).map_err(|source| PackageSourceCause::Io {
        phase,
        path: path.to_path_buf().into_boxed_path(),
        source: io_fact(&source),
    })
}

fn io_fact(source: &io::Error) -> PackageSourceIoFact {
    PackageSourceIoFact {
        kind: source.kind(),
        raw_os_code: source.raw_os_error(),
    }
}
