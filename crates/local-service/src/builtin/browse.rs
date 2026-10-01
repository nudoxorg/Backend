//! A project's dependency tree, read from Cargo and the advisory authority.
//!
//! Cargo is the authority for the current target's active dependency graph:
//! `cargo metadata --filter-platform <host>`. Cargo.lock rows outside that
//! graph can also be disabled by feature selection, so the difference is not
//! called "other platforms." When Cargo cannot answer (not installed, no
//! network for a missing download, a stale lockfile under `--locked`), the
//! tree is read from `Cargo.lock` alone and says so.
//!
//! The Cargo half is cached against the bytes of `Cargo.lock` and every
//! member manifest, so a tree read costs one Cargo run per change to the
//! project, not one per read. Advisories are observed on every read, so a
//! refresh shows at once.

use backend_library::browse::{
    LockedInactiveCoverage, LockfileGraphCoverage, ProjectTree, TreeInput, TreeInputPackage,
    TreeSource, build_tree, lockfile_input, metadata_input_with_stable_source_witness,
};
use backend_library::{
    CargoPackageSourceAuthorityFailureV1, CargoPackageSourceAuthorityStateV1,
    CargoPackageSourceAuthorityV1, CargoPackageSourceFileResultV1,
    CargoPackageSourceInventoryCoverageV1, CargoPackageSourceInventoryFailureV1,
    CargoPackageSourceInventoryGapV1, CargoPackageSourceInventoryResultV1,
    CargoPackageSourceInventoryV1, CargoPackageSourcePathV1, CargoPackageSourceReadFailureV1,
    CargoPackageSourceSemanticStatusV1, MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS,
    MAX_CARGO_PACKAGE_SOURCE_INVENTORY_SCAN_ENTRIES, PackageReference,
};
use backend_platform::directory::{DirectoryCapability, EntryKind};
use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Largest `cargo metadata` document admitted.
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
/// Largest `Cargo.lock` admitted.
/// How long Cargo may take to answer.
const CARGO_DEADLINE: Duration = Duration::from_secs(90);
const MAX_CARGO_OBSERVATION_PATHS: usize = 21_024;
const MAX_CARGO_OBSERVATION_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CARGO_OBSERVATION_TOTAL_BYTES: usize = 256 * 1024 * 1024;
const MAX_CARGO_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_CARGO_CONFIG_INPUTS: usize = 256;
const MAX_CARGO_CONFIG_DEPTH: usize = 16;
const MAX_SOURCE_DIRECTORY_ENTRIES: usize = 2_048;
const MAX_SOURCE_DIRECTORY_DEPTH: usize = 32;

/// The last tree input read from Cargo, and the file bytes it was read from.
#[derive(Default)]
pub(super) struct BrowseCache {
    entry: Option<CacheEntry>,
}

struct CacheEntry {
    /// The project directory Cargo resolves from (see [`workspace_root`]).
    workspace: PathBuf,
    witness: [u8; 32],
    watched: Vec<PathBuf>,
    input: TreeInput,
}

impl BrowseCache {
    /// Reads the tree of the Cargo project containing `root`.
    pub(super) fn project_tree(
        &mut self,
        root: &Path,
        authority: Option<&backend_engine::advisory::AdvisoryAuthority>,
    ) -> Result<ProjectTree, String> {
        if !root.is_absolute() {
            return Err("project-tree needs an absolute project directory".to_owned());
        }
        let input = self.input(root)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let empty = backend_engine::advisory::AdvisoryAuthority::new(0);
        let authority = authority.unwrap_or(&empty);
        let observe = |name: &str, version: &str| {
            let package = backend_engine::advisory::normalize_package("cargo", name)
                .unwrap_or_else(|_| backend_engine::advisory::PackageIdentity {
                    ecosystem: "cargo".to_owned(),
                    name: name.to_owned(),
                    canonical_purl: None,
                });
            authority.observe(&package, version, false, false, now, false)
        };
        Ok(build_tree(&input, &observe))
    }

    /// Reads one source-only text file from the currently revalidated Cargo
    /// package observation. Neither the route nor the request supplies a root
    /// path; the owner recovers it from the admitted metadata row.
    pub(super) fn source_file(
        &mut self,
        package: PackageReference,
        path: CargoPackageSourcePathV1,
    ) -> CargoPackageSourceFileResultV1 {
        if !path.has_admissible_shape()
            || CargoPackageSourceAuthorityV1::digest_from_package_reference(&package).is_none()
        {
            return unavailable_source_file(
                Some(package),
                CargoPackageSourceReadFailureV1::InvalidPackageReference,
            );
        }
        if !supported_source_path(path.as_str()) {
            return unavailable_source_file(
                Some(package),
                CargoPackageSourceReadFailureV1::UnsupportedFileKind,
            );
        }
        let Some(workspace) = self.entry.as_ref().map(|entry| entry.workspace.clone()) else {
            return unavailable_source_file(
                Some(package),
                CargoPackageSourceReadFailureV1::AuthorityUnavailable,
            );
        };
        let input = match self.input(&workspace) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_source_file(
                    Some(package),
                    CargoPackageSourceReadFailureV1::SourceObservationUnavailable,
                );
            }
        };
        let Some(package_row) = input.packages.iter().find(|row| {
            matches!(
                &row.source_authority,
                CargoPackageSourceAuthorityStateV1::Admitted(authority)
                    if authority.matches_package_reference(&package)
            )
        }) else {
            return CargoPackageSourceFileResultV1::Stale { package };
        };
        let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &package_row.source_authority
        else {
            unreachable!("the predicate admitted only source authority rows")
        };
        let Some(package_root) = package_row.source_root.as_deref() else {
            return unavailable_source_file(
                Some(package),
                CargoPackageSourceReadFailureV1::PackageRootUnavailable,
            );
        };
        if !authority.has_admissible_shape()
            || !authority.matches_package_root_path(package_root)
            || !authority.matches_workspace_root_path(Path::new(&input.root))
        {
            return unavailable_source_file(
                Some(package),
                CargoPackageSourceReadFailureV1::AuthorityUnavailable,
            );
        }
        let authority = authority.clone();
        let contents = match read_source_file_under(package_root, &path) {
            Ok(contents) => contents,
            Err(reason) => return unavailable_source_file(Some(package), reason),
        };
        let contents = match String::from_utf8(contents) {
            Ok(contents) if !contents.as_bytes().contains(&0) => contents,
            _ => {
                return unavailable_source_file(
                    Some(package),
                    CargoPackageSourceReadFailureV1::NotUtf8Text,
                );
            }
        };

        // Recheck the complete metadata input set after opening and reading.
        // A changed manifest/config/source authority invalidates these bytes;
        // the next request must use a fresh tree receipt.
        let current = match self.input(&workspace) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_source_file(
                    Some(package),
                    CargoPackageSourceReadFailureV1::SourceObservationUnavailable,
                );
            }
        };
        let still_current = current.packages.iter().any(|row| {
            matches!(
                &row.source_authority,
                CargoPackageSourceAuthorityStateV1::Admitted(current)
                    if current.authority_digest() == authority.authority_digest()
            )
        });
        if !still_current {
            return CargoPackageSourceFileResultV1::Stale { package };
        }
        let contents = contents.into_boxed_str();
        let content_digest = *blake3::hash(contents.as_bytes()).as_bytes();
        CargoPackageSourceFileResultV1::Read {
            package,
            authority,
            path,
            content_digest,
            contents,
            semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
        }
    }

    /// Lists a bounded set of path addresses from the currently revalidated
    /// Cargo package observation. Each path remains a hint and must be read
    /// separately through [`Self::source_file`].
    pub(super) fn source_inventory(
        &mut self,
        package: PackageReference,
    ) -> CargoPackageSourceInventoryResultV1 {
        if CargoPackageSourceAuthorityV1::digest_from_package_reference(&package).is_none() {
            return unavailable_source_inventory(
                Some(package),
                CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
            );
        }
        let Some(workspace) = self.entry.as_ref().map(|entry| entry.workspace.clone()) else {
            return unavailable_source_inventory(
                Some(package),
                CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
            );
        };
        let input = match self.input(&workspace) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_source_inventory(
                    Some(package),
                    CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
                );
            }
        };
        let Some(package_row) = input.packages.iter().find(|row| {
            matches!(
                &row.source_authority,
                CargoPackageSourceAuthorityStateV1::Admitted(authority)
                    if authority.matches_package_reference(&package)
            )
        }) else {
            return CargoPackageSourceInventoryResultV1::Stale { package };
        };
        let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &package_row.source_authority
        else {
            unreachable!("the predicate admitted only source authority rows")
        };
        let Some(package_root) = package_row.source_root.as_deref() else {
            return unavailable_source_inventory(
                Some(package),
                CargoPackageSourceInventoryFailureV1::PackageRootUnavailable,
            );
        };
        if !authority.has_admissible_shape()
            || !authority.matches_package_root_path(package_root)
            || !authority.matches_workspace_root_path(Path::new(&input.root))
        {
            return unavailable_source_inventory(
                Some(package),
                CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
            );
        }
        let authority = authority.clone();
        let first = match source_inventory_under(package_root) {
            Ok(inventory) => inventory,
            Err(reason) => {
                return unavailable_source_inventory(Some(package), reason);
            }
        };
        // Directory names can change independently of Cargo metadata. Require
        // two identical no-follow enumerations before returning address hints.
        let second = match source_inventory_under(package_root) {
            Ok(inventory) => inventory,
            Err(reason) => {
                return unavailable_source_inventory(Some(package), reason);
            }
        };
        if first != second {
            return CargoPackageSourceInventoryResultV1::Stale { package };
        }

        // Recheck the Cargo metadata, config, and tool-selection witness after
        // walking. A route is useful only while its exact source authority is
        // still present in the owner's current observation.
        let current = match self.input(&workspace) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_source_inventory(
                    Some(package),
                    CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
                );
            }
        };
        let still_current = current.packages.iter().any(|row| {
            matches!(
                &row.source_authority,
                CargoPackageSourceAuthorityStateV1::Admitted(current)
                    if current.authority_digest() == authority.authority_digest()
            )
        });
        if !still_current {
            return CargoPackageSourceInventoryResultV1::Stale { package };
        }
        CargoPackageSourceInventoryResultV1::Listed(CargoPackageSourceInventoryV1 {
            package,
            authority,
            paths: first.paths.into_boxed_slice(),
            coverage: first.coverage,
        })
    }

    fn input(&mut self, root: &Path) -> Result<TreeInput, String> {
        let workspace = workspace_root(root)?
            .ok_or_else(|| {
                format!(
                    "{} is not inside a Cargo project (no Cargo.toml above it)",
                    root.display()
                )
            })?
            .canonicalize()
            .map_err(|error| {
                format!("cannot resolve Cargo workspace root without following links: {error}")
            })?;
        // The same project, and none of the files it was read from moved.
        // (A nested workspace is another project: compare the resolved
        // directory, never a path prefix.)
        if let Some(entry) = &self.entry
            && entry.workspace == workspace
            && observation_witness(&workspace, &entry.watched)
                .is_ok_and(|observed| observed.digest == entry.witness)
        {
            return Ok(entry.input.clone());
        }
        let (input, watched, witness) = read_project(&workspace)?;
        self.entry = Some(CacheEntry {
            workspace,
            witness,
            watched,
            input: input.clone(),
        });
        Ok(input)
    }
}

fn unavailable_source_file(
    package: Option<PackageReference>,
    reason: CargoPackageSourceReadFailureV1,
) -> CargoPackageSourceFileResultV1 {
    CargoPackageSourceFileResultV1::Unavailable { package, reason }
}

fn unavailable_source_inventory(
    package: Option<PackageReference>,
    reason: CargoPackageSourceInventoryFailureV1,
) -> CargoPackageSourceInventoryResultV1 {
    CargoPackageSourceInventoryResultV1::Unavailable { package, reason }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceInventoryScan {
    paths: Vec<CargoPackageSourcePathV1>,
    coverage: CargoPackageSourceInventoryCoverageV1,
}

fn source_inventory_under(
    package_root: &Path,
) -> Result<SourceInventoryScan, CargoPackageSourceInventoryFailureV1> {
    let root = DirectoryCapability::open_read_only_source(package_root)
        .map_err(|_| CargoPackageSourceInventoryFailureV1::DirectoryUnavailable)?;
    let mut scan = SourceInventoryScan {
        paths: Vec::new(),
        coverage: CargoPackageSourceInventoryCoverageV1::Complete,
    };
    let mut visited = 0_usize;
    walk_source_inventory(&root, "", 0, &mut visited, &mut scan);
    scan.paths.sort();
    scan.paths.dedup();
    Ok(scan)
}

fn walk_source_inventory(
    directory: &DirectoryCapability,
    relative_directory: &str,
    depth: usize,
    visited: &mut usize,
    scan: &mut SourceInventoryScan,
) {
    if !matches!(
        scan.coverage,
        CargoPackageSourceInventoryCoverageV1::Complete
    ) {
        return;
    }
    if depth >= MAX_SOURCE_DIRECTORY_DEPTH {
        scan.coverage = CargoPackageSourceInventoryCoverageV1::Partial {
            reason: CargoPackageSourceInventoryGapV1::DepthLimit,
        };
        return;
    }
    let entries = match directory.entries(MAX_SOURCE_DIRECTORY_ENTRIES) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::FileTooLarge => {
            scan.coverage = CargoPackageSourceInventoryCoverageV1::Partial {
                reason: CargoPackageSourceInventoryGapV1::DirectoryEntryLimit,
            };
            return;
        }
        Err(_) => {
            scan.coverage = CargoPackageSourceInventoryCoverageV1::Partial {
                reason: CargoPackageSourceInventoryGapV1::DirectoryUnavailable,
            };
            return;
        }
    };
    for entry in entries {
        if *visited >= MAX_CARGO_PACKAGE_SOURCE_INVENTORY_SCAN_ENTRIES {
            scan.coverage = CargoPackageSourceInventoryCoverageV1::Partial {
                reason: CargoPackageSourceInventoryGapV1::ScanEntryLimit,
            };
            return;
        }
        *visited += 1;
        let Some(name) = entry.name.to_str() else {
            scan.coverage = CargoPackageSourceInventoryCoverageV1::Partial {
                reason: CargoPackageSourceInventoryGapV1::UnaddressablePath,
            };
            return;
        };
        if is_internal_source_segment(name) {
            continue;
        }
        let path = if relative_directory.is_empty() {
            name.to_owned()
        } else {
            format!("{relative_directory}/{name}")
        };
        match entry.kind {
            EntryKind::Link | EntryKind::Special => continue,
            EntryKind::Directory => {
                if path.len() > backend_library::MAX_CARGO_PACKAGE_SOURCE_PATH_BYTES {
                    scan.coverage = CargoPackageSourceInventoryCoverageV1::Partial {
                        reason: CargoPackageSourceInventoryGapV1::UnaddressablePath,
                    };
                    return;
                }
                let child = match directory.open_dir(name) {
                    Ok(child) => child,
                    Err(_) => {
                        scan.coverage = CargoPackageSourceInventoryCoverageV1::Partial {
                            reason: CargoPackageSourceInventoryGapV1::DirectoryUnavailable,
                        };
                        return;
                    }
                };
                walk_source_inventory(&child, &path, depth.saturating_add(1), visited, scan);
                if !matches!(
                    scan.coverage,
                    CargoPackageSourceInventoryCoverageV1::Complete
                ) {
                    return;
                }
            }
            EntryKind::File => {
                if !supported_source_path(&path) {
                    continue;
                }
                let path = match CargoPackageSourcePathV1::new(path) {
                    Ok(path) => path,
                    Err(_) => {
                        scan.coverage = CargoPackageSourceInventoryCoverageV1::Partial {
                            reason: CargoPackageSourceInventoryGapV1::UnaddressablePath,
                        };
                        return;
                    }
                };
                if scan.paths.len() == MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS {
                    scan.coverage = CargoPackageSourceInventoryCoverageV1::Truncated {
                        limit: u16::try_from(MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS)
                            .unwrap_or(u16::MAX),
                    };
                    return;
                }
                scan.paths.push(path);
            }
        }
    }
}

fn is_internal_source_segment(segment: &str) -> bool {
    [".git", ".hg", ".svn", ".cargo", "target", "build"]
        .iter()
        .any(|internal| segment.eq_ignore_ascii_case(internal))
}

fn supported_source_path(path: &str) -> bool {
    if path.split('/').any(is_internal_source_segment) {
        return false;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    if name == "Cargo.toml" || name.eq_ignore_ascii_case("README") {
        return true;
    }
    Path::new(name)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "rs" | "md"
                    | "markdown"
                    | "rst"
                    | "txt"
                    | "c"
                    | "h"
                    | "cc"
                    | "cpp"
                    | "cxx"
                    | "hpp"
                    | "hh"
                    | "cs"
                    | "go"
                    | "java"
                    | "kt"
                    | "kts"
                    | "py"
                    | "pyi"
                    | "js"
                    | "jsx"
                    | "mjs"
                    | "cjs"
                    | "ts"
                    | "tsx"
                    | "mts"
                    | "cts"
                    | "swift"
                    | "scala"
                    | "sh"
                    | "toml"
                    | "yaml"
                    | "yml"
            )
        })
}

fn read_source_file_under(
    package_root: &Path,
    path: &CargoPackageSourcePathV1,
) -> Result<Vec<u8>, CargoPackageSourceReadFailureV1> {
    let mut segments = path.as_str().split('/').peekable();
    let mut directory = DirectoryCapability::open_read_only_source(package_root)
        .map_err(|_| CargoPackageSourceReadFailureV1::FileUnavailable)?;
    while let Some(segment) = segments.next() {
        if segments.peek().is_some() {
            directory = directory
                .open_dir(segment)
                .map_err(|_| CargoPackageSourceReadFailureV1::FileUnavailable)?;
        } else {
            let mut file = directory
                .open_file_read(segment)
                .map_err(|_| CargoPackageSourceReadFailureV1::FileUnavailable)?;
            let metadata = file
                .metadata()
                .map_err(|_| CargoPackageSourceReadFailureV1::FileUnavailable)?;
            if metadata.len() > backend_library::MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES as u64 {
                return Err(CargoPackageSourceReadFailureV1::FileTooLarge);
            }
            let mut bytes = Vec::new();
            file.by_ref()
                .take(backend_library::MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| CargoPackageSourceReadFailureV1::FileUnavailable)?;
            if bytes.len() > backend_library::MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES {
                return Err(CargoPackageSourceReadFailureV1::FileTooLarge);
            }
            return Ok(bytes);
        }
    }
    Err(CargoPackageSourceReadFailureV1::InvalidRelativePath)
}

struct InputObservation {
    digest: [u8; 32],
    /// Path commitments for candidates absent at the sampling instant.
    /// Required metadata-listed manifests are checked against this set;
    /// optional configs remain valid absences in the witness.
    missing_path_keys: BTreeSet<[u8; 32]>,
    lockfile: Option<String>,
    manifest: Option<Vec<u8>>,
}

struct CoherentMetadata {
    metadata: Vec<u8>,
    host: String,
    input_witness: [u8; 32],
    lockfile: Option<String>,
    watched: Vec<PathBuf>,
}

/// Hashes a bounded, exact Cargo input set through no-follow capabilities.
/// A missing candidate is part of the witness, so creating a previously
/// absent lock/config file invalidates the cache too.
fn observation_witness(workspace: &Path, files: &[PathBuf]) -> Result<InputObservation, String> {
    let environment = cargo_environment_witness()?;
    let tool = current_cargo_tool_witness().unwrap_or_else(unavailable_tool_witness);
    observation_witness_with_context(workspace, files, environment, tool)
}

fn strict_observation_witness(
    workspace: &Path,
    files: &[PathBuf],
) -> Result<InputObservation, String> {
    let environment = cargo_environment_witness()?;
    let tool = current_cargo_tool_witness()?;
    observation_witness_with_context(workspace, files, environment, tool)
}

fn unavailable_tool_witness(error: String) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-tool-unavailable.v1\0");
    hasher.update(error.as_bytes());
    *hasher.finalize().as_bytes()
}

fn observation_witness_with_context(
    workspace: &Path,
    files: &[PathBuf],
    environment: [u8; 32],
    tool: [u8; 32],
) -> Result<InputObservation, String> {
    if files.len() > MAX_CARGO_OBSERVATION_PATHS {
        return Err("Cargo source observation has too many input paths".to_owned());
    }
    let mut ordered = files.to_vec();
    ordered.sort();
    ordered.dedup();
    if ordered.len() > MAX_CARGO_OBSERVATION_PATHS {
        return Err("Cargo source observation has too many unique input paths".to_owned());
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-source-input-witness.v1\0");
    let mut total = 0_usize;
    let mut missing_path_keys = BTreeSet::new();
    let mut lockfile = None;
    let mut manifest = None;
    for file in ordered {
        if !file.is_absolute() {
            return Err("Cargo source observation contains a non-absolute path".to_owned());
        }
        let encoded = file.as_os_str().as_encoded_bytes();
        hasher.update(&(encoded.len() as u64).to_le_bytes());
        hasher.update(encoded);
        match read_observation_file(&file, MAX_CARGO_OBSERVATION_FILE_BYTES)? {
            Some(bytes) => {
                total = total.saturating_add(bytes.len());
                if total > MAX_CARGO_OBSERVATION_TOTAL_BYTES {
                    return Err("Cargo source observation exceeds its byte budget".to_owned());
                }
                hasher.update(&[1]);
                hasher.update(&(bytes.len() as u64).to_le_bytes());
                hasher.update(&bytes);
                if file == workspace.join("Cargo.lock") {
                    lockfile = Some(String::from_utf8(bytes).map_err(|_| {
                        "Cargo.lock is not valid UTF-8 during source observation".to_owned()
                    })?);
                } else if file == workspace.join("Cargo.toml") {
                    manifest = Some(bytes);
                }
            }
            None => {
                hasher.update(&[0]);
                missing_path_keys.insert(observation_path_key(&file));
            }
        }
    }
    hasher.update(&environment);
    hasher.update(&tool);
    Ok(InputObservation {
        digest: *hasher.finalize().as_bytes(),
        missing_path_keys,
        lockfile,
        manifest,
    })
}

/// Reads one absolute regular file without following any ancestor or final
/// symlink. Missing files are recorded as absent; other failures make the
/// observation unavailable.
fn read_observation_file(path: &Path, maximum: usize) -> Result<Option<Vec<u8>>, String> {
    let Some(parent) = path.parent() else {
        return Err("Cargo observation path has no parent".to_owned());
    };
    let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) else {
        return Err("Cargo observation path is not UTF-8".to_owned());
    };
    let directory = match DirectoryCapability::open_read_only_source(parent) {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot hold Cargo input directory: {error}")),
    };
    let mut file = match directory.open_file_read(name) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "cannot open Cargo input without following links: {error}"
            ));
        }
    };
    let metadata = file
        .metadata()
        .map_err(|error| format!("cannot inspect Cargo input: {error}"))?;
    if !metadata.is_file() {
        return Err("Cargo observation input is not a regular file".to_owned());
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read Cargo input: {error}"))?;
    if bytes.len() > maximum {
        return Err("Cargo observation file exceeds its byte limit".to_owned());
    }
    let after = file
        .metadata()
        .map_err(|error| format!("cannot recheck Cargo input: {error}"))?;
    if metadata.len() != after.len() || metadata.len() != bytes.len() as u64 {
        return Err("Cargo input changed while it was observed".to_owned());
    }
    Ok(Some(bytes))
}

fn cargo_environment_witness() -> Result<[u8; 32], String> {
    let mut entries = Vec::new();
    let mut total = 0_usize;
    for (key, value) in std::env::vars_os() {
        let named = key.to_string_lossy();
        if !(named.starts_with("CARGO_")
            || named.starts_with("NUDOX_")
            || matches!(
                named.as_ref(),
                "PATH"
                    | "RUSTC"
                    | "RUSTC_WRAPPER"
                    | "RUSTC_WORKSPACE_WRAPPER"
                    | "RUSTFLAGS"
                    | "RUSTUP_TOOLCHAIN"
            ))
        {
            continue;
        }
        total = total
            .saturating_add(key.as_encoded_bytes().len())
            .saturating_add(value.as_encoded_bytes().len());
        if entries.len() >= 256 || total > 64 * 1024 {
            return Err("Cargo environment witness exceeds its count or byte limit".to_owned());
        }
        entries.push((key, value));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-source-environment.v1\0");
    for (key, value) in entries {
        let key = key.as_encoded_bytes();
        let value = value.as_encoded_bytes();
        hasher.update(&(key.len() as u64).to_le_bytes());
        hasher.update(key);
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn current_cargo_tool_witness() -> Result<[u8; 32], String> {
    let cargo = cargo_program().ok_or_else(|| "cargo was not found".to_owned())?;
    let version = run(&cargo, Path::new("/"), &["-vV"], 64 * 1024)?;
    metadata_tool_witness(&cargo, &version)
}

fn metadata_tool_witness(cargo: &Path, version: &[u8]) -> Result<[u8; 32], String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-source-tools.v1\0");
    for path in [Some(cargo.to_path_buf()), rustc_program(cargo)]
        .into_iter()
        .flatten()
    {
        let resolved = path
            .canonicalize()
            .map_err(|error| format!("cannot resolve Cargo metadata tool: {error}"))?;
        let metadata = std::fs::metadata(&resolved)
            .map_err(|error| format!("cannot inspect Cargo metadata tool: {error}"))?;
        hasher.update(resolved.as_os_str().as_encoded_bytes());
        hasher.update(&metadata.len().to_le_bytes());
        if let Ok(modified) = metadata.modified()
            && let Ok(duration) = modified.duration_since(std::time::UNIX_EPOCH)
        {
            hasher.update(&duration.as_secs().to_le_bytes());
            hasher.update(&duration.subsec_nanos().to_le_bytes());
        }
    }
    hasher.update(&(version.len() as u64).to_le_bytes());
    hasher.update(version);
    Ok(*hasher.finalize().as_bytes())
}

fn rustc_program(cargo: &Path) -> Option<PathBuf> {
    if let Some(rustc) = std::env::var_os("RUSTC") {
        return Some(PathBuf::from(rustc));
    }
    if let Some(sibling) = cargo.parent().map(|bin| bin.join("rustc"))
        && sibling.is_file()
    {
        return Some(sibling);
    }
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| directory.join("rustc"))
        .find(|path| path.is_file())
}

/// The tree input and its observed paths. The first metadata call discovers
/// the exact package-manifest set. A second call is bracketed by a bounded
/// no-follow read-set witness, and its output must equal the discovery pass.
fn read_project(workspace: &Path) -> Result<(TreeInput, Vec<PathBuf>, [u8; 32]), String> {
    let workspace = workspace.to_path_buf();
    match coherent_metadata(&workspace, cargo_metadata) {
        Ok(observed) => {
            let input = metadata_input_with_stable_source_witness(
                &observed.metadata,
                &observed.host,
                observed.lockfile.as_deref(),
                observed.input_witness,
            )
            .map_err(|error| error.to_string())?;
            Ok((input, observed.watched, observed.input_witness))
        }
        Err(reason) => {
            let mut watched = basic_input_paths(&workspace)?;
            watched.extend(cargo_config_paths(&workspace)?);
            watched.sort();
            watched.dedup();
            let observed = observation_witness(&workspace, &watched)?;
            let lockfile = observed.lockfile.ok_or_else(|| {
                format!("{reason}; and {} has no Cargo.lock", workspace.display())
            })?;
            let manifest = observed
                .manifest
                .ok_or_else(|| "Cargo.toml disappeared during lockfile fallback".to_owned())?;
            let patched = patched_names_bytes(&manifest);
            let root = workspace
                .to_str()
                .ok_or_else(|| "workspace root path is not UTF-8".to_owned())?;
            let input = lockfile_input(&lockfile, root, &patched, &reason)
                .map_err(|error| error.to_string())?;
            Ok((input, watched, observed.digest))
        }
    }
}

fn coherent_metadata(
    workspace: &Path,
    run_metadata: impl FnMut(&Path) -> Result<(Vec<u8>, String, [u8; 32]), String>,
) -> Result<CoherentMetadata, String> {
    coherent_metadata_with(workspace, run_metadata, strict_observation_witness)
}

fn coherent_metadata_with(
    workspace: &Path,
    mut run_metadata: impl FnMut(&Path) -> Result<(Vec<u8>, String, [u8; 32]), String>,
    mut observe: impl FnMut(&Path, &[PathBuf]) -> Result<InputObservation, String>,
) -> Result<CoherentMetadata, String> {
    let (discovery_metadata, discovery_host, discovery_tool) = run_metadata(workspace)?;
    let discovery_paths = metadata_observation_paths(workspace, &discovery_metadata)?;
    let required_manifests = metadata_required_manifests(&discovery_metadata)?;
    let before = observe(workspace, &discovery_paths)?;
    require_required_manifests_present(&before, &required_manifests)?;

    let (metadata, host, tool_witness) = run_metadata(workspace)?;
    if metadata != discovery_metadata || host != discovery_host || tool_witness != discovery_tool {
        return Err("Cargo metadata inputs or tool changed between observation passes".to_owned());
    }
    let watched = metadata_observation_paths(workspace, &metadata)?;
    if watched != discovery_paths {
        return Err("Cargo metadata input set changed during observation".to_owned());
    }
    let after = observe(workspace, &watched)?;
    require_required_manifests_present(&after, &required_manifests)?;
    if before.digest != after.digest {
        return Err(
            "Cargo manifests, lockfile, configuration, or tools changed during metadata".to_owned(),
        );
    }
    Ok(CoherentMetadata {
        metadata,
        host,
        input_witness: after.digest,
        lockfile: after.lockfile,
        watched,
    })
}

fn metadata_observation_paths(workspace: &Path, metadata: &[u8]) -> Result<Vec<PathBuf>, String> {
    if metadata.len() > MAX_METADATA_BYTES {
        return Err("Cargo metadata exceeded its bounded response size".to_owned());
    }
    let value: serde_json::Value = serde_json::from_slice(metadata)
        .map_err(|error| format!("Cargo metadata JSON is malformed: {error}"))?;
    let packages = value
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Cargo metadata has no package array".to_owned())?;
    if packages.len() > 20_000 {
        return Err("Cargo metadata package set exceeds the observation limit".to_owned());
    }
    let mut paths = basic_input_paths(workspace)?;
    let mut manifest_path_bytes = 0_usize;
    for package in packages {
        let manifest = package
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Cargo metadata package omitted manifest_path".to_owned())?;
        if manifest.len() > 4 * 1024 {
            return Err("Cargo metadata manifest path exceeds the observation limit".to_owned());
        }
        manifest_path_bytes = manifest_path_bytes.saturating_add(manifest.len());
        if manifest_path_bytes > 16 * 1024 * 1024 {
            return Err("Cargo metadata manifest paths exceed their byte budget".to_owned());
        }
        let path = PathBuf::from(manifest);
        if !path.is_absolute()
            || manifest.contains("//")
            || path.file_name().and_then(std::ffi::OsStr::to_str) != Some("Cargo.toml")
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err("Cargo metadata returned a noncanonical manifest path".to_owned());
        }
        paths.push(path);
    }
    paths.extend(cargo_config_paths(workspace)?);
    paths.sort();
    paths.dedup();
    if paths.len() > MAX_CARGO_OBSERVATION_PATHS {
        return Err("Cargo metadata input set exceeds the observation limit".to_owned());
    }
    Ok(paths)
}

fn metadata_required_manifests(metadata: &[u8]) -> Result<Vec<PathBuf>, String> {
    let value: serde_json::Value = serde_json::from_slice(metadata)
        .map_err(|error| format!("Cargo metadata JSON is malformed: {error}"))?;
    let packages = value
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Cargo metadata has no package array".to_owned())?;
    if packages.len() > 20_000 {
        return Err("Cargo metadata package set exceeds the observation limit".to_owned());
    }
    let mut required = BTreeSet::new();
    let mut total_path_bytes = 0_usize;
    for package in packages {
        let manifest = package
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Cargo metadata package omitted manifest_path".to_owned())?;
        if manifest.len() > 4 * 1024 {
            return Err("Cargo metadata manifest path exceeds the observation limit".to_owned());
        }
        total_path_bytes = total_path_bytes.saturating_add(manifest.len());
        if total_path_bytes > 16 * 1024 * 1024 {
            return Err("Cargo metadata manifest paths exceed their byte budget".to_owned());
        }
        required.insert(PathBuf::from(manifest));
    }
    Ok(required.into_iter().collect())
}

fn require_required_manifests_present(
    observation: &InputObservation,
    required_manifests: &[PathBuf],
) -> Result<(), String> {
    if required_manifests.iter().any(|path| {
        observation
            .missing_path_keys
            .contains(&observation_path_key(path))
    }) {
        return Err(
            "a Cargo metadata-listed package manifest was absent during observation".to_owned(),
        );
    }
    Ok(())
}

fn observation_path_key(path: &Path) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-observation-path.v1\0");
    hasher.update(path.as_os_str().as_encoded_bytes());
    *hasher.finalize().as_bytes()
}

fn basic_input_paths(workspace: &Path) -> Result<Vec<PathBuf>, String> {
    let workspace = workspace
        .canonicalize()
        .map_err(|error| format!("cannot resolve workspace root: {error}"))?;
    let mut paths = vec![workspace.join("Cargo.lock"), workspace.join("Cargo.toml")];
    Ok(paths)
}

/// Captures inherited Cargo config candidates, recursive `include` inputs,
/// and configured custom target JSONs. Unresolvable includes/target files
/// fail closed instead of leaving an unobserved input outside the witness.
fn cargo_config_paths(workspace: &Path) -> Result<Vec<PathBuf>, String> {
    let mut pending = Vec::<(PathBuf, usize)>::new();
    let ancestors = workspace.ancestors().take(128).collect::<Vec<_>>();
    if ancestors.len() == 128 && ancestors.last().is_some_and(|path| path.parent().is_some()) {
        return Err("Cargo config ancestor chain exceeds its limit".to_owned());
    }
    for ancestor in ancestors {
        pending.push((ancestor.join(".cargo/config"), 0));
        pending.push((ancestor.join(".cargo/config.toml"), 0));
    }
    let cargo_home = match std::env::var_os("CARGO_HOME") {
        Some(home) => {
            let home = PathBuf::from(home);
            if !home.is_absolute() {
                return Err("relative CARGO_HOME cannot be safely observed".to_owned());
            }
            home
        }
        None => std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| {
                "Cargo home cannot be resolved for configuration observation".to_owned()
            })?
            .join(".cargo"),
    };
    pending.push((cargo_home.join("config"), 0));
    pending.push((cargo_home.join("config.toml"), 0));

    let mut paths = BTreeSet::new();
    while let Some((path, depth)) = pending.pop() {
        if !paths.insert(path.clone()) {
            continue;
        }
        if paths.len() > MAX_CARGO_CONFIG_INPUTS {
            return Err("Cargo configuration input set exceeds its limit".to_owned());
        }
        let Some(bytes) = read_observation_file(&path, MAX_CARGO_CONFIG_BYTES)? else {
            continue;
        };
        let document: toml::Value = std::str::from_utf8(&bytes)
            .map_err(|_| "Cargo config is not UTF-8".to_owned())?
            .parse()
            .map_err(|error: toml::de::Error| format!("Cargo config is malformed: {error}"))?;
        if let Some(include) = document.get("include") {
            if depth >= MAX_CARGO_CONFIG_DEPTH {
                return Err("Cargo config include depth exceeds its limit".to_owned());
            }
            let values = if let Some(value) = include.as_str() {
                vec![value]
            } else if let Some(values) = include.as_array() {
                if values.len() > 64 {
                    return Err("Cargo config has too many included files".to_owned());
                }
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .ok_or_else(|| "Cargo config include has a non-string path".to_owned())
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                return Err("Cargo config include has an unsupported shape".to_owned());
            };
            if values.len() > 64 {
                return Err("Cargo config has too many included files".to_owned());
            }
            let parent = path
                .parent()
                .ok_or_else(|| "Cargo config has no parent".to_owned())?;
            for value in values {
                if value.len() > 4 * 1024 || value.contains('\0') {
                    return Err("Cargo config include path is out of bounds".to_owned());
                }
                let included = PathBuf::from(value);
                if pending.len() >= MAX_CARGO_CONFIG_INPUTS {
                    return Err("Cargo config include queue exceeds its limit".to_owned());
                }
                pending.push((
                    if included.is_absolute() {
                        included
                    } else {
                        parent.join(included)
                    },
                    depth + 1,
                ));
            }
        }
        if let Some(target) = document
            .get("build")
            .and_then(toml::Value::as_table)
            .and_then(|build| build.get("target"))
        {
            let targets = if let Some(target) = target.as_str() {
                vec![target]
            } else if let Some(targets) = target.as_array() {
                if targets.is_empty() || targets.len() > 16 {
                    return Err("Cargo build.target array is empty or exceeds its limit".to_owned());
                }
                targets
                    .iter()
                    .map(|target| {
                        target.as_str().ok_or_else(|| {
                            "Cargo build.target array contains a non-string entry".to_owned()
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?
            } else {
                return Err("Cargo build.target has an unsupported shape".to_owned());
            };
            for target in targets {
                observe_custom_target_path(
                    workspace,
                    path.parent().unwrap_or(workspace),
                    target,
                    &mut paths,
                )?;
            }
        }
    }
    if let Some(target) = std::env::var_os("CARGO_BUILD_TARGET") {
        let target = target
            .to_str()
            .ok_or_else(|| "CARGO_BUILD_TARGET is not UTF-8".to_owned())?;
        observe_custom_target_path(workspace, workspace, target, &mut paths)?;
    }
    Ok(paths.into_iter().collect())
}

fn observe_custom_target_path(
    workspace: &Path,
    config_directory: &Path,
    target: &str,
    observed_paths: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    if target.len() > 4 * 1024 || target.contains('\0') {
        return Err("Cargo build.target value is out of bounds".to_owned());
    }
    if !target.ends_with(".json") {
        return Ok(());
    }
    let target = PathBuf::from(target);
    let candidates = if target.is_absolute() {
        vec![target]
    } else {
        vec![config_directory.join(&target), workspace.join(&target)]
    };
    let mut found = false;
    for candidate in &candidates {
        match read_observation_file(candidate, MAX_CARGO_OBSERVATION_FILE_BYTES) {
            Ok(Some(_)) => found = true,
            Ok(None) => {}
            Err(error) => return Err(error),
        }
    }
    if !found {
        return Err(
            "Cargo custom target JSON could not be located for source observation".to_owned(),
        );
    }
    observed_paths.extend(candidates);
    if observed_paths.len() > MAX_CARGO_CONFIG_INPUTS {
        return Err("Cargo configuration input set exceeds its limit".to_owned());
    }
    Ok(())
}

fn patched_names_bytes(manifest: &[u8]) -> BTreeSet<String> {
    std::str::from_utf8(manifest)
        .ok()
        .and_then(|text| text.parse::<toml::Value>().ok())
        .map(|document| {
            document
                .get("patch")
                .and_then(toml::Value::as_table)
                .into_iter()
                .flat_map(|registries| registries.values())
                .filter_map(toml::Value::as_table)
                .flat_map(|table| table.keys().cloned())
                .collect()
        })
        .unwrap_or_default()
}

/// The nearest directory at or above `root` whose `Cargo.toml` declares
/// `[workspace]`, else the nearest with a `Cargo.toml`: where Cargo itself
/// would resolve the project.
fn workspace_root(root: &Path) -> Result<Option<PathBuf>, String> {
    let mut nearest = None;
    for directory in root.ancestors() {
        let manifest = directory.join("Cargo.toml");
        let Some(bytes) = read_observation_file(&manifest, MAX_CARGO_CONFIG_BYTES)? else {
            continue;
        };
        if nearest.is_none() {
            nearest = Some(
                directory
                    .canonicalize()
                    .map_err(|error| format!("cannot resolve Cargo project root: {error}"))?,
            );
        }
        let is_workspace = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| text.parse::<toml::Value>().ok())
            .is_some_and(|document| document.get("workspace").is_some());
        if is_workspace {
            let canonical = directory
                .canonicalize()
                .map_err(|error| format!("cannot resolve Cargo workspace root: {error}"))?;
            return Ok(Some(canonical));
        }
    }
    Ok(nearest)
}

/// Runs `cargo metadata` for this host; returns the document, host, and exact
/// Cargo/Rust compiler tool stamp used for both resolution and cache checks.
fn cargo_metadata(workspace: &Path) -> Result<(Vec<u8>, String, [u8; 32]), String> {
    let environment_before = cargo_environment_witness()?;
    let cargo = cargo_program().ok_or_else(|| "cargo was not found".to_owned())?;
    let version = run(&cargo, workspace, &["-vV"], 64 * 1024)?;
    if cargo_environment_witness()? != environment_before {
        return Err(
            "Cargo tool-selection environment changed during metadata admission".to_owned(),
        );
    }
    let host = String::from_utf8_lossy(&version)
        .lines()
        .find_map(|line| {
            line.strip_prefix("host: ")
                .map(str::trim)
                .map(ToOwned::to_owned)
        })
        .ok_or_else(|| "cargo -vV named no host".to_owned())?;
    let tool_before = metadata_tool_witness(&cargo, &version)?;
    let metadata = run(
        &cargo,
        workspace,
        &[
            "metadata",
            "--offline",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            &host,
        ],
        MAX_METADATA_BYTES,
    )?;
    let tool_after = metadata_tool_witness(&cargo, &version)?;
    let environment_after = cargo_environment_witness()?;
    if tool_before != tool_after || environment_before != environment_after {
        return Err("Cargo tools or selection environment changed during metadata".to_owned());
    }
    let tool_witness = tool_after;
    Ok((metadata, host, tool_witness))
}

/// `NUDOX_CARGO`, else `cargo` beside `NUDOX_RUSTC`, else the first `cargo`
/// on `PATH` or in the usual install places.
fn cargo_program() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("NUDOX_CARGO") {
        return Some(PathBuf::from(explicit));
    }
    let executable = |path: PathBuf| path.is_file().then_some(path);
    if let Some(rustc) = std::env::var_os("NUDOX_RUSTC")
        && let Some(found) = Path::new(&rustc)
            .parent()
            .map(|bin| bin.join("cargo"))
            .and_then(executable)
    {
        return Some(found);
    }
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("cargo"))
                .collect()
        })
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join(".cargo/bin/cargo"));
        candidates.push(home.join(".nix-profile/bin/cargo"));
        if let Some(user) = home.file_name() {
            candidates.push(
                Path::new("/etc/profiles/per-user")
                    .join(user)
                    .join("bin/cargo"),
            );
        }
    }
    candidates.push(PathBuf::from("/opt/homebrew/bin/cargo"));
    candidates.push(PathBuf::from("/usr/local/bin/cargo"));
    candidates.push(PathBuf::from("/run/current-system/sw/bin/cargo"));
    candidates.into_iter().find_map(executable)
}

/// Runs one bounded, deadline-limited command and returns its stdout.
fn run(
    program: &Path,
    directory: &Path,
    arguments: &[&str],
    maximum: usize,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new(program);
    // Cargo runs `rustc` to learn the host's targets and cfgs, found through
    // `RUSTC` or `PATH`. A Finder launch's `PATH` holds no `rustc`, and a
    // metadata read that cannot run it fell back to the bare lockfile (every
    // pinned package, including those no build here compiles): name the
    // compiler that ships with this cargo.
    if std::env::var_os("RUSTC").is_none()
        && let Some(rustc) = program
            .parent()
            .map(|bin| bin.join("rustc"))
            .filter(|rustc| rustc.is_file())
    {
        command.env("RUSTC", rustc);
    }
    let mut child = command
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("cargo could not start: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "cargo has no stdout".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "cargo has no stderr".to_owned())?;
    let limit = u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1);
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(limit).read_to_end(&mut bytes).map(|_| bytes)
    });
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stderr.take(64 * 1024).read_to_end(&mut bytes);
        bytes
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > CARGO_DEADLINE => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "cargo {} took longer than {}s",
                    arguments[0],
                    CARGO_DEADLINE.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(format!("cargo {} failed: {error}", arguments[0])),
        }
    };
    let bytes = reader
        .join()
        .map_err(|_| "cargo's output reader panicked".to_owned())?
        .map_err(|error| format!("reading cargo's output: {error}"))?;
    let errors = errors.join().unwrap_or_default();
    if bytes.len() > maximum {
        return Err(format!(
            "cargo {} wrote more than {maximum} bytes",
            arguments[0]
        ));
    }
    if !status.success() {
        let first = String::from_utf8_lossy(&errors)
            .lines()
            .find(|line| line.starts_with("error"))
            .map_or_else(|| format!("exit status {status}"), ToOwned::to_owned);
        return Err(format!("cargo {} failed: {first}", arguments[0]));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_workspace_root_is_the_outermost_workspace_manifest() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let repository = repository.canonicalize().expect("repository");
        let found = workspace_root(&repository.join("crates/present"))
            .expect("workspace observation")
            .expect("workspace");
        assert_eq!(found, repository);
        let manifest =
            read_observation_file(&repository.join("Cargo.toml"), MAX_CARGO_CONFIG_BYTES)
                .expect("manifest read")
                .expect("manifest exists");
        let patched = patched_names_bytes(&manifest);
        assert!(patched.contains("gpui-ce"), "{patched:?}");
    }

    #[test]
    fn a_directory_outside_any_cargo_project_says_so() {
        let error = BrowseCache::default()
            .project_tree(Path::new("/"), None)
            .expect_err("no project at the root");
        assert!(error.contains("is not inside a Cargo project"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn cargo_source_file_reader_uses_nofollow_regular_files_and_hides_internal_roots() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-cargo-source-file-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        let package = scratch.0.join("package");
        std::fs::create_dir_all(package.join("src")).expect("source directory");
        std::fs::write(package.join("src/lib.rs"), "pub fn observed() {}\n").expect("source file");
        let outside = scratch.0.join("outside.rs");
        std::fs::write(&outside, "outside secret\n").expect("outside file");
        std::os::unix::fs::symlink(&outside, package.join("src/linked.rs"))
            .expect("source symlink");

        let regular_path =
            CargoPackageSourcePathV1::new("src/lib.rs").expect("canonical package relative path");
        assert_eq!(
            read_source_file_under(&package, &regular_path).expect("regular source file"),
            b"pub fn observed() {}\n"
        );
        let linked_path = CargoPackageSourcePathV1::new("src/linked.rs")
            .expect("valid but symlinked source path");
        assert_eq!(
            read_source_file_under(&package, &linked_path),
            Err(CargoPackageSourceReadFailureV1::FileUnavailable)
        );
        assert!(CargoPackageSourcePathV1::new(".git/config").is_err());
        assert!(CargoPackageSourcePathV1::new("target/generated.rs").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cargo_source_inventory_is_sorted_bounded_and_skips_internal_or_linked_paths() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-cargo-source-inventory-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        let package = scratch.0.join("package");
        std::fs::create_dir_all(package.join("src")).expect("source directory");
        std::fs::create_dir_all(package.join("target")).expect("target directory");
        std::fs::create_dir_all(package.join(".git")).expect("git directory");
        std::fs::write(package.join("Cargo.toml"), "[package]\n").expect("manifest");
        std::fs::write(package.join("README.md"), "docs\n").expect("readme");
        std::fs::write(package.join("src/lib.rs"), "pub fn local() {}\n").expect("source");
        std::fs::write(package.join("target/generated.rs"), "generated\n")
            .expect("generated source");
        std::fs::write(package.join(".git/config"), "internal\n").expect("git internals");
        let outside = scratch.0.join("outside");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("linked.rs"), "outside\n").expect("outside source");
        std::os::unix::fs::symlink(&outside, package.join("linked-dir")).expect("linked directory");
        std::os::unix::fs::symlink(outside.join("linked.rs"), package.join("linked.rs"))
            .expect("linked file");

        let scan = source_inventory_under(&package).expect("inventory scan");
        let paths = scan
            .paths
            .iter()
            .map(CargoPackageSourcePathV1::as_str)
            .collect::<Vec<_>>();
        assert_eq!(paths, ["Cargo.toml", "README.md", "src/lib.rs"]);
        assert_eq!(
            scan.coverage,
            CargoPackageSourceInventoryCoverageV1::Complete
        );
    }

    #[test]
    fn cargo_source_inventory_reports_the_fixed_path_cap() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let package = std::env::temp_dir().join(format!(
            "backend-cargo-source-inventory-cap-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let scratch = Scratch(package.clone());
        std::fs::create_dir_all(&package).expect("package");
        for number in 0..=MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS {
            std::fs::write(package.join(format!("source-{number:04}.rs")), "")
                .expect("source file");
        }
        let scan = source_inventory_under(&package).expect("inventory scan");
        assert_eq!(scan.paths.len(), MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS);
        assert_eq!(
            scan.coverage,
            CargoPackageSourceInventoryCoverageV1::Truncated {
                limit: MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS as u16,
            }
        );
    }

    #[test]
    fn an_untouched_workspace_is_cached_but_lockfile_and_manifest_changes_each_invalidate_it() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-browse-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        std::fs::create_dir_all(scratch.0.join("src")).expect("project dir");
        let manifest = scratch.0.join("Cargo.toml");
        let lockfile = scratch.0.join("Cargo.lock");
        let write_project = |version: &str| {
            std::fs::write(
                &manifest,
                format!(
                    "[package]\nname = \"gapfix\"\nversion = \"{version}\"\nedition = \"2021\"\n"
                ),
            )
            .expect("manifest");
            std::fs::write(
                &lockfile,
                format!(
                    "# This file is automatically @generated by Cargo.\n# It is not intended for manual editing.\nversion = 4\n\n[[package]]\nname = \"gapfix\"\nversion = \"{version}\"\n"
                ),
            )
            .expect("lockfile");
        };
        write_project("0.1.0");
        std::fs::write(scratch.0.join("src/lib.rs"), "").expect("lib.rs");
        let root = scratch.0.canonicalize().expect("canonical root");

        // Plant a cache entry whose witness matches the files exactly as
        // they stand right now, but whose `input` is a sentinel no real read
        // of this project would ever produce (a real read's `root` is this
        // absolute path, never the literal string below). If an untouched
        // read is served from the cache, it must come back exactly as
        // planted — proving `read_project` (and so Cargo) was never called
        // again. This is the half of the seam a mutation that always
        // recomputes (never caches) cannot pass.
        let watched = vec![root.join("Cargo.lock"), root.join("Cargo.toml")];
        let sentinel = TreeInput {
            source: TreeSource::Lockfile {
                reason: "planted by the test, never a real read".to_owned(),
                coverage: LockfileGraphCoverage::Complete,
                workspace_membership: backend_library::browse::LockfileWorkspaceMembership::Unknown,
            },
            root: "sentinel-root".to_owned(),
            packages: Vec::new(),
            edges: Vec::new(),
            locked_inactive: 0,
            locked_inactive_coverage: LockedInactiveCoverage::Unavailable,
        };
        let mut cache = BrowseCache::default();
        cache.entry = Some(CacheEntry {
            workspace: root.clone(),
            witness: witness(&watched),
            watched: watched.clone(),
            input: sentinel.clone(),
        });

        let untouched = cache.project_tree(&root, None).expect("untouched read");
        assert_eq!(
            untouched.root, "sentinel-root",
            "an untouched workspace must be served from the cache, not recomputed: {untouched:?}"
        );

        // A lockfile-only edit invalidates the cached Cargo resolution. Its
        // manifest stays byte-for-byte identical, so watching only manifests
        // cannot pass this assertion.
        let initial_lock = std::fs::read_to_string(&lockfile).expect("initial lockfile");
        std::fs::write(
            &lockfile,
            format!("{initial_lock}\n# lockfile changed alone\n"),
        )
        .expect("changed lockfile");
        let touched = cache.project_tree(&root, None).expect("lockfile-only read");
        assert_ne!(
            touched.root, "sentinel-root",
            "a changed lockfile alone must invalidate the cache and force a real read"
        );

        // Replant the sentinel against the new lockfile, then change only
        // Cargo.toml. Both inputs to Cargo's answer have independent guards.
        cache.entry = Some(CacheEntry {
            workspace: root.clone(),
            witness: witness(&watched),
            watched,
            input: sentinel,
        });
        std::fs::write(
            &manifest,
            "[package]\nname = \"gapfix\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
        )
        .expect("changed manifest");
        let touched = cache.project_tree(&root, None).expect("manifest-only read");
        assert_ne!(
            touched.root, "sentinel-root",
            "a changed manifest alone must force a real read"
        );
    }

    #[test]
    fn a_path_dependency_manifest_change_during_metadata_refuses_the_source_observation() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-cargo-observation-race-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        let workspace = scratch.0.join("workspace");
        let app = workspace.join("app");
        let dependency = scratch.0.join("path-dependency");
        std::fs::create_dir_all(app.join("src")).expect("app directory");
        std::fs::create_dir_all(dependency.join("src")).expect("dependency directory");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(workspace.join("Cargo.lock"), "version = 4\n").expect("lockfile");
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("app manifest");
        std::fs::write(
            dependency.join("Cargo.toml"),
            "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("path dependency manifest");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let app_manifest = app.join("Cargo.toml").canonicalize().expect("app path");
        let dependency_manifest = dependency
            .join("Cargo.toml")
            .canonicalize()
            .expect("dependency path");
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace,
            "workspace_members": ["app 0.1.0 (path+file:///workspace/app)"],
            "packages": [
                {
                    "id": "app 0.1.0 (path+file:///workspace/app)",
                    "name": "app",
                    "version": "0.1.0",
                    "manifest_path": app_manifest,
                },
                {
                    "id": "dep 0.1.0 (path+file:///path-dependency)",
                    "name": "dep",
                    "version": "0.1.0",
                    "manifest_path": dependency_manifest,
                }
            ]
        }))
        .expect("metadata fixture");
        let mut runs = 0;
        let result = coherent_metadata_with(
            &workspace,
            |_: &Path| {
                runs += 1;
                if runs == 2 {
                    std::fs::write(
                        &dependency_manifest,
                        "[package]\nname = \"dep\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
                    )
                    .expect("mutate path dependency during second metadata pass");
                }
                Ok((
                    metadata.clone(),
                    "x86_64-unknown-linux-gnu".to_owned(),
                    [9; 32],
                ))
            },
            |workspace, paths| observation_witness_with_context(workspace, paths, [1; 32], [2; 32]),
        );
        assert!(
            result
                .expect_err("changed path dependency must invalidate source authority")
                .contains("changed during metadata"),
            "the observation must fail closed when Cargo ran across an input replacement"
        );
        assert_eq!(
            runs, 2,
            "no retry may turn this race into a success receipt"
        );
    }

    #[test]
    fn metadata_authority_rejects_a_required_manifest_absent_across_an_absent_present_absent_aba() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-cargo-manifest-aba-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        let workspace = scratch.0.join("workspace");
        let app = workspace.join("app");
        let dependency = scratch.0.join("path-dependency");
        std::fs::create_dir_all(&app).expect("app directory");
        std::fs::create_dir_all(&dependency).expect("dependency directory");
        std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n").expect("workspace manifest");
        std::fs::write(workspace.join("Cargo.lock"), "version = 4\n").expect("lockfile");
        let app_manifest = app.join("Cargo.toml");
        let dependency_manifest = dependency.join("Cargo.toml");
        std::fs::write(&app_manifest, "[package]\nname=\"app\"\n").expect("app manifest");
        std::fs::write(&dependency_manifest, "[package]\nname=\"dep\"\n")
            .expect("dependency manifest");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let app_manifest = app_manifest.canonicalize().expect("canonical app manifest");
        let dependency_manifest = dependency_manifest
            .canonicalize()
            .expect("canonical dependency manifest");
        let metadata = serde_json::to_vec(&serde_json::json!({
            "packages": [
                {"manifest_path": app_manifest},
                {"manifest_path": dependency_manifest}
            ]
        }))
        .expect("metadata fixture");
        let required = metadata_required_manifests(&metadata).expect("required manifest set");
        let watched = vec![
            workspace.join("Cargo.lock"),
            workspace.join("Cargo.toml"),
            app_manifest.clone(),
            dependency_manifest.clone(),
        ];

        // This is the exact digest-only ABA that used to compare equal:
        // sampling sees the required path absent, Cargo could see it present,
        // and the final sample sees it absent again.
        std::fs::remove_file(&dependency_manifest).expect("begin absent interval");
        let before = observation_witness_with_context(&workspace, &watched, [1; 32], [2; 32])
            .expect("pre-pass observation");
        std::fs::write(&dependency_manifest, "[package]\nname=\"dep\"\n")
            .expect("transiently restore manifest");
        std::fs::remove_file(&dependency_manifest).expect("end absent interval");
        let after = observation_witness_with_context(&workspace, &watched, [1; 32], [2; 32])
            .expect("post-pass observation");
        assert_eq!(
            before.digest, after.digest,
            "the absent/present/absent ABA preserves the old byte witness"
        );
        assert!(require_required_manifests_present(&before, &required).is_err());
        assert!(require_required_manifests_present(&after, &required).is_err());

        // The production two-pass gate refuses before starting another Cargo
        // pass when its required manifest is already missing.
        std::fs::write(&dependency_manifest, "[package]\nname=\"dep\"\n")
            .expect("restore manifest before gate");
        let mut cargo_runs = 0;
        let mut observations = 0;
        let result = coherent_metadata_with(
            &workspace,
            |_| {
                cargo_runs += 1;
                Ok((metadata.clone(), "host".to_owned(), [3; 32]))
            },
            |workspace, paths| {
                observations += 1;
                if observations == 1 {
                    std::fs::remove_file(&dependency_manifest)
                        .expect("manifest disappears before second Cargo pass");
                }
                observation_witness_with_context(workspace, paths, [1; 32], [2; 32])
            },
        );
        assert!(
            result
                .expect_err("required absent manifest must refuse authority")
                .contains("metadata-listed package manifest was absent"),
            "the refusal must identify the required-input condition without exposing a path"
        );
        assert_eq!(
            cargo_runs, 1,
            "the second Cargo pass must not run on an incomplete input set"
        );
        assert_eq!(observations, 1);
    }

    #[test]
    fn stable_metadata_witness_covers_nonmember_path_dependency_manifests() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-cargo-observation-stable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        let workspace = scratch.0.join("workspace");
        let app = workspace.join("app");
        let dependency = scratch.0.join("path-dependency");
        std::fs::create_dir_all(&app).expect("app directory");
        std::fs::create_dir_all(&dependency).expect("dependency directory");
        std::fs::write(workspace.join("Cargo.toml"), "[workspace]\n").expect("workspace manifest");
        std::fs::write(workspace.join("Cargo.lock"), "version = 4\n").expect("lockfile");
        std::fs::write(app.join("Cargo.toml"), "[package]\nname=\"app\"\n").expect("app manifest");
        std::fs::write(dependency.join("Cargo.toml"), "[package]\nname=\"dep\"\n")
            .expect("dependency manifest");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let app_manifest = app.join("Cargo.toml").canonicalize().expect("app path");
        let dependency_manifest = dependency
            .join("Cargo.toml")
            .canonicalize()
            .expect("dependency path");
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace,
            "packages": [
                {"manifest_path": app_manifest},
                {"manifest_path": dependency_manifest}
            ]
        }))
        .expect("metadata fixture");
        let result = coherent_metadata_with(
            &workspace,
            |_| Ok((metadata.clone(), "host".to_owned(), [3; 32])),
            |workspace, paths| observation_witness_with_context(workspace, paths, [1; 32], [2; 32]),
        )
        .expect("stable input observation");
        assert_ne!(result.input_witness, [0; 32]);
        assert!(result.watched.contains(&dependency_manifest));
    }
}

#[cfg(test)]
fn witness(files: &[PathBuf]) -> [u8; 32] {
    let workspace = files
        .first()
        .and_then(|path| path.parent())
        .expect("workspace");
    observation_witness(workspace, files)
        .expect("source witness")
        .digest
}
