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
const MAX_CARGO_TOOL_BINARY_BYTES: u64 = 128 * 1024 * 1024;
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
    tool_witness_reuse: Option<CargoToolWitnessReuse>,
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
        let cached_tool = self
            .entry
            .as_ref()
            .filter(|entry| entry.workspace == workspace)
            .and_then(|entry| entry.tool_witness_reuse.clone());
        if let Some(entry) = &self.entry
            && entry.workspace == workspace
            && observation_witness(&workspace, &entry.watched, cached_tool.as_ref())
                .is_ok_and(|observed| observed.digest == entry.witness)
        {
            return Ok(entry.input.clone());
        }
        let (input, watched, witness, tool_witness_reuse) =
            read_project(&workspace, cached_tool.as_ref())?;
        self.entry = Some(CacheEntry {
            workspace,
            witness,
            watched,
            input: input.clone(),
            tool_witness_reuse,
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
    tool_witness_reuse: Option<CargoToolWitnessReuse>,
    lockfile: Option<String>,
    manifest: Option<Vec<u8>>,
}

/// Reusable byte digests for tools rooted in Nix's immutable store. Each
/// entry is usable only after the same canonical path and immutable file/tree
/// identity have been revalidated through a no-follow file handle.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CargoToolWitnessReuse {
    files: Box<[CargoToolFileReuse]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CargoToolFileReuse {
    canonical_path: PathBuf,
    immutable_identity: [u8; 32],
    content_digest: [u8; 32],
}

struct CargoToolWitness {
    digest: [u8; 32],
    reuse: CargoToolWitnessReuse,
}

struct CargoToolFileWitness {
    canonical_path: PathBuf,
    content_digest: [u8; 32],
    reuse: Option<CargoToolFileReuse>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CargoToolSelection {
    cargo: PathBuf,
    rustc: PathBuf,
    rustc_wrapper: Option<PathBuf>,
    rustc_workspace_wrapper: Option<PathBuf>,
    inject_default_rustc: bool,
}

struct CoherentMetadata {
    metadata: Vec<u8>,
    host: String,
    input_witness: [u8; 32],
    tool_witness_reuse: Option<CargoToolWitnessReuse>,
    lockfile: Option<String>,
    watched: Vec<PathBuf>,
}

/// Hashes a bounded, exact Cargo input set through no-follow capabilities.
/// A missing candidate is part of the witness, so creating a previously
/// absent lock/config file invalidates the cache too.
fn observation_witness(
    workspace: &Path,
    files: &[PathBuf],
    cached_tool: Option<&CargoToolWitnessReuse>,
) -> Result<InputObservation, String> {
    let environment = cargo_environment_witness()?;
    let (tool, reuse) = match current_cargo_tool_witness(workspace, cached_tool) {
        Ok(tool) => (tool.digest, Some(tool.reuse)),
        Err(error) => (unavailable_tool_witness(error), None),
    };
    let mut observation = observation_witness_with_context(workspace, files, environment, tool)?;
    observation.tool_witness_reuse = reuse;
    Ok(observation)
}

fn strict_observation_witness(
    workspace: &Path,
    files: &[PathBuf],
    cached_tool: Option<&CargoToolWitnessReuse>,
) -> Result<InputObservation, String> {
    let environment = cargo_environment_witness()?;
    let tool = current_cargo_tool_witness(workspace, cached_tool)?;
    let mut observation =
        observation_witness_with_context(workspace, files, environment, tool.digest)?;
    observation.tool_witness_reuse = Some(tool.reuse);
    Ok(observation)
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
        tool_witness_reuse: None,
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
            || named.starts_with("RUSTC_")
            || named.starts_with("RUSTUP_")
            || named.starts_with("SCCACHE_")
            || matches!(
                named.as_ref(),
                "PATH" | "HOME" | "XDG_CONFIG_HOME" | "RUSTC" | "RUSTFLAGS"
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

fn current_cargo_tool_witness(
    workspace: &Path,
    cached: Option<&CargoToolWitnessReuse>,
) -> Result<CargoToolWitness, String> {
    let cargo = selected_cargo_program(workspace)?;
    let version = run(&cargo, workspace, &["-vV"], 64 * 1024)?;
    let selection = cargo_tool_selection(&cargo, workspace, &version)?;
    metadata_tool_witness(workspace, &version, &selection, cached)
}

fn metadata_tool_witness(
    workspace: &Path,
    cargo_version: &[u8],
    selection: &CargoToolSelection,
    cached: Option<&CargoToolWitnessReuse>,
) -> Result<CargoToolWitness, String> {
    let cargo_executable_witness = tool_executable_witness(&selection.cargo, cached)?;

    let selected_rustc = &selection.rustc;
    let selected_rustc_version = run(&selected_rustc, workspace, &["-vV"], 64 * 1024)?;
    let selected_sysroot = run(
        &selected_rustc,
        workspace,
        &["--print", "sysroot"],
        64 * 1024,
    )?;
    let selected_sysroot = std::str::from_utf8(&selected_sysroot)
        .map_err(|_| "selected rustc returned a non-UTF-8 sysroot".to_owned())?
        .trim();
    if selected_sysroot.is_empty() || selected_sysroot.len() > 4 * 1024 {
        return Err("selected rustc returned an invalid sysroot".to_owned());
    }
    let sysroot = PathBuf::from(selected_sysroot)
        .canonicalize()
        .map_err(|_| "selected rustc sysroot cannot be resolved".to_owned())?;
    let rustc_name = format!("rustc{}", std::env::consts::EXE_SUFFIX);
    let effective_rustc = sysroot.join("bin").join(rustc_name);
    let effective_rustc = effective_rustc
        .canonicalize()
        .map_err(|_| "selected rustc sysroot has no verifiable compiler executable".to_owned())?;
    let effective_rustc_version = run(&effective_rustc, workspace, &["-vV"], 64 * 1024)?;
    if effective_rustc_version != selected_rustc_version {
        return Err("selected rustc does not match its reported sysroot compiler".to_owned());
    }
    let selected_rustc_witness = tool_executable_witness(selected_rustc, cached)?;
    let effective_rustc_witness = tool_executable_witness(&effective_rustc, cached)?;

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-source-tools.v3\0");
    hash_tool_role(
        &mut hasher,
        b"cargo",
        &selection.cargo,
        &cargo_executable_witness,
    );
    hasher.update(&(cargo_version.len() as u64).to_le_bytes());
    hasher.update(cargo_version);
    hash_tool_role(
        &mut hasher,
        b"rustc-invocation",
        selected_rustc,
        &selected_rustc_witness,
    );
    hash_tool_role(
        &mut hasher,
        b"rustc-effective",
        &effective_rustc,
        &effective_rustc_witness,
    );
    hasher.update(&(selected_rustc_version.len() as u64).to_le_bytes());
    hasher.update(&selected_rustc_version);
    hasher.update(sysroot.as_os_str().as_encoded_bytes());

    let mut reuse = vec![
        cargo_executable_witness.reuse,
        selected_rustc_witness.reuse,
        effective_rustc_witness.reuse,
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    for (role, wrapper) in [
        (
            b"rustc-wrapper".as_slice(),
            selection.rustc_wrapper.as_deref(),
        ),
        (
            b"rustc-workspace-wrapper".as_slice(),
            selection.rustc_workspace_wrapper.as_deref(),
        ),
    ] {
        let Some(wrapper) = wrapper else { continue };
        let version = verify_sccache_wrapper(wrapper, workspace)?;
        let witness = tool_executable_witness(wrapper, cached)?;
        hash_tool_role(&mut hasher, role, wrapper, &witness);
        hasher.update(&(version.len() as u64).to_le_bytes());
        hasher.update(&version);
        if let Some(file_reuse) = witness.reuse {
            reuse.push(file_reuse);
        }
    }
    reuse.sort_by(|left, right| left.canonical_path.cmp(&right.canonical_path));
    reuse.dedup_by(|left, right| left.canonical_path == right.canonical_path);
    Ok(CargoToolWitness {
        digest: *hasher.finalize().as_bytes(),
        reuse: CargoToolWitnessReuse {
            files: reuse.into_boxed_slice(),
        },
    })
}

fn effective_cargo_executable(
    cargo: &Path,
    workspace: &Path,
    expected_version: &[u8],
) -> Result<PathBuf, String> {
    let resolved = cargo
        .canonicalize()
        .map_err(|_| "Cargo executable path cannot be resolved".to_owned())?;
    let is_rustup_proxy = cargo.file_stem() == Some(std::ffi::OsStr::new("cargo"))
        && resolved.file_stem() == Some(std::ffi::OsStr::new("rustup"));
    if !is_rustup_proxy {
        return Ok(resolved);
    }
    let selected = run(&resolved, workspace, &["which", "cargo"], 16 * 1024)?;
    let selected = std::str::from_utf8(&selected)
        .map_err(|_| "rustup returned a non-UTF-8 Cargo path".to_owned())?
        .trim();
    if selected.is_empty() || selected.len() > 4 * 1024 {
        return Err("rustup returned an invalid Cargo path".to_owned());
    }
    let selected = PathBuf::from(selected);
    if !selected.is_absolute() {
        return Err("rustup returned a non-absolute Cargo path".to_owned());
    }
    let selected_version = run(&selected, workspace, &["-vV"], 64 * 1024)?;
    if selected_version != expected_version {
        return Err("rustup-selected Cargo differs from the observed Cargo invocation".to_owned());
    }
    selected
        .canonicalize()
        .map_err(|_| "rustup-selected Cargo path cannot be resolved".to_owned())
}

fn hash_tool_role(
    hasher: &mut blake3::Hasher,
    role: &[u8],
    path: &Path,
    file: &CargoToolFileWitness,
) {
    hasher.update(&(role.len() as u64).to_le_bytes());
    hasher.update(role);
    hasher.update(&(path.as_os_str().as_encoded_bytes().len() as u64).to_le_bytes());
    hasher.update(path.as_os_str().as_encoded_bytes());
    hasher.update(file.canonical_path.as_os_str().as_encoded_bytes());
    hasher.update(&file.content_digest);
}

/// Hashes exact bounded executable bytes. For root-owned, read-only Nix store
/// objects, a previous digest may be reused only after revalidating the
/// canonical file and every store-directory identity through the current
/// filesystem. Other paths are rehashed on every observation.
fn tool_executable_witness(
    path: &Path,
    cached: Option<&CargoToolWitnessReuse>,
) -> Result<CargoToolFileWitness, String> {
    let resolved = path
        .canonicalize()
        .map_err(|_| "tool executable path cannot be resolved".to_owned())?;
    let parent = resolved
        .parent()
        .ok_or_else(|| "tool executable has no parent directory".to_owned())?;
    let name = resolved
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| "tool executable name is not UTF-8".to_owned())?;
    let directory = DirectoryCapability::open_read_only_source(parent).map_err(|_| {
        "tool executable directory cannot be held without following links".to_owned()
    })?;
    let mut file = directory
        .open_file_read(name)
        .map_err(|_| "tool executable cannot be opened without following links".to_owned())?;
    let before = file
        .metadata()
        .map_err(|_| "tool executable metadata cannot be read".to_owned())?;
    if !before.is_file() || before.len() == 0 || before.len() > MAX_CARGO_TOOL_BINARY_BYTES {
        return Err("tool executable is not a bounded regular file".to_owned());
    }
    let immutable_identity = immutable_nix_store_file_identity(&resolved, &before);
    let reused = immutable_identity.and_then(|identity| {
        cached?.files.iter().find_map(|entry| {
            (entry.canonical_path == resolved && entry.immutable_identity == identity)
                .then_some(entry.content_digest)
        })
    });
    let mut prefix = [0_u8; 4];
    file.read_exact(&mut prefix)
        .map_err(|_| "tool executable is shorter than its format header".to_owned())?;
    if !is_supported_executable_header(prefix) {
        return Err("selected Cargo/Rust tool is not a verifiable native executable".to_owned());
    }
    let content_digest = if let Some(reused) = reused {
        // The immutable path proof is checked again after the held file read.
        reused
    } else {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"backend.cargo-tool-executable.v2\0");
        hasher.update(resolved.as_os_str().as_encoded_bytes());
        hasher.update(&before.len().to_le_bytes());
        hasher.update(&prefix);
        let mut total = prefix.len() as u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|_| "tool executable bytes could not be read".to_owned())?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(read as u64);
            if total > MAX_CARGO_TOOL_BINARY_BYTES {
                return Err("tool executable exceeds its byte limit".to_owned());
            }
            hasher.update(&buffer[..read]);
        }
        if total != before.len() {
            return Err("tool executable changed while its bytes were observed".to_owned());
        }
        *hasher.finalize().as_bytes()
    };
    let after = file
        .metadata()
        .map_err(|_| "tool executable could not be rechecked".to_owned())?;
    if !same_tool_file_metadata(&before, &after)
        || immutable_identity != immutable_nix_store_file_identity(&resolved, &after)
    {
        return Err("tool executable changed while its bytes were observed".to_owned());
    }
    Ok(CargoToolFileWitness {
        canonical_path: resolved.clone(),
        content_digest,
        reuse: immutable_identity.map(|immutable_identity| CargoToolFileReuse {
            canonical_path: resolved,
            immutable_identity,
            content_digest,
        }),
    })
}

#[cfg(unix)]
fn same_tool_file_metadata(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.mode() == after.mode()
        && before.uid() == after.uid()
        && before.gid() == after.gid()
        && before.nlink() == after.nlink()
        && before.size() == after.size()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

#[cfg(not(unix))]
fn same_tool_file_metadata(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    before.is_file()
        && after.is_file()
        && before.len() == after.len()
        && before.modified().ok() == after.modified().ok()
        && before.permissions().readonly() == after.permissions().readonly()
}

#[cfg(unix)]
fn immutable_nix_store_file_identity(path: &Path, file: &std::fs::Metadata) -> Option<[u8; 32]> {
    use std::os::unix::fs::MetadataExt;

    let store = Path::new("/nix/store");
    let relative = path.strip_prefix(store).ok()?;
    let mut components = relative.components();
    let object_name = components.next()?.as_os_str().to_str()?;
    if !is_nix_store_object_name(object_name) {
        return None;
    }
    let store_metadata = std::fs::symlink_metadata(store).ok()?;
    let store_mode = store_metadata.mode();
    if !store_metadata.is_dir()
        || store_metadata.uid() != 0
        || (store_mode & 0o022 != 0 && store_mode & 0o1000 == 0)
    {
        return None;
    }
    let object_root = store.join(object_name);
    let parent = path.parent()?;
    if !parent.starts_with(&object_root) {
        return None;
    }
    let mut directories = Vec::new();
    let mut current = parent.to_path_buf();
    loop {
        directories.push(current.clone());
        if current == object_root {
            break;
        }
        current = current.parent()?.to_path_buf();
        if !current.starts_with(&object_root) {
            return None;
        }
    }
    directories.reverse();

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.nix-store-immutable-file.v1\0");
    hash_unix_metadata(&mut hasher, store, &store_metadata);
    for directory in directories {
        let metadata = std::fs::symlink_metadata(&directory).ok()?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != 0
            || metadata.mode() & 0o222 != 0
        {
            return None;
        }
        hash_unix_metadata(&mut hasher, &directory, &metadata);
    }
    if !file.is_file() || file.uid() != 0 || file.mode() & 0o222 != 0 {
        return None;
    }
    hash_unix_metadata(&mut hasher, path, file);
    Some(*hasher.finalize().as_bytes())
}

#[cfg(not(unix))]
fn immutable_nix_store_file_identity(_path: &Path, _file: &std::fs::Metadata) -> Option<[u8; 32]> {
    None
}

#[cfg(unix)]
fn hash_unix_metadata(hasher: &mut blake3::Hasher, path: &Path, metadata: &std::fs::Metadata) {
    use std::os::unix::fs::MetadataExt;
    hasher.update(path.as_os_str().as_encoded_bytes());
    for value in [
        metadata.dev(),
        metadata.ino(),
        metadata.mode() as u64,
        metadata.uid() as u64,
        metadata.gid() as u64,
        metadata.nlink(),
        metadata.size(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
    ] {
        hasher.update(&value.to_le_bytes());
    }
}

fn verify_sccache_wrapper(path: &Path, workspace: &Path) -> Result<Vec<u8>, String> {
    let resolved = path
        .canonicalize()
        .map_err(|_| "Cargo wrapper path cannot be resolved".to_owned())?;
    if resolved.file_name() != Some(std::ffi::OsStr::new("sccache")) {
        return Err("Cargo selected an unrecognized rustc wrapper".to_owned());
    }
    let version = run(path, workspace, &["--version"], 16 * 1024)?;
    let version_text = std::str::from_utf8(&version)
        .map_err(|_| "sccache returned a non-UTF-8 version".to_owned())?
        .trim();
    if !is_recognized_sccache_version(version_text) {
        return Err("Cargo selected a wrapper without recognized sccache identity".to_owned());
    }
    for config in sccache_configuration_paths(workspace)? {
        if let Some(bytes) = read_observation_file(&config, MAX_CARGO_CONFIG_BYTES)? {
            let document: toml::Value = std::str::from_utf8(&bytes)
                .map_err(|_| "sccache config is not UTF-8".to_owned())?
                .parse()
                .map_err(|_| "sccache config is malformed".to_owned())?;
            if document.get("include").is_some() {
                return Err("sccache config includes are not admitted".to_owned());
            }
        }
    }
    Ok(version)
}

fn is_nix_store_object_name(name: &str) -> bool {
    let Some((hash, package)) = name.split_once('-') else {
        return false;
    };
    hash.len() == 32
        && !package.is_empty()
        && hash
            .bytes()
            .all(|byte| b"0123456789abcdfghijklmnpqrsvwxyz".contains(&byte))
}

fn is_recognized_sccache_version(version: &str) -> bool {
    version.len() <= 4 * 1024
        && version
            .strip_prefix("sccache ")
            .is_some_and(|version| !version.is_empty())
}

fn is_supported_executable_header(header: [u8; 4]) -> bool {
    header == *b"\x7fELF"
        || &header[..2] == b"MZ"
        || matches!(
            header,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
}

/// Resolves the effective compiler and wrapper configuration Cargo can use.
/// Unknown `[env]` overrides and conflicting config declarations fail closed;
/// explicit compiler and wrapper settings are admitted when their entire
/// native execution chain is measured below.
fn cargo_tool_selection(
    cargo: &Path,
    workspace: &Path,
    cargo_version: &[u8],
) -> Result<CargoToolSelection, String> {
    reject_cargo_env_tool_overrides(workspace)?;
    let config_rustc = cargo_build_tool_value(workspace, "rustc")?;
    let config_wrapper = cargo_build_tool_value(workspace, "rustc-wrapper")?;
    let config_workspace_wrapper = cargo_build_tool_value(workspace, "rustc-workspace-wrapper")?;
    let selected_rustc =
        effective_tool_value("RUSTC", "CARGO_BUILD_RUSTC", config_rustc.as_deref())?;
    let inject_default_rustc = selected_rustc.is_none();
    let rustc = selected_rustc
        .map(|value| resolve_program(&value, workspace, "rustc"))
        .transpose()?
        .or_else(|| {
            cargo
                .parent()
                .map(|directory| directory.join("rustc"))
                .filter(|path| path.is_file())
        })
        .or_else(|| find_executable_on_path(std::ffi::OsStr::new("rustc"), workspace))
        .ok_or_else(|| "the Cargo-selected rustc executable was not found".to_owned())?;
    let rustc_wrapper = effective_tool_value(
        "RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        config_wrapper.as_deref(),
    )?
    .map(|value| resolve_program(&value, workspace, "rustc wrapper"))
    .transpose()?;
    let rustc_workspace_wrapper = effective_tool_value(
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        config_workspace_wrapper.as_deref(),
    )?
    .map(|value| resolve_program(&value, workspace, "rustc workspace wrapper"))
    .transpose()?;
    let cargo = effective_cargo_executable(cargo, workspace, cargo_version)?;
    Ok(CargoToolSelection {
        cargo,
        rustc,
        rustc_wrapper,
        rustc_workspace_wrapper,
        inject_default_rustc,
    })
}

fn reject_cargo_env_tool_overrides(workspace: &Path) -> Result<(), String> {
    for path in cargo_config_paths(workspace)? {
        let Some(bytes) = read_observation_file(&path, MAX_CARGO_CONFIG_BYTES)? else {
            continue;
        };
        if path.extension() == Some(std::ffi::OsStr::new("json")) {
            continue;
        }
        let document: toml::Value = std::str::from_utf8(&bytes)
            .map_err(|_| "Cargo config is not UTF-8 during tool admission".to_owned())?
            .parse()
            .map_err(|_| "Cargo config is malformed during tool admission".to_owned())?;
        if cargo_config_has_env_tool_override(&document) {
            return Err("Cargo [env] tool overrides are not admitted".to_owned());
        }
    }
    Ok(())
}

fn cargo_build_tool_value(workspace: &Path, key: &str) -> Result<Option<String>, String> {
    let mut values = BTreeSet::new();
    for path in cargo_config_paths(workspace)? {
        let Some(bytes) = read_observation_file(&path, MAX_CARGO_CONFIG_BYTES)? else {
            continue;
        };
        if path.extension() == Some(std::ffi::OsStr::new("json")) {
            continue;
        }
        let document: toml::Value = std::str::from_utf8(&bytes)
            .map_err(|_| "Cargo config is not UTF-8 during tool admission".to_owned())?
            .parse()
            .map_err(|_| "Cargo config is malformed during tool admission".to_owned())?;
        let Some(value) = cargo_config_build_value(&document, key)? else {
            continue;
        };
        validate_tool_value(value, key)?;
        values.insert(value.to_owned());
    }
    if values.len() > 1 {
        return Err(format!(
            "Cargo config declares conflicting build.{key} values"
        ));
    }
    Ok(values.into_iter().next())
}

fn cargo_config_build_value<'a>(
    document: &'a toml::Value,
    key: &str,
) -> Result<Option<&'a str>, String> {
    let Some(value) = document
        .get("build")
        .and_then(toml::Value::as_table)
        .and_then(|build| build.get(key))
    else {
        return Ok(None);
    };
    value
        .as_str()
        .map(Some)
        .ok_or_else(|| format!("Cargo build.{key} must be a single executable path"))
}

fn cargo_config_has_env_tool_override(document: &toml::Value) -> bool {
    document
        .get("env")
        .and_then(toml::Value::as_table)
        .is_some_and(|env| {
            ["RUSTC", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"]
                .iter()
                .any(|key| env.contains_key(*key))
        })
}

fn effective_tool_value(
    direct_environment: &str,
    config_environment: &str,
    config_value: Option<&str>,
) -> Result<Option<String>, String> {
    let direct = environment_tool_value(direct_environment)?;
    let configured_environment = environment_tool_value(config_environment)?;
    let values = [
        direct,
        configured_environment,
        config_value.map(str::to_owned),
    ]
    .into_iter()
    .flatten()
    .collect::<BTreeSet<_>>();
    if values.len() > 1 {
        // Cargo's direct tool env and config-key env have documented override
        // paths, but conflicting simultaneous selectors are too easy to
        // misread across releases. Refuse instead of witnessing the wrong one.
        return Err(format!(
            "Cargo has conflicting {direct_environment} tool selections"
        ));
    }
    let value = values.into_iter().next();
    if let Some(value) = value.as_deref() {
        validate_tool_value(value, direct_environment)?;
    }
    Ok(value.filter(|value| !value.is_empty()))
}

fn environment_tool_value(name: &str) -> Result<Option<String>, String> {
    std::env::var_os(name)
        .map(|value| {
            value
                .into_string()
                .map_err(|_| format!("{name} is not UTF-8"))
        })
        .transpose()
}

fn validate_tool_value(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty() || value.len() > 4 * 1024 || value.contains('\0') {
        return Err(format!("Cargo {label} selection is empty or out of bounds"));
    }
    Ok(())
}

fn resolve_program(value: &str, workspace: &Path, label: &str) -> Result<PathBuf, String> {
    validate_tool_value(value, label)?;
    let path = PathBuf::from(value);
    let resolved = if path.is_absolute() {
        Some(path)
    } else if path.components().count() > 1 {
        Some(workspace.join(path))
    } else {
        find_executable_on_path(path.as_os_str(), workspace)
    }
    .ok_or_else(|| format!("Cargo-selected {label} executable was not found"))?;
    if !resolved.is_file() {
        return Err(format!("Cargo-selected {label} executable is not a file"));
    }
    Ok(resolved)
}

fn find_executable_on_path(name: &std::ffi::OsStr, workspace: &Path) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| {
            if directory.is_absolute() {
                directory.join(name)
            } else {
                workspace.join(directory).join(name)
            }
        })
        .find(|path| path.is_file())
}

/// The tree input and its observed paths. The first metadata call discovers
/// the exact package-manifest set. A second call is bracketed by a bounded
/// no-follow read-set witness, and its output must equal the discovery pass.
fn read_project(
    workspace: &Path,
    cached_tool: Option<&CargoToolWitnessReuse>,
) -> Result<
    (
        TreeInput,
        Vec<PathBuf>,
        [u8; 32],
        Option<CargoToolWitnessReuse>,
    ),
    String,
> {
    let workspace = workspace.to_path_buf();
    match coherent_metadata(
        &workspace,
        |workspace| cargo_metadata(workspace, cached_tool),
        cached_tool,
    ) {
        Ok(observed) => {
            let input = metadata_input_with_stable_source_witness(
                &observed.metadata,
                &observed.host,
                observed.lockfile.as_deref(),
                observed.input_witness,
            )
            .map_err(|error| error.to_string())?;
            Ok((
                input,
                observed.watched,
                observed.input_witness,
                observed.tool_witness_reuse,
            ))
        }
        Err(reason) => {
            let mut watched = basic_input_paths(&workspace)?;
            watched.extend(cargo_config_paths(&workspace)?);
            watched.extend(sccache_configuration_paths(&workspace)?);
            watched.extend(rustup_selection_paths(&workspace)?);
            watched.sort();
            watched.dedup();
            let observed = observation_witness(&workspace, &watched, cached_tool)?;
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
            Ok((input, watched, observed.digest, observed.tool_witness_reuse))
        }
    }
}

fn coherent_metadata(
    workspace: &Path,
    run_metadata: impl FnMut(&Path) -> Result<(Vec<u8>, String, [u8; 32]), String>,
    cached_tool: Option<&CargoToolWitnessReuse>,
) -> Result<CoherentMetadata, String> {
    let mut tool_reuse = cached_tool.cloned();
    coherent_metadata_with(workspace, run_metadata, |workspace, files| {
        let observation = strict_observation_witness(workspace, files, tool_reuse.as_ref())?;
        tool_reuse = observation.tool_witness_reuse.clone();
        Ok(observation)
    })
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
        tool_witness_reuse: after.tool_witness_reuse,
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
    paths.extend(sccache_configuration_paths(workspace)?);
    paths.extend(rustup_selection_paths(workspace)?);
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

/// Captures the bounded sccache configuration locations that can affect the
/// recognized transparent wrapper. The paths are watched even when absent,
/// so creating a config invalidates a warm observation.
fn sccache_configuration_paths(workspace: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = BTreeSet::new();
    for key in ["SCCACHE_CONF", "SCCACHE_CONFIG"] {
        if let Some(value) = std::env::var_os(key) {
            let path = PathBuf::from(value);
            let path = if path.is_absolute() {
                path
            } else {
                workspace.join(path)
            };
            paths.insert(path);
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME") {
        let root = PathBuf::from(xdg);
        if !root.is_absolute() {
            return Err("relative XDG_CONFIG_HOME cannot be safely observed".to_owned());
        }
        paths.insert(root.join("sccache/config"));
        paths.insert(root.join("sccache/config.toml"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if !home.is_absolute() {
            return Err("relative HOME cannot be safely observed".to_owned());
        }
        paths.insert(home.join(".config/sccache/config"));
        paths.insert(home.join(".config/sccache/config.toml"));
        paths.insert(home.join("Library/Application Support/Mozilla.sccache/config"));
    }
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let appdata = PathBuf::from(appdata);
        if !appdata.is_absolute() {
            return Err("relative APPDATA cannot be safely observed".to_owned());
        }
        paths.insert(appdata.join("Mozilla/sccache/config/config"));
    }
    if paths.len() > 12 {
        return Err("sccache configuration input set exceeds its limit".to_owned());
    }
    Ok(paths.into_iter().collect())
}

/// Rustup toolchain selectors affect which Cargo and rustc shims execute.
/// Bind selector files from the workspace ancestry plus rustup's default
/// toolchain setting, all as bounded no-follow observations.
fn rustup_selection_paths(workspace: &Path) -> Result<Vec<PathBuf>, String> {
    let ancestors = workspace.ancestors().take(128).collect::<Vec<_>>();
    if ancestors.len() == 128 && ancestors.last().is_some_and(|path| path.parent().is_some()) {
        return Err("rustup selector ancestor chain exceeds its limit".to_owned());
    }
    let mut paths = ancestors
        .into_iter()
        .flat_map(|ancestor| {
            [
                ancestor.join("rust-toolchain"),
                ancestor.join("rust-toolchain.toml"),
            ]
        })
        .collect::<BTreeSet<_>>();
    let rustup_home = match std::env::var_os("RUSTUP_HOME") {
        Some(home) => {
            let home = PathBuf::from(home);
            if !home.is_absolute() {
                return Err("relative RUSTUP_HOME cannot be safely observed".to_owned());
            }
            home
        }
        None => std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| "rustup home cannot be resolved".to_owned())?
            .join(".rustup"),
    };
    paths.insert(rustup_home.join("settings.toml"));
    if paths.len() > MAX_CARGO_CONFIG_INPUTS + 256 {
        return Err("rustup selector input set exceeds its limit".to_owned());
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
fn cargo_metadata(
    workspace: &Path,
    cached: Option<&CargoToolWitnessReuse>,
) -> Result<(Vec<u8>, String, [u8; 32]), String> {
    let environment_before = cargo_environment_witness()?;
    let cargo = selected_cargo_program(workspace)?;
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
    let selection_before = cargo_tool_selection(&cargo, workspace, &version)?;
    let tool_before = metadata_tool_witness(workspace, &version, &selection_before, cached)?;
    let metadata = run_with_default_rustc(
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
        selection_before
            .inject_default_rustc
            .then_some(selection_before.rustc.as_path()),
    )?;
    let selection_after = cargo_tool_selection(&cargo, workspace, &version)?;
    let tool_after = metadata_tool_witness(workspace, &version, &selection_after, cached)?;
    let environment_after = cargo_environment_witness()?;
    if tool_before.digest != tool_after.digest || environment_before != environment_after {
        return Err("Cargo tools or selection environment changed during metadata".to_owned());
    }
    let tool_witness = tool_after.digest;
    Ok((metadata, host, tool_witness))
}

/// `NUDOX_CARGO`, else `cargo` beside `NUDOX_RUSTC`, else the first `cargo`
/// on `PATH` or in the usual install places.
fn selected_cargo_program(workspace: &Path) -> Result<PathBuf, String> {
    if let Some(explicit) = std::env::var_os("NUDOX_CARGO") {
        let explicit = explicit
            .to_str()
            .ok_or_else(|| "NUDOX_CARGO path is not UTF-8".to_owned())?;
        return resolve_program(explicit, workspace, "Cargo");
    }
    if let Some(rustc) = std::env::var_os("NUDOX_RUSTC") {
        let rustc = rustc
            .to_str()
            .ok_or_else(|| "NUDOX_RUSTC path is not UTF-8".to_owned())?;
        let rustc = resolve_program(rustc, workspace, "configured rustc")?;
        if let Some(found) = rustc.parent().map(|bin| bin.join("cargo"))
            && found.is_file()
        {
            return Ok(found);
        }
    }
    let mut candidates: Vec<PathBuf> = std::env::var_os("PATH")
        .into_iter()
        .flat_map(std::env::split_paths)
        .map(|directory| {
            if directory.is_absolute() {
                directory.join("cargo")
            } else {
                workspace.join(directory).join("cargo")
            }
        })
        .collect();
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
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "cargo was not found".to_owned())
}

/// Runs one bounded, deadline-limited command and returns its stdout.
fn run(
    program: &Path,
    directory: &Path,
    arguments: &[&str],
    maximum: usize,
) -> Result<Vec<u8>, String> {
    run_with_default_rustc(program, directory, arguments, maximum, None)
}

fn run_with_default_rustc(
    program: &Path,
    directory: &Path,
    arguments: &[&str],
    maximum: usize,
    default_rustc: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let mut command = Command::new(program);
    if let Some(rustc) = default_rustc {
        // Cargo's selected compiler has already been resolved and witnessed;
        // provide it only when no environment or Cargo config selected one.
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
            tool_witness_reuse: None,
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
            tool_witness_reuse: None,
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

    #[cfg(unix)]
    #[test]
    fn tool_identity_detects_same_size_same_mtime_executable_replacement() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-cargo-tool-witness-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        std::fs::create_dir_all(&scratch.0).expect("tool directory");
        let tool = scratch.0.join("cargo-fixture");
        let replacement = scratch.0.join("replacement");
        std::fs::write(&tool, b"\x7fELFfirst-image").expect("initial executable bytes");
        let modified = std::fs::metadata(&tool)
            .expect("initial metadata")
            .modified()
            .expect("initial modified time");
        std::fs::write(&replacement, b"\x7fELFother-image").expect("replacement bytes");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&replacement)
            .expect("replacement file")
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .expect("preserve mtime");
        let before_metadata = std::fs::metadata(&tool).expect("initial metadata");
        let replacement_metadata = std::fs::metadata(&replacement).expect("replacement metadata");
        assert_eq!(before_metadata.len(), replacement_metadata.len());
        assert_eq!(
            before_metadata.modified().unwrap(),
            replacement_metadata.modified().unwrap()
        );

        let canonical = tool.canonicalize().expect("canonical fixture tool");
        let forged_reuse = CargoToolWitnessReuse {
            files: vec![CargoToolFileReuse {
                canonical_path: canonical,
                immutable_identity: [0x55; 32],
                content_digest: [0xaa; 32],
            }]
            .into_boxed_slice(),
        };
        let before = tool_executable_witness(&tool, Some(&forged_reuse))
            .expect("initial executable witness");
        assert_ne!(
            before.content_digest, [0xaa; 32],
            "mutable paths must ignore cached digest claims"
        );
        std::fs::rename(&replacement, &tool).expect("atomic same-path replacement");
        let after = tool_executable_witness(&tool, Some(&forged_reuse))
            .expect("replacement executable witness");
        assert_ne!(
            before.content_digest, after.content_digest,
            "executable content, not stat metadata, identifies the tool"
        );
    }

    #[test]
    fn cargo_tool_config_preserves_selected_compiler_and_rejects_unmeasured_env_overrides() {
        let build = "[build]\nrustc = '/opt/custom/rustc'\nrustc-wrapper = 'sccache'\nrustc-workspace-wrapper = 'sccache'\n"
            .parse::<toml::Value>()
            .expect("valid Cargo config");
        assert_eq!(
            cargo_config_build_value(&build, "rustc").unwrap(),
            Some("/opt/custom/rustc")
        );
        assert_eq!(
            cargo_config_build_value(&build, "rustc-wrapper").unwrap(),
            Some("sccache")
        );
        assert_eq!(
            cargo_config_build_value(&build, "rustc-workspace-wrapper").unwrap(),
            Some("sccache")
        );
        assert!(!cargo_config_has_env_tool_override(&build));

        for config in [
            "[env]\nRUSTC = '/opt/custom/rustc'\n",
            "[env]\nRUSTC_WRAPPER = 'sccache'\n",
            "[env]\nRUSTC_WORKSPACE_WRAPPER = 'sccache'\n",
        ] {
            let document = config.parse::<toml::Value>().expect("valid Cargo config");
            assert!(cargo_config_has_env_tool_override(&document), "{config}");
        }
        let ordinary = "[build]\ntarget = 'x86_64-unknown-linux-gnu'\n"
            .parse::<toml::Value>()
            .expect("ordinary config");
        assert_eq!(cargo_config_build_value(&ordinary, "rustc").unwrap(), None);
        assert!(!cargo_config_has_env_tool_override(&ordinary));
    }

    #[test]
    fn only_known_sccache_identity_and_canonical_nix_store_names_are_admitted() {
        assert!(is_recognized_sccache_version("sccache 0.17.0"));
        assert!(!is_recognized_sccache_version("cache 0.17.0"));
        assert!(!is_recognized_sccache_version("sccache "));
        assert!(is_nix_store_object_name(
            "4n5rm7aink6xcsj5df33sf7wm3m387a2-sccache-0.17.0"
        ));
        assert!(!is_nix_store_object_name("not-a-store-object"));
        assert!(!is_nix_store_object_name(
            "ffffffffffffffffffffffffffffffff-unknown-hash-alphabet"
        ));
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
    observation_witness(workspace, files, None)
        .expect("source witness")
        .digest
}
