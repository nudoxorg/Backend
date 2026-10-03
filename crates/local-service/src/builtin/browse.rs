//! A project's dependency tree, read from Cargo and the advisory authority.
//!
//! Cargo is the authority for the current target's active dependency graph:
//! `cargo metadata --filter-platform <host>`. Cargo.lock rows outside that
//! graph can also be disabled by feature selection, so the difference is not
//! called "other platforms." When Cargo cannot answer (not installed, no
//! network for a missing download, a stale lockfile under `--locked`), the
//! tree is read from `Cargo.lock` alone and says so.
//!
//! The Cargo half is cached against the effective lock, exact package
//! manifests, Cargo configuration and tools, registry checksums, and bounded
//! auto-target membership. A warm read reuses that graph only after the same
//! inputs are rechecked. Advisories are observed on every read, so a refresh
//! shows at once.

use backend_library::browse::{
    LockedInactiveCoverage, LockfileGraphCoverage, ProjectTree, TreeInput, TreeInputPackage,
    TreeSource, build_tree, lockfile_input, metadata_input,
    metadata_input_with_stable_source_witness,
};
use backend_library::{
    CargoPackageReadmeAbsenceV1, CargoPackageReadmeFailureV1, CargoPackageReadmeLinkFailureV1,
    CargoPackageReadmeLinkRequestV1, CargoPackageReadmeLinkResultV1,
    CargoPackageReadmeLinkTargetV1, CargoPackageReadmeManifestV1, CargoPackageReadmeOriginV1,
    CargoPackageReadmeRequestV1, CargoPackageReadmeResultV1, CargoPackageReadmeRootScopeV1,
    CargoPackageReadmeSelectionV1, CargoPackageReadmeV1, CargoPackageSourceAuthorityFailureV1,
    CargoPackageSourceAuthorityStateV1, CargoPackageSourceAuthorityV1,
    CargoPackageSourceFileResultV1, CargoPackageSourceInventoryCoverageV1,
    CargoPackageSourceInventoryFailureV1, CargoPackageSourceInventoryGapV1,
    CargoPackageSourceInventoryResultV1, CargoPackageSourceInventoryV1, CargoPackageSourcePathV1,
    CargoPackageSourceReadFailureV1, CargoPackageSourceSemanticStatusV1,
    MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS, MAX_CARGO_PACKAGE_SOURCE_INVENTORY_SCAN_ENTRIES,
    PackageReference,
};
use backend_platform::child_output::{
    self, CaptureCommand, CaptureEnvironment, CaptureError, CaptureLimits, OutputStream,
};
use backend_platform::directory::{DirectoryCapability, EntryKind};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Largest `cargo metadata` document admitted.
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
/// Maximum time for one Cargo run and one deferred browse observation.
const CARGO_DEADLINE: Duration = Duration::from_secs(90);
const MAX_CARGO_OBSERVATION_PATHS: usize = 42_048;
const MAX_CARGO_OBSERVATION_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CARGO_OBSERVATION_TOTAL_BYTES: usize = 256 * 1024 * 1024;
const MAX_CARGO_TARGET_MEMBERSHIP_ROOTS: usize = 20_000;
const MAX_CARGO_TARGET_MEMBERSHIP_ENTRIES: usize = 65_536;
const MAX_CARGO_TARGET_MEMBERSHIP_DEPTH: usize = 16;
const MAX_CARGO_TOOL_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CARGO_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_CARGO_CONFIG_INPUTS: usize = 256;
const MAX_CARGO_CONFIG_DEPTH: usize = 16;
const MAX_SOURCE_DIRECTORY_ENTRIES: usize = 2_048;
const MAX_SOURCE_DIRECTORY_DEPTH: usize = 32;
const MAX_BROWSE_CACHED_WORKSPACES: usize = 2;
const MAX_BROWSE_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE: usize = 16;
/// Conservative reserve for the bounded workspace and request-binding index
/// bucket allocations. These maps retain their high-water buckets after row
/// removal, so account for their maximum size once for the cache's lifetime.
const BROWSE_CACHE_INDEX_RETAINED_BYTES: usize = 256 * 1024;

/// One absolute budget for a deferred browse request, including queue wait,
/// every Cargo pass, source witness, and source-file revalidation. A worker
/// owns this state until its terminal completion or joined shutdown.
pub(super) struct ObservationControl {
    cancelled: AtomicBool,
    deadline: Instant,
    #[cfg(test)]
    force_expired: AtomicBool,
}

impl ObservationControl {
    pub(super) fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            deadline: Instant::now() + CARGO_DEADLINE,
            #[cfg(test)]
            force_expired: AtomicBool::new(false),
        }
    }

    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(super) fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub(super) fn is_expired(&self) -> bool {
        #[cfg(test)]
        if self.force_expired.load(Ordering::Acquire) {
            return true;
        }
        Instant::now() >= self.deadline
    }

    #[cfg(test)]
    pub(super) fn expire_for_test(&self) {
        self.force_expired.store(true, Ordering::Release);
    }
}

thread_local! {
    static ACTIVE_OBSERVATION: RefCell<Option<Arc<ObservationControl>>> = const { RefCell::new(None) };
}

/// Installs the worker-owned budget only for one synchronous observation.
/// The guard also restores it if a worker unwinds in a test.
pub(super) fn with_observation_control<T>(
    control: Arc<ObservationControl>,
    operation: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Arc<ObservationControl>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE_OBSERVATION.with(|slot| {
                slot.replace(self.0.take());
            });
        }
    }
    let previous = ACTIVE_OBSERVATION.with(|slot| slot.replace(Some(control)));
    let restore = Restore(previous);
    let value = operation();
    drop(restore);
    value
}

fn observation_budget() -> Result<(), String> {
    ACTIVE_OBSERVATION.with(|slot| {
        let active = slot.borrow();
        let Some(control) = active.as_ref() else {
            return Ok(());
        };
        if control.is_cancelled() {
            Err("Cargo source observation was cancelled".to_owned())
        } else if control.is_expired() {
            Err("Cargo source observation exceeded its deadline".to_owned())
        } else {
            Ok(())
        }
    })
}

fn observation_deadline() -> Option<Instant> {
    ACTIVE_OBSERVATION.with(|slot| slot.borrow().as_ref().map(|control| control.deadline))
}

fn observation_control() -> Option<Arc<ObservationControl>> {
    ACTIVE_OBSERVATION.with(|slot| slot.borrow().as_ref().cloned())
}

/// Reads a bounded regular file in chunks so cancellation and the absolute
/// deadline are checked during large witness/README/source reads.
fn read_bounded_file(reader: &mut impl Read, maximum: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        observation_budget().map_err(std::io::Error::other)?;
        let remaining = maximum.saturating_add(1).saturating_sub(bytes.len());
        if remaining == 0 {
            break;
        }
        let capacity = chunk.len();
        let count = match reader.read(&mut chunk[..remaining.min(capacity)]) {
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    observation_budget().map_err(std::io::Error::other)?;
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct RequestBindingKey {
    requested_root_digest: [u8; 32],
    effective_workspace_root_digest: [u8; 32],
}

/// Bounded, shared immutable Cargo observations for currently admitted trees.
pub(super) struct BrowseCache {
    entries: HashMap<PathBuf, CacheEntry>,
    bindings: HashMap<RequestBindingKey, PathBuf>,
    requested_bindings: HashMap<[u8; 32], RequestBindingKey>,
    cached_bytes: usize,
    use_clock: u64,
    #[cfg(test)]
    counters: BrowseCacheCounters,
}

impl Default for BrowseCache {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            bindings: HashMap::new(),
            requested_bindings: HashMap::new(),
            // HashMap buckets do not shrink on ordinary removal. Reserve the
            // bounded maximum index allocation once instead of reallocating
            // the indexes whenever a source witness changes.
            cached_bytes: BROWSE_CACHE_INDEX_RETAINED_BYTES,
            use_clock: 0,
            #[cfg(test)]
            counters: BrowseCacheCounters::default(),
        }
    }
}

struct CacheEntry {
    witness: [u8; 32],
    lock_origin_witness: [u8; 32],
    target_roots: Vec<PathBuf>,
    watched: Vec<PathBuf>,
    input: Arc<TreeInput>,
    /// Directory from which the exact-manifest metadata command was run.
    /// Cargo tool selection and nested config discovery are rechecked against
    /// this same context when the cached observation is reused.
    metadata_context: PathBuf,
    /// Estimated retained bytes, including this entry's indexes.
    retained_bytes: usize,
    /// Exact source authority digest to package-row index; `None` means an
    /// impossible duplicate digest was observed and is never selected.
    package_rows: HashMap<[u8; 32], Option<usize>>,
    /// Bounded exact request roots that admitted this immutable observation.
    request_bindings: HashMap<RequestBindingKey, CachedRequestBinding>,
    tool_witness_reuse: Option<CargoToolWitnessReuse>,
    last_used: u64,
}

/// Exact request manifest and the project tables it declares. The effective
/// workspace is deliberately absent: Cargo metadata resolves that relationship.
#[derive(Clone, Debug)]
struct RequestedCargoManifest {
    root: PathBuf,
    manifest: PathBuf,
    has_package: bool,
    has_workspace: bool,
}

#[derive(Clone, Copy)]
struct CachedRequestBinding {
    binding: backend_library::browse::ProjectTreeRequestBindingV1,
    last_used: u64,
}

#[cfg(test)]
#[derive(Default)]
struct BrowseCacheCounters {
    tree_input_allocations: usize,
    cache_hits: usize,
    retained_bytes_reused: usize,
    evictions: usize,
    request_binding_evictions: usize,
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
        observation_budget()?;
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
        let mut tree = build_tree(input.as_ref(), &observe);
        let binding =
            backend_library::browse::ProjectTreeRequestBindingV1::for_paths(root, &tree.root)
                .ok_or_else(|| {
                    "Cargo project tree could not bind its requested and resolved roots".to_owned()
                })?;
        observation_budget()?;
        self.admit_request_binding(Path::new(&input.root), binding);
        tree.request_binding = Some(binding);
        Ok(tree)
    }

    /// Reads one source-only text file from the currently revalidated Cargo
    /// package observation. Neither the route nor the request supplies a root
    /// path; the owner recovers it from the admitted metadata row.
    pub(super) fn source_file(
        &mut self,
        request: backend_library::CargoPackageSourceRequestV1,
        path: CargoPackageSourcePathV1,
    ) -> CargoPackageSourceFileResultV1 {
        let valid_request = request.has_admissible_shape();
        let package = request.package;
        let request_binding = request.request_binding;
        if !valid_request {
            return unavailable_source_file(
                None,
                None,
                CargoPackageSourceReadFailureV1::InvalidPackageReference,
            );
        }
        if !path.has_admissible_shape() {
            return unavailable_source_file(
                Some(package),
                Some(request_binding),
                CargoPackageSourceReadFailureV1::InvalidRelativePath,
            );
        }
        if !supported_source_path(path.as_str()) {
            return unavailable_source_file(
                Some(package),
                Some(request_binding),
                CargoPackageSourceReadFailureV1::UnsupportedFileKind,
            );
        }
        let Some(workspace) = self.workspace_for_binding(request_binding) else {
            if self.has_cached_authority(&package) {
                return CargoPackageSourceFileResultV1::Stale {
                    package,
                    request_binding,
                };
            }
            return unavailable_source_file(
                Some(package),
                Some(request_binding),
                CargoPackageSourceReadFailureV1::AuthorityUnavailable,
            );
        };
        let input = match self.input(&workspace) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_source_file(
                    Some(package),
                    Some(request_binding),
                    CargoPackageSourceReadFailureV1::SourceObservationUnavailable,
                );
            }
        };
        if !self.has_current_binding(&workspace, request_binding) {
            return CargoPackageSourceFileResultV1::Stale {
                package,
                request_binding,
            };
        }
        let Some(package_row_index) = self.package_row_index(&workspace, &package) else {
            return CargoPackageSourceFileResultV1::Stale {
                package,
                request_binding,
            };
        };
        let Some(package_row) = input.packages.get(package_row_index) else {
            return unavailable_source_file(
                Some(package),
                Some(request_binding),
                CargoPackageSourceReadFailureV1::AuthorityUnavailable,
            );
        };
        let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &package_row.source_authority
        else {
            return CargoPackageSourceFileResultV1::Stale {
                package,
                request_binding,
            };
        };
        let Some(package_root) = package_row.source_root.clone() else {
            return unavailable_source_file(
                Some(package),
                Some(request_binding),
                CargoPackageSourceReadFailureV1::PackageRootUnavailable,
            );
        };
        if !authority.has_admissible_shape()
            || !authority.matches_package_reference(&package)
            || !authority.matches_package_root_path(&package_root)
            || !authority.matches_workspace_root_path(Path::new(&input.root))
            || !request_binding.matches_workspace_root_identity(authority.roots().workspace_root)
        {
            return unavailable_source_file(
                Some(package),
                Some(request_binding),
                CargoPackageSourceReadFailureV1::AuthorityUnavailable,
            );
        }
        let authority = authority.clone();
        let contents = match read_source_file_under(&package_root, &path) {
            Ok(contents) => contents,
            Err(reason) => {
                return unavailable_source_file(Some(package), Some(request_binding), reason);
            }
        };
        let contents = match String::from_utf8(contents) {
            Ok(contents) if !contents.as_bytes().contains(&0) => contents,
            _ => {
                return unavailable_source_file(
                    Some(package),
                    Some(request_binding),
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
                    Some(request_binding),
                    CargoPackageSourceReadFailureV1::SourceObservationUnavailable,
                );
            }
        };
        let still_current = self.has_current_binding(&workspace, request_binding)
            && self
                .package_row_index(&workspace, &package)
                .and_then(|index| current.packages.get(index))
                .is_some_and(|row| {
                    matches!(
                        &row.source_authority,
                        CargoPackageSourceAuthorityStateV1::Admitted(current)
                            if current.authority_digest() == authority.authority_digest()
                                && current.matches_package_reference(&package)
                    )
                });
        if !still_current {
            return CargoPackageSourceFileResultV1::Stale {
                package,
                request_binding,
            };
        }
        let second = match read_source_file_under(&package_root, &path) {
            Ok(contents) => contents,
            Err(_) => {
                return CargoPackageSourceFileResultV1::Stale {
                    package,
                    request_binding,
                };
            }
        };
        if second.as_slice() != contents.as_bytes() {
            return CargoPackageSourceFileResultV1::Stale {
                package,
                request_binding,
            };
        }
        let contents = contents.into_boxed_str();
        let content_digest = *blake3::hash(contents.as_bytes()).as_bytes();
        CargoPackageSourceFileResultV1::Read {
            package,
            authority,
            request_binding,
            path,
            content_digest,
            contents,
            semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
        }
    }

    /// Reads the manifest-selected README for the exact source release and
    /// currently admitted project-tree request. The request supplies no path.
    pub(super) fn package_readme(
        &mut self,
        request: CargoPackageReadmeRequestV1,
    ) -> CargoPackageReadmeResultV1 {
        let valid_request = request.has_admissible_shape();
        let requested_root_digest = request.requested_root_digest;
        let expected_workspace_root_digest = request.expected_workspace_root_digest;
        let package = request.package;
        if !valid_request {
            return unavailable_package_readme(
                Some(package),
                None,
                CargoPackageReadmeFailureV1::InvalidPackageReference,
            );
        }
        let Some((workspace, request_binding)) =
            self.binding_for_request(requested_root_digest, None)
        else {
            return unavailable_package_readme(
                Some(package),
                None,
                CargoPackageReadmeFailureV1::AuthorityUnavailable,
            );
        };
        if expected_workspace_root_digest
            .is_some_and(|expected| expected != request_binding.effective_workspace_root_digest)
        {
            return CargoPackageReadmeResultV1::Stale {
                package,
                request_binding: Some(request_binding),
            };
        }
        let input = match self.input(&workspace) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_package_readme(
                    Some(package),
                    Some(request_binding),
                    CargoPackageReadmeFailureV1::SourceObservationUnavailable,
                );
            }
        };
        if !self.has_current_binding(&workspace, request_binding)
            || !request_binding.matches_effective_workspace_root(&input.root)
        {
            return CargoPackageReadmeResultV1::Stale {
                package,
                request_binding: Some(request_binding),
            };
        }
        let Some(package_row_index) = self.package_row_index(&workspace, &package) else {
            return CargoPackageReadmeResultV1::Stale {
                package,
                request_binding: Some(request_binding),
            };
        };
        let Some(package_row) = input.packages.get(package_row_index) else {
            return unavailable_package_readme(
                Some(package),
                Some(request_binding),
                CargoPackageReadmeFailureV1::AuthorityUnavailable,
            );
        };
        let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &package_row.source_authority
        else {
            return CargoPackageReadmeResultV1::Stale {
                package,
                request_binding: Some(request_binding),
            };
        };
        let Some(package_root) = package_row.source_root.clone() else {
            return unavailable_package_readme(
                Some(package),
                Some(request_binding),
                CargoPackageReadmeFailureV1::PackageRootUnavailable,
            );
        };
        if !authority.has_admissible_shape()
            || !authority.matches_package_reference(&package)
            || !authority.matches_package_root_path(&package_root)
            || !authority.matches_workspace_root_path(Path::new(&input.root))
            || !request_binding.matches_workspace_root_identity(authority.roots().workspace_root)
        {
            return unavailable_package_readme(
                Some(package),
                Some(request_binding),
                CargoPackageReadmeFailureV1::AuthorityUnavailable,
            );
        }
        let authority = authority.clone();
        let declaration = match package_readme_manifest(&package_root) {
            Ok(declaration) => declaration,
            Err(reason) => {
                return unavailable_package_readme(Some(package), Some(request_binding), reason);
            }
        };
        let selected = match select_and_read_package_readme(
            &package_root,
            Path::new(&input.root),
            &declaration,
        ) {
            Ok(selected) => selected,
            Err(reason) => {
                return unavailable_package_readme(Some(package), Some(request_binding), reason);
            }
        };

        // Revalidate all manifests/configuration after the bounded file read.
        let current = match self.input(&workspace) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_package_readme(
                    Some(package),
                    Some(request_binding),
                    CargoPackageReadmeFailureV1::SourceObservationUnavailable,
                );
            }
        };
        if !self.has_current_binding(&workspace, request_binding) {
            return CargoPackageReadmeResultV1::Stale {
                package,
                request_binding: Some(request_binding),
            };
        }
        let current_declaration = match package_readme_manifest(&package_root) {
            Ok(declaration) => declaration,
            Err(_) => {
                return CargoPackageReadmeResultV1::Stale {
                    package,
                    request_binding: Some(request_binding),
                };
            }
        };
        let current_selected = match select_and_read_package_readme(
            &package_root,
            Path::new(&current.root),
            &current_declaration,
        ) {
            Ok(selected) => selected,
            Err(_) => {
                return CargoPackageReadmeResultV1::Stale {
                    package,
                    request_binding: Some(request_binding),
                };
            }
        };
        if current_declaration != declaration || current_selected != selected {
            return CargoPackageReadmeResultV1::Stale {
                package,
                request_binding: Some(request_binding),
            };
        }
        let still_current = self
            .package_row_index(&workspace, &package)
            .and_then(|index| current.packages.get(index))
            .is_some_and(|row| {
                matches!(
                    &row.source_authority,
                    CargoPackageSourceAuthorityStateV1::Admitted(current)
                        if current.authority_digest() == authority.authority_digest()
                            && current.matches_package_reference(&package)
                )
            });
        if !still_current {
            return CargoPackageReadmeResultV1::Stale {
                package,
                request_binding: Some(request_binding),
            };
        }

        match selected {
            SelectedPackageReadme::Absent(reason) => CargoPackageReadmeResultV1::Absent {
                package,
                authority,
                request_binding,
                reason,
            },
            SelectedPackageReadme::Read {
                root_scope,
                path,
                selection,
                contents,
            } => {
                let contents = match readme_text(contents) {
                    Ok(contents) => contents,
                    Err(reason) => {
                        return unavailable_package_readme(
                            Some(package),
                            Some(request_binding),
                            reason,
                        );
                    }
                };
                let readme = CargoPackageReadmeV1 {
                    root_scope,
                    path,
                    selection,
                    content_digest: *blake3::hash(contents.as_bytes()).as_bytes(),
                    contents,
                    semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
                };
                CargoPackageReadmeResultV1::Read {
                    package,
                    authority,
                    request_binding,
                    readme,
                }
            }
        }
    }

    /// Follows one relative link only while its exact package README origin,
    /// requested project, effective workspace, and Cargo source authority are
    /// still current. Both README and target files are read through held,
    /// no-follow directory capabilities.
    pub(super) fn package_readme_link(
        &mut self,
        request: CargoPackageReadmeLinkRequestV1,
    ) -> CargoPackageReadmeLinkResultV1 {
        if !request.has_admissible_shape() {
            return unavailable_package_readme_link(
                Some(request.origin),
                CargoPackageReadmeLinkFailureV1::InvalidRequest,
            );
        }
        let origin = request.origin;
        let href = request.href;
        let Some(workspace) = self.workspace_for_binding(origin.request_binding) else {
            return unavailable_package_readme_link(
                Some(origin),
                CargoPackageReadmeLinkFailureV1::ObservationUnavailable,
            );
        };
        let readme_request =
            CargoPackageReadmeRequestV1::from_tree(origin.package.clone(), origin.request_binding);
        let readme = match self.package_readme(readme_request) {
            CargoPackageReadmeResultV1::Read {
                package,
                authority,
                request_binding,
                readme,
            } if package == origin.package
                && request_binding == origin.request_binding
                && readme.root_scope == origin.root_scope
                && readme.path == origin.path
                && readme.selection == origin.selection
                && readme.content_digest == origin.content_digest =>
            {
                (authority, readme)
            }
            CargoPackageReadmeResultV1::Stale { .. }
            | CargoPackageReadmeResultV1::Read { .. }
            | CargoPackageReadmeResultV1::Absent { .. } => {
                return CargoPackageReadmeLinkResultV1::Stale { origin };
            }
            CargoPackageReadmeResultV1::Unavailable { .. } => {
                return unavailable_package_readme_link(
                    Some(origin),
                    CargoPackageReadmeLinkFailureV1::ObservationUnavailable,
                );
            }
        };
        let (authority, _readme) = readme;
        let target = match origin.resolve_relative_href(&href) {
            Ok(target) => target,
            Err(reason) => {
                return unavailable_package_readme_link(Some(origin), reason);
            }
        };
        let (path, fragment) = match target {
            CargoPackageReadmeLinkTargetV1::Anchor { fragment } => {
                return CargoPackageReadmeLinkResultV1::Anchor { origin, fragment };
            }
            CargoPackageReadmeLinkTargetV1::File { path, fragment } => (path, fragment),
        };
        let root_scope = origin.root_scope;
        if !supported_source_path(path.as_str()) {
            return unavailable_package_readme_link(
                Some(origin),
                CargoPackageReadmeLinkFailureV1::UnsupportedFileKind,
            );
        }

        let input = match self.input(&workspace) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_package_readme_link(
                    Some(origin),
                    CargoPackageReadmeLinkFailureV1::ObservationUnavailable,
                );
            }
        };
        if !self.has_current_binding(&workspace, origin.request_binding)
            || !origin
                .request_binding
                .matches_effective_workspace_root(&input.root)
        {
            return CargoPackageReadmeLinkResultV1::Stale { origin };
        }
        let Some(package_row_index) = self.package_row_index(&workspace, &origin.package) else {
            return CargoPackageReadmeLinkResultV1::Stale { origin };
        };
        let Some(package_row) = input.packages.get(package_row_index) else {
            return CargoPackageReadmeLinkResultV1::Stale { origin };
        };
        let CargoPackageSourceAuthorityStateV1::Admitted(current_authority) =
            &package_row.source_authority
        else {
            return CargoPackageReadmeLinkResultV1::Stale { origin };
        };
        if !current_authority.matches_package_reference(&origin.package)
            || current_authority.authority_digest() != authority.authority_digest()
        {
            return CargoPackageReadmeLinkResultV1::Stale { origin };
        }
        let Some(package_root) = package_row.source_root.clone() else {
            return unavailable_package_readme_link(
                Some(origin),
                CargoPackageReadmeLinkFailureV1::ObservationUnavailable,
            );
        };
        if !current_authority.has_admissible_shape()
            || !current_authority.matches_package_root_path(&package_root)
            || !current_authority.matches_workspace_root_path(Path::new(&input.root))
        {
            return unavailable_package_readme_link(
                Some(origin),
                CargoPackageReadmeLinkFailureV1::ObservationUnavailable,
            );
        }
        let target_root = match origin.root_scope {
            CargoPackageReadmeRootScopeV1::Package => package_root.clone(),
            CargoPackageReadmeRootScopeV1::EffectiveWorkspace
                if origin.selection == CargoPackageReadmeSelectionV1::WorkspaceInherited
                    && Path::new(&package_root).starts_with(Path::new(&input.root)) =>
            {
                PathBuf::from(&input.root)
            }
            CargoPackageReadmeRootScopeV1::EffectiveWorkspace => {
                return CargoPackageReadmeLinkResultV1::Stale { origin };
            }
        };
        let first = match read_source_file_under_limit(
            &target_root,
            &path,
            backend_library::MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES,
        ) {
            Ok(Some(contents)) => contents,
            Ok(None) => {
                return unavailable_package_readme_link(
                    Some(origin),
                    CargoPackageReadmeLinkFailureV1::TargetUnavailable,
                );
            }
            Err(CargoPackageSourceReadFailureV1::FileTooLarge) => {
                return unavailable_package_readme_link(
                    Some(origin),
                    CargoPackageReadmeLinkFailureV1::TargetTooLarge,
                );
            }
            Err(_) => {
                return unavailable_package_readme_link(
                    Some(origin),
                    CargoPackageReadmeLinkFailureV1::TargetUnavailable,
                );
            }
        };
        let contents = match String::from_utf8(first) {
            Ok(contents) if !contents.as_bytes().contains(&0) => contents,
            _ => {
                return unavailable_package_readme_link(
                    Some(origin),
                    CargoPackageReadmeLinkFailureV1::NotUtf8Text,
                );
            }
        };

        // Repeat the manifest-selected README check after following the link;
        // a changed manifest, source authority, or tree request invalidates
        // the origin instead of returning bytes from a neighboring release.
        match self.package_readme(CargoPackageReadmeRequestV1::from_tree(
            origin.package.clone(),
            origin.request_binding,
        )) {
            CargoPackageReadmeResultV1::Read {
                package,
                authority: current,
                request_binding,
                readme,
            } if package == origin.package
                && current.authority_digest() == authority.authority_digest()
                && request_binding == origin.request_binding
                && readme.root_scope == origin.root_scope
                && readme.path == origin.path
                && readme.selection == origin.selection
                && readme.content_digest == origin.content_digest => {}
            _ => return CargoPackageReadmeLinkResultV1::Stale { origin },
        }
        let second = match read_source_file_under_limit(
            &target_root,
            &path,
            backend_library::MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES,
        ) {
            Ok(Some(contents)) => contents,
            _ => return CargoPackageReadmeLinkResultV1::Stale { origin },
        };
        if contents.as_bytes() != second.as_slice()
            || !self.has_current_binding(&workspace, origin.request_binding)
        {
            return CargoPackageReadmeLinkResultV1::Stale { origin };
        }
        CargoPackageReadmeLinkResultV1::Read {
            origin,
            authority,
            root_scope,
            path,
            fragment,
            content_digest: *blake3::hash(contents.as_bytes()).as_bytes(),
            contents: contents.into_boxed_str(),
            semantic: CargoPackageSourceSemanticStatusV1::NotIndexed,
        }
    }

    /// Lists a bounded set of path addresses from the currently revalidated
    /// Cargo package observation. Each path remains a hint and must be read
    /// separately through [`Self::source_file`].
    pub(super) fn source_inventory(
        &mut self,
        request: backend_library::CargoPackageSourceRequestV1,
    ) -> CargoPackageSourceInventoryResultV1 {
        let valid_request = request.has_admissible_shape();
        let package = request.package;
        let request_binding = request.request_binding;
        if !valid_request {
            return unavailable_source_inventory(
                None,
                None,
                CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
            );
        }
        let Some(workspace) = self.workspace_for_binding(request_binding) else {
            if self.has_cached_authority(&package) {
                return CargoPackageSourceInventoryResultV1::Stale {
                    package,
                    request_binding,
                };
            }
            return unavailable_source_inventory(
                Some(package),
                Some(request_binding),
                CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
            );
        };
        let input = match self.input(&workspace) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_source_inventory(
                    Some(package),
                    Some(request_binding),
                    CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
                );
            }
        };
        if !self.has_current_binding(&workspace, request_binding) {
            return CargoPackageSourceInventoryResultV1::Stale {
                package,
                request_binding,
            };
        }
        let Some(package_row_index) = self.package_row_index(&workspace, &package) else {
            return CargoPackageSourceInventoryResultV1::Stale {
                package,
                request_binding,
            };
        };
        let Some(package_row) = input.packages.get(package_row_index) else {
            return unavailable_source_inventory(
                Some(package),
                Some(request_binding),
                CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
            );
        };
        let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &package_row.source_authority
        else {
            return CargoPackageSourceInventoryResultV1::Stale {
                package,
                request_binding,
            };
        };
        let Some(package_root) = package_row.source_root.clone() else {
            return unavailable_source_inventory(
                Some(package),
                Some(request_binding),
                CargoPackageSourceInventoryFailureV1::PackageRootUnavailable,
            );
        };
        if !authority.has_admissible_shape()
            || !authority.matches_package_reference(&package)
            || !authority.matches_package_root_path(&package_root)
            || !authority.matches_workspace_root_path(Path::new(&input.root))
            || !request_binding.matches_workspace_root_identity(authority.roots().workspace_root)
        {
            return unavailable_source_inventory(
                Some(package),
                Some(request_binding),
                CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
            );
        }
        let authority = authority.clone();
        let first = match source_inventory_under(&package_root) {
            Ok(inventory) => inventory,
            Err(reason) => {
                return unavailable_source_inventory(Some(package), Some(request_binding), reason);
            }
        };
        // Directory names can change independently of Cargo metadata. Require
        // two identical no-follow enumerations before returning address hints.
        let second = match source_inventory_under(&package_root) {
            Ok(inventory) => inventory,
            Err(reason) => {
                return unavailable_source_inventory(Some(package), Some(request_binding), reason);
            }
        };
        if first != second {
            return CargoPackageSourceInventoryResultV1::Stale {
                package,
                request_binding,
            };
        }

        // Recheck the Cargo metadata, config, and tool-selection witness after
        // walking. A route is useful only while its exact source authority is
        // still present in the owner's current observation.
        let current = match self.input(&workspace) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_source_inventory(
                    Some(package),
                    Some(request_binding),
                    CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
                );
            }
        };
        let still_current = self.has_current_binding(&workspace, request_binding)
            && self
                .package_row_index(&workspace, &package)
                .and_then(|index| current.packages.get(index))
                .is_some_and(|row| {
                    matches!(
                        &row.source_authority,
                        CargoPackageSourceAuthorityStateV1::Admitted(current)
                            if current.authority_digest() == authority.authority_digest()
                                && current.matches_package_reference(&package)
                    )
                });
        if !still_current {
            return CargoPackageSourceInventoryResultV1::Stale {
                package,
                request_binding,
            };
        }
        CargoPackageSourceInventoryResultV1::Listed(CargoPackageSourceInventoryV1 {
            package,
            authority,
            request_binding,
            paths: first.paths.into_boxed_slice(),
            coverage: first.coverage,
        })
    }

    fn input(&mut self, root: &Path) -> Result<Arc<TreeInput>, String> {
        self.input_with_read_project(root, read_project)
    }

    /// Shares the owner input path with a narrow observation seam so request
    /// scope and cache membership can be tested without starting Cargo.
    fn input_with_read_project(
        &mut self,
        root: &Path,
        read: impl FnOnce(
            &RequestedCargoManifest,
            Option<&CargoToolWitnessReuse>,
        ) -> Result<ProjectInputRead, String>,
    ) -> Result<Arc<TreeInput>, String> {
        let requested = requested_cargo_manifest(root)?.ok_or_else(|| {
            format!(
                "requested project {} is outside Cargo scope: its exact directory has no recognized Cargo package or workspace manifest; ancestor workspaces are not used for project-tree requests",
                root.display()
            )
        })?;

        // A cached workspace is reusable only when its actual Cargo metadata
        // proves this exact manifest is a member (or this exact manifest is a
        // virtual workspace root). This also lets a warmed ancestor workspace
        // entry coexist with an excluded standalone package inside it.
        let candidates = self
            .entries
            .iter()
            .filter(|(_, entry)| {
                // Cargo config and selected-tool lookup are contextual to the
                // exact manifest directory used for metadata. Until cache
                // entries retain a separately comparable graph witness and
                // per-request config/tool recipe, reuse only the context that
                // actually produced this observation.
                entry.metadata_context == requested.root
                    && tree_input_proves_requested_manifest(
                        &entry.input,
                        &requested,
                        &entry.watched,
                    )
            })
            .map(|(workspace, _)| workspace.clone())
            .collect::<Vec<_>>();
        for workspace in candidates {
            let observed = {
                let entry = self
                    .entries
                    .get(&workspace)
                    .expect("candidate entry remained present during freshness check");
                observation_witness_for_context(
                    &workspace,
                    &entry.metadata_context,
                    &entry.watched,
                    entry.tool_witness_reuse.as_ref(),
                )
                .and_then(|observed| {
                    let target_witness = cargo_auto_target_membership_witness(&entry.target_roots)?;
                    Ok(compose_cargo_input_witness(
                        observed.digest,
                        target_witness,
                        entry.lock_origin_witness,
                    ))
                })
            };
            // A cancelled witness read says nothing about freshness. Keep the
            // prior admitted observation and bindings for a later request.
            observation_budget()?;
            if observed.is_ok_and(|observed| {
                self.entries
                    .get(&workspace)
                    .is_some_and(|entry| observed == entry.witness)
            }) {
                self.touch_entry(&workspace);
                let entry = self
                    .entries
                    .get(&workspace)
                    .expect("cache entry remained present while touching it");
                #[cfg(test)]
                {
                    self.counters.cache_hits = self.counters.cache_hits.saturating_add(1);
                    self.counters.retained_bytes_reused = self
                        .counters
                        .retained_bytes_reused
                        .saturating_add(entry.retained_bytes);
                }
                return Ok(Arc::clone(&entry.input));
            }
            // A failed or changed witness revokes every binding before a new
            // exact-manifest metadata observation can be admitted.
            self.remove_workspace(&workspace);
        }

        let metadata_context = requested.root.clone();
        let read = read(&requested, None)?;
        let witness = read.witness();
        if !tree_input_proves_requested_manifest(&read.input, &requested, &read.watched) {
            return Err(
                "Cargo metadata did not admit the exact requested package manifest in its resolved workspace".to_owned(),
            );
        }
        let workspace = PathBuf::from(&read.input.root);
        let canonical_workspace = workspace
            .canonicalize()
            .map_err(|_| "Cargo metadata returned a missing workspace root".to_owned())?;
        if !workspace.is_absolute() || canonical_workspace != workspace {
            return Err("Cargo metadata returned a noncanonical workspace root".to_owned());
        }
        // Replacing an entry at the same effective root must also revoke its
        // old request bindings, even if it did not prove this request.
        self.remove_workspace(&workspace);
        let target_roots = read.target_roots;
        let input = Arc::new(read.input);
        let package_rows = source_package_row_index(&input);
        let retained_bytes = browse_entry_retained_bytes(
            &workspace,
            &metadata_context,
            &read.watched,
            read.watched.capacity(),
            &input,
            &package_rows,
            read.tool_witness_reuse.as_ref(),
        );
        #[cfg(test)]
        {
            self.counters.tree_input_allocations =
                self.counters.tree_input_allocations.saturating_add(1);
        }
        if retained_bytes <= MAX_BROWSE_CACHE_BYTES {
            while self.entries.len() >= MAX_BROWSE_CACHED_WORKSPACES
                || self.cached_bytes.saturating_add(retained_bytes) > MAX_BROWSE_CACHE_BYTES
            {
                let Some(oldest) = self
                    .entries
                    .iter()
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(workspace, _)| workspace.clone())
                else {
                    break;
                };
                self.remove_workspace(&oldest);
                #[cfg(test)]
                {
                    self.counters.evictions = self.counters.evictions.saturating_add(1);
                }
            }
            if self.entries.len() < MAX_BROWSE_CACHED_WORKSPACES
                && self.cached_bytes.saturating_add(retained_bytes) <= MAX_BROWSE_CACHE_BYTES
            {
                let last_used = self.next_use();
                self.cached_bytes = self.cached_bytes.saturating_add(retained_bytes);
                self.entries.insert(
                    workspace.clone(),
                    CacheEntry {
                        witness,
                        lock_origin_witness: read.lock_origin_witness,
                        target_roots,
                        watched: read.watched,
                        input: Arc::clone(&input),
                        metadata_context,
                        retained_bytes,
                        package_rows,
                        request_bindings: HashMap::new(),
                        tool_witness_reuse: read.tool_witness_reuse,
                        last_used,
                    },
                );
            }
        }
        Ok(input)
    }

    fn next_use(&mut self) -> u64 {
        self.use_clock = self.use_clock.wrapping_add(1).max(1);
        self.use_clock
    }

    fn touch_entry(&mut self, workspace: &Path) {
        let last_used = self.next_use();
        if let Some(entry) = self.entries.get_mut(workspace) {
            entry.last_used = last_used;
        }
    }

    fn remove_workspace(&mut self, workspace: &Path) {
        let Some(entry) = self.entries.remove(workspace) else {
            return;
        };
        self.cached_bytes = self.cached_bytes.saturating_sub(entry.retained_bytes);
        for (key, _) in entry.request_bindings {
            if self
                .bindings
                .get(&key)
                .is_some_and(|cached| cached.as_path() == workspace)
            {
                self.bindings.remove(&key);
            }
            if self.requested_bindings.get(&key.requested_root_digest) == Some(&key) {
                self.requested_bindings.remove(&key.requested_root_digest);
            }
        }
    }

    fn admit_request_binding(
        &mut self,
        workspace: &Path,
        binding: backend_library::browse::ProjectTreeRequestBindingV1,
    ) {
        let workspace = workspace.to_path_buf();
        if !self.entries.contains_key(&workspace) {
            return;
        }
        let key = request_binding_key(binding);
        if let Some(previous) = self
            .requested_bindings
            .get(&binding.requested_root_digest)
            .copied()
            && previous != key
        {
            self.remove_binding(previous);
        }
        if self
            .bindings
            .get(&key)
            .is_some_and(|cached_workspace| cached_workspace != &workspace)
        {
            self.remove_binding(key);
        }
        let binding_exists = self
            .entries
            .get(&workspace)
            .is_some_and(|entry| entry.request_bindings.contains_key(&key));
        if !binding_exists
            && self.entries.get(&workspace).is_some_and(|entry| {
                entry.request_bindings.len() >= MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE
            })
        {
            let oldest = self.entries.get(&workspace).and_then(|entry| {
                entry
                    .request_bindings
                    .iter()
                    .min_by_key(|(_, binding)| binding.last_used)
                    .map(|(key, _)| *key)
            });
            if let Some(oldest) = oldest {
                self.remove_binding(oldest);
                #[cfg(test)]
                {
                    self.counters.request_binding_evictions =
                        self.counters.request_binding_evictions.saturating_add(1);
                }
            }
        }
        let last_used = self.next_use();
        let Some(entry) = self.entries.get_mut(&workspace) else {
            return;
        };
        entry
            .request_bindings
            .insert(key, CachedRequestBinding { binding, last_used });
        self.bindings.insert(key, workspace);
        self.requested_bindings
            .insert(binding.requested_root_digest, key);
    }

    fn remove_binding(&mut self, key: RequestBindingKey) {
        if let Some(workspace) = self.bindings.remove(&key)
            && let Some(entry) = self.entries.get_mut(&workspace)
        {
            entry.request_bindings.remove(&key);
        }
        if self.requested_bindings.get(&key.requested_root_digest) == Some(&key) {
            self.requested_bindings.remove(&key.requested_root_digest);
        }
    }

    fn workspace_for_binding(
        &mut self,
        binding: backend_library::browse::ProjectTreeRequestBindingV1,
    ) -> Option<PathBuf> {
        let key = request_binding_key(binding);
        let workspace = self.bindings.get(&key)?.clone();
        if !self.has_current_binding(&workspace, binding) {
            return None;
        }
        self.touch_binding(&workspace, key);
        Some(workspace)
    }

    fn binding_for_request(
        &mut self,
        requested_root_digest: [u8; 32],
        expected_workspace_root_digest: Option<[u8; 32]>,
    ) -> Option<(
        PathBuf,
        backend_library::browse::ProjectTreeRequestBindingV1,
    )> {
        let key = if let Some(effective) = expected_workspace_root_digest {
            RequestBindingKey {
                requested_root_digest,
                effective_workspace_root_digest: effective,
            }
        } else {
            *self.requested_bindings.get(&requested_root_digest)?
        };
        let workspace = self.bindings.get(&key)?.clone();
        let binding = self
            .entries
            .get(&workspace)?
            .request_bindings
            .get(&key)?
            .binding;
        if request_binding_key(binding) != key {
            return None;
        }
        self.touch_binding(&workspace, key);
        Some((workspace, binding))
    }

    fn has_current_binding(
        &self,
        workspace: &Path,
        binding: backend_library::browse::ProjectTreeRequestBindingV1,
    ) -> bool {
        self.entries
            .get(workspace)
            .and_then(|entry| entry.request_bindings.get(&request_binding_key(binding)))
            .is_some_and(|cached| cached.binding == binding)
            && self
                .bindings
                .get(&request_binding_key(binding))
                .is_some_and(|cached_workspace| cached_workspace.as_path() == workspace)
    }

    fn touch_binding(&mut self, workspace: &Path, key: RequestBindingKey) {
        let last_used = self.next_use();
        if let Some(binding) = self
            .entries
            .get_mut(workspace)
            .and_then(|entry| entry.request_bindings.get_mut(&key))
        {
            binding.last_used = last_used;
        }
    }

    fn has_cached_authority(&self, package: &PackageReference) -> bool {
        let Some(digest) = CargoPackageSourceAuthorityV1::digest_from_package_reference(package)
        else {
            return false;
        };
        // At most two workspaces are retained, so this is a constant-bounded
        // lookup through each workspace's direct authority index rather than
        // a second process-global map with independent retention accounting.
        self.entries
            .values()
            .any(|entry| entry.package_rows.contains_key(&digest))
    }

    fn package_row_index(&self, workspace: &Path, package: &PackageReference) -> Option<usize> {
        let digest = CargoPackageSourceAuthorityV1::digest_from_package_reference(package)?;
        self.entries
            .get(workspace)?
            .package_rows
            .get(&digest)
            .copied()
            .flatten()
    }
}

fn unavailable_source_file(
    package: Option<PackageReference>,
    request_binding: Option<backend_library::browse::ProjectTreeRequestBindingV1>,
    reason: CargoPackageSourceReadFailureV1,
) -> CargoPackageSourceFileResultV1 {
    CargoPackageSourceFileResultV1::Unavailable {
        package,
        request_binding,
        reason,
    }
}

fn unavailable_package_readme(
    package: Option<PackageReference>,
    request_binding: Option<backend_library::browse::ProjectTreeRequestBindingV1>,
    reason: CargoPackageReadmeFailureV1,
) -> CargoPackageReadmeResultV1 {
    CargoPackageReadmeResultV1::Unavailable {
        package,
        request_binding,
        reason,
    }
}

fn unavailable_package_readme_link(
    origin: Option<CargoPackageReadmeOriginV1>,
    reason: CargoPackageReadmeLinkFailureV1,
) -> CargoPackageReadmeLinkResultV1 {
    CargoPackageReadmeLinkResultV1::Unavailable { origin, reason }
}

fn unavailable_source_inventory(
    package: Option<PackageReference>,
    request_binding: Option<backend_library::browse::ProjectTreeRequestBindingV1>,
    reason: CargoPackageSourceInventoryFailureV1,
) -> CargoPackageSourceInventoryResultV1 {
    CargoPackageSourceInventoryResultV1::Unavailable {
        package,
        request_binding,
        reason,
    }
}

fn request_binding_key(
    binding: backend_library::browse::ProjectTreeRequestBindingV1,
) -> RequestBindingKey {
    RequestBindingKey {
        requested_root_digest: binding.requested_root_digest,
        effective_workspace_root_digest: binding.effective_workspace_root_digest,
    }
}

fn source_package_row_index(input: &TreeInput) -> HashMap<[u8; 32], Option<usize>> {
    let mut rows: HashMap<[u8; 32], Option<usize>> = HashMap::new();
    for (index, package) in input.packages.iter().enumerate() {
        let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &package.source_authority
        else {
            continue;
        };
        rows.entry(authority.authority_digest())
            .and_modify(|row| *row = None)
            .or_insert(Some(index));
    }
    rows
}

fn browse_entry_retained_bytes(
    workspace: &PathBuf,
    metadata_context: &PathBuf,
    watched: &[PathBuf],
    watched_capacity: usize,
    input: &TreeInput,
    package_rows: &HashMap<[u8; 32], Option<usize>>,
    tool_reuse: Option<&CargoToolWitnessReuse>,
) -> usize {
    // Each admitted request binding retains a workspace path clone in the
    // direct binding index. The cache-wide reserve accounts for all bounded
    // hash-map bucket allocations, including high-water capacity after eviction.
    let binding_path_budget =
        MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE.saturating_mul(workspace.capacity());
    let mut bytes = std::mem::size_of::<CacheEntry>()
        .saturating_add(workspace.capacity())
        .saturating_add(metadata_context.capacity())
        .saturating_add(1_024)
        .saturating_add(binding_path_budget)
        .saturating_add(std::mem::size_of::<Arc<TreeInput>>())
        .saturating_add(std::mem::size_of::<TreeInput>())
        .saturating_add(input.root.capacity())
        .saturating_add(input.packages.capacity() * std::mem::size_of::<TreeInputPackage>())
        .saturating_add(
            input.edges.capacity() * std::mem::size_of::<backend_library::browse::TreeEdge>(),
        )
        .saturating_add(package_rows.capacity().saturating_mul(256))
        .saturating_add(watched_capacity.saturating_mul(std::mem::size_of::<PathBuf>()));
    match &input.source {
        TreeSource::Cargo { host } => bytes = bytes.saturating_add(host.capacity()),
        TreeSource::Lockfile { reason, .. } => bytes = bytes.saturating_add(reason.capacity()),
    }
    for path in watched {
        bytes = bytes.saturating_add(path.capacity());
    }
    if let Some(tool_reuse) = tool_reuse {
        // `files` is a boxed slice, so its length is its exact element count
        // with no spare vector capacity.
        bytes = bytes
            .saturating_add(tool_reuse.files.len() * std::mem::size_of::<CargoToolFileReuse>());
        for file in &tool_reuse.files {
            bytes = bytes.saturating_add(file.canonical_path.capacity());
        }
    }
    for package in &input.packages {
        bytes = bytes
            .saturating_add(package.id.capacity())
            .saturating_add(package.name.capacity())
            .saturating_add(package.version.capacity())
            .saturating_add(package.license.as_ref().map_or(0, String::capacity))
            .saturating_add(package.description.as_ref().map_or(0, String::capacity))
            .saturating_add(package.categories.capacity() * std::mem::size_of::<String>())
            .saturating_add(package.keywords.capacity() * std::mem::size_of::<String>());
        if let Some(root) = &package.source_root {
            bytes = bytes.saturating_add(root.capacity());
        }
        if let Some(origin) = &package.origin {
            bytes = bytes.saturating_add(match origin {
                backend_library::browse::PackageOrigin::Registry { source }
                | backend_library::browse::PackageOrigin::Git { source } => source.capacity(),
                backend_library::browse::PackageOrigin::Vendored { path } => path.capacity(),
                backend_library::browse::PackageOrigin::Unresolved { source } => {
                    source.as_ref().map_or(0, String::capacity)
                }
            });
        }
        for value in package.categories.iter().chain(package.keywords.iter()) {
            bytes = bytes.saturating_add(value.capacity());
        }
        if let CargoPackageSourceAuthorityStateV1::Admitted(authority) = &package.source_authority {
            // The authority owns bounded text through the library's receipt
            // type; charge its actual string capacities rather than each
            // field's maximum possible payload.
            bytes = bytes
                .saturating_add(std::mem::size_of_val(authority))
                .saturating_add(authority.retained_text_capacity_bytes());
        }
    }
    for edge in &input.edges {
        bytes = bytes
            .saturating_add(edge.from.capacity())
            .saturating_add(edge.to.capacity());
    }
    bytes
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceInventoryScan {
    paths: Vec<CargoPackageSourcePathV1>,
    coverage: CargoPackageSourceInventoryCoverageV1,
}

fn source_inventory_under(
    package_root: &Path,
) -> Result<SourceInventoryScan, CargoPackageSourceInventoryFailureV1> {
    observation_budget().map_err(|_| CargoPackageSourceInventoryFailureV1::DirectoryUnavailable)?;
    let root = DirectoryCapability::open_read_only_source(package_root)
        .map_err(|_| CargoPackageSourceInventoryFailureV1::DirectoryUnavailable)?;
    let mut scan = SourceInventoryScan {
        paths: Vec::new(),
        coverage: CargoPackageSourceInventoryCoverageV1::Complete,
    };
    let mut visited = 0_usize;
    walk_source_inventory(&root, "", 0, &mut visited, &mut scan);
    observation_budget().map_err(|_| CargoPackageSourceInventoryFailureV1::DirectoryUnavailable)?;
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
    if observation_budget().is_err() {
        scan.coverage = CargoPackageSourceInventoryCoverageV1::Partial {
            reason: CargoPackageSourceInventoryGapV1::DirectoryUnavailable,
        };
        return;
    }
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
        if observation_budget().is_err() {
            scan.coverage = CargoPackageSourceInventoryCoverageV1::Partial {
                reason: CargoPackageSourceInventoryGapV1::DirectoryUnavailable,
            };
            return;
        }
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

#[derive(Debug, PartialEq)]
enum SelectedPackageReadme {
    Absent(CargoPackageReadmeAbsenceV1),
    Read {
        root_scope: CargoPackageReadmeRootScopeV1,
        path: CargoPackageSourcePathV1,
        selection: CargoPackageReadmeSelectionV1,
        contents: Vec<u8>,
    },
}

fn package_readme_manifest(
    package_root: &Path,
) -> Result<CargoPackageReadmeManifestV1, CargoPackageReadmeFailureV1> {
    let manifest_path = CargoPackageSourcePathV1::new("Cargo.toml")
        .map_err(|_| CargoPackageReadmeFailureV1::PackageManifestMalformed)?;
    let bytes = read_source_file_under_limit(
        package_root,
        &manifest_path,
        backend_library::MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES,
    )
    .map_err(|error| match error {
        CargoPackageSourceReadFailureV1::FileTooLarge => {
            CargoPackageReadmeFailureV1::PackageManifestTooLarge
        }
        _ => CargoPackageReadmeFailureV1::PackageManifestUnavailable,
    })?
    .ok_or(CargoPackageReadmeFailureV1::PackageManifestUnavailable)?;
    let manifest = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|manifest| toml::from_str::<toml::Value>(manifest).ok())
        .ok_or(CargoPackageReadmeFailureV1::PackageManifestMalformed)?;
    let Some(package) = manifest.get("package") else {
        return Err(CargoPackageReadmeFailureV1::PackageManifestMalformed);
    };
    let Some(readme) = package.get("readme") else {
        return Ok(CargoPackageReadmeManifestV1::Unspecified);
    };
    match readme {
        toml::Value::String(path)
            if !path.is_empty()
                && path.len() <= backend_library::MAX_CARGO_PACKAGE_SOURCE_PATH_BYTES =>
        {
            Ok(CargoPackageReadmeManifestV1::Path(path.clone()))
        }
        toml::Value::Boolean(true) => Ok(CargoPackageReadmeManifestV1::Enabled),
        toml::Value::Boolean(false) => Ok(CargoPackageReadmeManifestV1::Disabled),
        toml::Value::Table(fields)
            if fields.len() == 1
                && fields.get("workspace") == Some(&toml::Value::Boolean(true)) =>
        {
            Ok(CargoPackageReadmeManifestV1::WorkspaceInherited)
        }
        _ => Err(CargoPackageReadmeFailureV1::PackageManifestMalformed),
    }
}

fn select_and_read_package_readme(
    package_root: &Path,
    workspace_root: &Path,
    readme: &CargoPackageReadmeManifestV1,
) -> Result<SelectedPackageReadme, CargoPackageReadmeFailureV1> {
    match readme {
        CargoPackageReadmeManifestV1::Path(path) => read_explicit_package_readme(
            package_root,
            manifest_readme_path(package_root, path)?,
            CargoPackageReadmeSelectionV1::ManifestPath,
        ),
        CargoPackageReadmeManifestV1::Disabled => Ok(SelectedPackageReadme::Absent(
            CargoPackageReadmeAbsenceV1::ManifestDisabled,
        )),
        CargoPackageReadmeManifestV1::Enabled => read_explicit_package_readme(
            package_root,
            CargoPackageSourcePathV1::new("README.md")
                .map_err(|_| CargoPackageReadmeFailureV1::InvalidReadmePath)?,
            CargoPackageReadmeSelectionV1::ManifestTrueDefault,
        ),
        CargoPackageReadmeManifestV1::WorkspaceInherited => {
            read_workspace_inherited_readme(package_root, workspace_root)
        }
        CargoPackageReadmeManifestV1::Unspecified => read_cargo_conventional_readme(package_root),
    }
}

fn read_workspace_inherited_readme(
    package_root: &Path,
    workspace_root: &Path,
) -> Result<SelectedPackageReadme, CargoPackageReadmeFailureV1> {
    if package_root.strip_prefix(workspace_root).is_err() {
        return Err(CargoPackageReadmeFailureV1::WorkspaceReadmeUnresolved);
    }
    let manifest_path = CargoPackageSourcePathV1::new("Cargo.toml")
        .map_err(|_| CargoPackageReadmeFailureV1::WorkspaceReadmeUnresolved)?;
    let bytes = read_source_file_under_limit(
        workspace_root,
        &manifest_path,
        backend_library::MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES,
    )
    .map_err(|_| CargoPackageReadmeFailureV1::WorkspaceReadmeUnresolved)?
    .ok_or(CargoPackageReadmeFailureV1::WorkspaceReadmeUnresolved)?;
    let manifest = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|manifest| toml::from_str::<toml::Value>(manifest).ok())
        .ok_or(CargoPackageReadmeFailureV1::WorkspaceReadmeUnresolved)?;
    let value = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("readme"))
        .ok_or(CargoPackageReadmeFailureV1::WorkspaceReadmeUnresolved)?;
    let workspace_path = match value {
        toml::Value::String(path) => path.as_str(),
        toml::Value::Boolean(true) => "README.md",
        toml::Value::Boolean(false) => {
            return Ok(SelectedPackageReadme::Absent(
                CargoPackageReadmeAbsenceV1::ManifestDisabled,
            ));
        }
        _ => return Err(CargoPackageReadmeFailureV1::WorkspaceReadmeUnresolved),
    };
    let path = manifest_readme_path(workspace_root, workspace_path)?;
    read_explicit_readme(
        workspace_root,
        CargoPackageReadmeRootScopeV1::EffectiveWorkspace,
        path,
        CargoPackageReadmeSelectionV1::WorkspaceInherited,
    )
}

fn read_cargo_conventional_readme(
    package_root: &Path,
) -> Result<SelectedPackageReadme, CargoPackageReadmeFailureV1> {
    for name in ["README.md", "README.txt", "README"] {
        let path = CargoPackageSourcePathV1::new(name)
            .map_err(|_| CargoPackageReadmeFailureV1::InvalidReadmePath)?;
        match read_source_file_under_limit(
            package_root,
            &path,
            backend_library::MAX_CARGO_PACKAGE_README_BYTES,
        ) {
            Ok(Some(contents)) => {
                return Ok(SelectedPackageReadme::Read {
                    root_scope: CargoPackageReadmeRootScopeV1::Package,
                    path,
                    selection: CargoPackageReadmeSelectionV1::CargoConventionalDefault,
                    contents,
                });
            }
            Ok(None) => {}
            Err(CargoPackageSourceReadFailureV1::FileTooLarge) => {
                return Err(CargoPackageReadmeFailureV1::ContentTooLarge);
            }
            Err(_) => return Err(CargoPackageReadmeFailureV1::SelectedFileUnavailable),
        }
    }
    Ok(SelectedPackageReadme::Absent(
        CargoPackageReadmeAbsenceV1::NoCargoDefault,
    ))
}

fn read_explicit_package_readme(
    package_root: &Path,
    path: CargoPackageSourcePathV1,
    selection: CargoPackageReadmeSelectionV1,
) -> Result<SelectedPackageReadme, CargoPackageReadmeFailureV1> {
    read_explicit_readme(
        package_root,
        CargoPackageReadmeRootScopeV1::Package,
        path,
        selection,
    )
}

fn read_explicit_readme(
    root: &Path,
    root_scope: CargoPackageReadmeRootScopeV1,
    path: CargoPackageSourcePathV1,
    selection: CargoPackageReadmeSelectionV1,
) -> Result<SelectedPackageReadme, CargoPackageReadmeFailureV1> {
    let contents = read_selected_readme(root, &path)?;
    Ok(SelectedPackageReadme::Read {
        root_scope,
        path,
        selection,
        contents,
    })
}

fn read_selected_readme(
    package_root: &Path,
    path: &CargoPackageSourcePathV1,
) -> Result<Vec<u8>, CargoPackageReadmeFailureV1> {
    read_source_file_under_limit(
        package_root,
        path,
        backend_library::MAX_CARGO_PACKAGE_README_BYTES,
    )
    .map_err(|error| match error {
        CargoPackageSourceReadFailureV1::FileTooLarge => {
            CargoPackageReadmeFailureV1::ContentTooLarge
        }
        _ => CargoPackageReadmeFailureV1::SelectedFileUnavailable,
    })?
    .ok_or(CargoPackageReadmeFailureV1::SelectedFileUnavailable)
}

fn readme_text(contents: Vec<u8>) -> Result<Box<str>, CargoPackageReadmeFailureV1> {
    let contents =
        String::from_utf8(contents).map_err(|_| CargoPackageReadmeFailureV1::NotUtf8Text)?;
    if contents.as_bytes().contains(&0) {
        return Err(CargoPackageReadmeFailureV1::NotUtf8Text);
    }
    Ok(contents.into_boxed_str())
}

fn package_relative_path(
    path: &Path,
) -> Result<CargoPackageSourcePathV1, CargoPackageReadmeFailureV1> {
    let path = path
        .to_str()
        .ok_or(CargoPackageReadmeFailureV1::InvalidReadmePath)?;
    let path = if std::path::MAIN_SEPARATOR == '/' {
        path.to_owned()
    } else {
        path.replace(std::path::MAIN_SEPARATOR, "/")
    };
    CargoPackageSourcePathV1::new(path).map_err(|_| CargoPackageReadmeFailureV1::InvalidReadmePath)
}

fn manifest_readme_path(
    allowed_root: &Path,
    declared_path: &str,
) -> Result<CargoPackageSourcePathV1, CargoPackageReadmeFailureV1> {
    let path = Path::new(declared_path);
    let relative = if path.is_absolute() {
        path.strip_prefix(allowed_root)
            .map_err(|_| CargoPackageReadmeFailureV1::ReadmeOutsideAuthorizedRoot)?
    } else {
        path
    };
    package_relative_path(relative)
}

fn read_source_file_under(
    package_root: &Path,
    path: &CargoPackageSourcePathV1,
) -> Result<Vec<u8>, CargoPackageSourceReadFailureV1> {
    read_source_file_under_limit(
        package_root,
        path,
        backend_library::MAX_CARGO_PACKAGE_SOURCE_FILE_BYTES,
    )?
    .ok_or(CargoPackageSourceReadFailureV1::FileUnavailable)
}

fn read_source_file_under_limit(
    package_root: &Path,
    path: &CargoPackageSourcePathV1,
    maximum: usize,
) -> Result<Option<Vec<u8>>, CargoPackageSourceReadFailureV1> {
    observation_budget().map_err(|_| CargoPackageSourceReadFailureV1::FileUnavailable)?;
    let mut segments = path.as_str().split('/').peekable();
    let mut directory = DirectoryCapability::open_read_only_source(package_root)
        .map_err(|_| CargoPackageSourceReadFailureV1::FileUnavailable)?;
    while let Some(segment) = segments.next() {
        if segments.peek().is_some() {
            directory = directory
                .open_dir(segment)
                .map_err(|_| CargoPackageSourceReadFailureV1::FileUnavailable)?;
        } else {
            let mut file = match directory.open_file_read(segment) {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(_) => return Err(CargoPackageSourceReadFailureV1::FileUnavailable),
            };
            let metadata = file
                .metadata()
                .map_err(|_| CargoPackageSourceReadFailureV1::FileUnavailable)?;
            if metadata.len() > maximum as u64 {
                return Err(CargoPackageSourceReadFailureV1::FileTooLarge);
            }
            let bytes = read_bounded_file(&mut file, maximum)
                .map_err(|_| CargoPackageSourceReadFailureV1::FileUnavailable)?;
            if bytes.len() > maximum {
                return Err(CargoPackageSourceReadFailureV1::FileTooLarge);
            }
            if !metadata.is_file() || metadata.len() != bytes.len() as u64 {
                return Err(CargoPackageSourceReadFailureV1::FileUnavailable);
            }
            return Ok(Some(bytes));
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

/// Describes where the Cargo.lock bytes used for one metadata answer came
/// from. This is an internal data distinction, not a cryptographic proof or
/// a capability that untrusted callers can use to authenticate a lockfile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CargoMetadataLockOrigin {
    /// Cargo used an existing lockfile selected by its ordinary configuration.
    Observed,
    /// Cargo generated the lockfile only at a private CLI-selected path.
    EphemeralGeneratedCargoLockV1,
}

/// Lockfile facts retained while the exact Cargo metadata query is coherent.
#[derive(Clone, Debug, Eq, PartialEq)]
struct CargoMetadataRun {
    metadata: Vec<u8>,
    host: String,
    tool_witness: [u8; 32],
    no_deps_witness: Option<[u8; 32]>,
    lockfile: Option<String>,
    lockfile_digest: Option<[u8; 32]>,
    lock_origin: Option<CargoMetadataLockOrigin>,
    /// Existing selected lock path, or the Cargo-reported missing source path
    /// whose absence authorized the private lockfile branch.
    lock_path_witness: Option<PathBuf>,
}

impl From<(Vec<u8>, String, [u8; 32])> for CargoMetadataRun {
    fn from((metadata, host, tool_witness): (Vec<u8>, String, [u8; 32])) -> Self {
        Self {
            metadata,
            host,
            tool_witness,
            no_deps_witness: None,
            lockfile: None,
            lockfile_digest: None,
            lock_origin: None,
            lock_path_witness: None,
        }
    }
}

/// The exact owner-side components that guard a cached Cargo tree input.
struct ProjectInputRead {
    input: TreeInput,
    watched: Vec<PathBuf>,
    target_roots: Vec<PathBuf>,
    file_witness: [u8; 32],
    target_witness: [u8; 32],
    lock_origin_witness: [u8; 32],
    tool_witness_reuse: Option<CargoToolWitnessReuse>,
}

impl ProjectInputRead {
    fn witness(&self) -> [u8; 32] {
        compose_cargo_input_witness(
            self.file_witness,
            self.target_witness,
            self.lock_origin_witness,
        )
    }
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
struct CargoConfiguredTool {
    value: String,
    config_path: PathBuf,
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
    file_witness: [u8; 32],
    target_witness: [u8; 32],
    lock_origin_witness: [u8; 32],
    tool_witness_reuse: Option<CargoToolWitnessReuse>,
    lockfile: Option<String>,
    target_roots: Vec<PathBuf>,
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
    observation_witness_for_context(workspace, workspace, files, cached_tool)
}

fn observation_witness_for_context(
    workspace: &Path,
    tool_context: &Path,
    files: &[PathBuf],
    cached_tool: Option<&CargoToolWitnessReuse>,
) -> Result<InputObservation, String> {
    let environment = cargo_environment_witness()?;
    let (tool, reuse) = match current_cargo_tool_witness(tool_context, cached_tool) {
        Ok(tool) => (tool.digest, Some(tool.reuse)),
        Err(error) => (unavailable_tool_witness(error), None),
    };
    let mut observation = observation_witness_with_context(workspace, files, environment, tool)?;
    observation.tool_witness_reuse = reuse;
    Ok(observation)
}

fn strict_observation_witness(
    workspace: &Path,
    tool_context: &Path,
    files: &[PathBuf],
    cached_tool: Option<&CargoToolWitnessReuse>,
) -> Result<InputObservation, String> {
    let environment = cargo_environment_witness()?;
    let tool = current_cargo_tool_witness(tool_context, cached_tool)?;
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
    observation_budget()?;
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
        observation_budget()?;
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
    observation_budget()?;
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
    let bytes = read_bounded_file(&mut file, maximum)
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
        if !is_cargo_environment_witness_name(named.as_ref()) {
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

fn is_cargo_environment_witness_name(name: &str) -> bool {
    name.starts_with("CARGO_")
        || name.starts_with("NUDOX_")
        || name.starts_with("RUSTC_")
        || name.starts_with("RUSTUP_")
        || name.starts_with("SCCACHE_")
        || matches!(
            name,
            "PATH"
                | "HOME"
                | "APPDATA"
                | "XDG_CONFIG_HOME"
                | "RUSTC"
                | "RUSTFLAGS"
                | "RUSTUP_TOOLCHAIN"
                | "CARGO_HOME"
        )
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
        witness_rustc_wrapper_chain(&mut hasher, role, wrapper, workspace, cached, &mut reuse)?;
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
    observation_budget()?;
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
            observation_budget()?;
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
    hash_unix_store_root_controls(&mut hasher, store, &store_metadata);
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

#[cfg(unix)]
fn hash_unix_store_root_controls(
    hasher: &mut blake3::Hasher,
    path: &Path,
    metadata: &std::fs::Metadata,
) {
    use std::os::unix::fs::MetadataExt;
    // The store root changes whenever any Nix object is installed. Bind only
    // its identity and safety controls; hashing size/mtime would invalidate
    // every tool token on unrelated store activity.
    hasher.update(path.as_os_str().as_encoded_bytes());
    for value in [
        metadata.dev(),
        metadata.ino(),
        metadata.mode() as u64,
        metadata.uid() as u64,
        metadata.gid() as u64,
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

fn witness_rustc_wrapper_chain(
    hasher: &mut blake3::Hasher,
    role: &[u8],
    wrapper: &Path,
    workspace: &Path,
    cached: Option<&CargoToolWitnessReuse>,
    reuse: &mut Vec<CargoToolFileReuse>,
) -> Result<(), String> {
    let resolved = wrapper
        .canonicalize()
        .map_err(|_| "Cargo wrapper path cannot be resolved".to_owned())?;
    if resolved.file_name() == Some(std::ffi::OsStr::new("sccache")) {
        let binary = tool_executable_witness(&resolved, cached)?;
        let version = verify_sccache_wrapper(&resolved, workspace)?;
        hasher.update(b"backend.cargo-wrapper.sccache.v1\0");
        hash_tool_role(hasher, role, &resolved, &binary);
        hash_wrapper_version(hasher, &version);
        if let Some(file_reuse) = binary.reuse {
            reuse.push(file_reuse);
        }
        return Ok(());
    }

    let (script, bytes) = tool_script_witness(&resolved)?;
    let (interpreter, sccache) = recognized_nudox_dependency_cache_wrapper(&bytes)
        .ok_or_else(|| "Cargo selected an unrecognized rustc wrapper".to_owned())?;
    let interpreter = interpreter
        .canonicalize()
        .map_err(|_| "rustc cache wrapper interpreter cannot be resolved".to_owned())?;
    let interpreter_witness = tool_executable_witness(&interpreter, cached)?;
    if !is_safe_nix_store_sccache_path(&sccache) {
        return Err("rustc cache wrapper has an unsafe sccache command path".to_owned());
    }
    let canonical_sccache = sccache
        .canonicalize()
        .map_err(|_| "rustc cache wrapper sccache path cannot be resolved".to_owned())?;
    if canonical_sccache != sccache || !is_safe_nix_store_sccache_path(&canonical_sccache) {
        return Err("rustc cache wrapper sccache path is not canonical".to_owned());
    }
    let sccache = canonical_sccache;
    let sccache_witness = tool_executable_witness(&sccache, cached)?;
    if sccache_witness.reuse.is_none() {
        return Err("rustc cache wrapper sccache is not an immutable Nix-store tool".to_owned());
    }
    let version = verify_sccache_wrapper(&sccache, workspace)?;

    hasher.update(b"backend.cargo-wrapper.nudox-dependency-cache.v1\0");
    hash_tool_role(hasher, role, &resolved, &script);
    hash_tool_role(
        hasher,
        b"rustc-wrapper-interpreter",
        &interpreter,
        &interpreter_witness,
    );
    hash_tool_role(hasher, b"rustc-wrapper-sccache", &sccache, &sccache_witness);
    hash_wrapper_version(hasher, &version);
    for witness in [script, interpreter_witness, sccache_witness] {
        if let Some(file_reuse) = witness.reuse {
            reuse.push(file_reuse);
        }
    }
    Ok(())
}

fn hash_wrapper_version(hasher: &mut blake3::Hasher, version: &[u8]) {
    hasher.update(&(version.len() as u64).to_le_bytes());
    hasher.update(version);
}

fn tool_script_witness(path: &Path) -> Result<(CargoToolFileWitness, Vec<u8>), String> {
    const MAX_WRAPPER_SCRIPT_BYTES: u64 = 64 * 1024;
    let resolved = path
        .canonicalize()
        .map_err(|_| "Cargo wrapper path cannot be resolved".to_owned())?;
    let parent = resolved
        .parent()
        .ok_or_else(|| "Cargo wrapper has no parent directory".to_owned())?;
    let name = resolved
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| "Cargo wrapper name is not UTF-8".to_owned())?;
    let directory = DirectoryCapability::open_read_only_source(parent)
        .map_err(|_| "Cargo wrapper directory cannot be held safely".to_owned())?;
    let mut file = directory
        .open_file_read(name)
        .map_err(|_| "Cargo wrapper cannot be opened without following links".to_owned())?;
    let before = file
        .metadata()
        .map_err(|_| "Cargo wrapper metadata cannot be read".to_owned())?;
    if !before.is_file() || before.len() == 0 || before.len() > MAX_WRAPPER_SCRIPT_BYTES {
        return Err("Cargo wrapper is not a bounded regular file".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if before.permissions().mode() & 0o111 == 0 {
            return Err("Cargo wrapper script is not executable".to_owned());
        }
    }
    let bytes = read_bounded_file(&mut file, MAX_WRAPPER_SCRIPT_BYTES as usize)
        .map_err(|_| "Cargo wrapper bytes cannot be read".to_owned())?;
    let after = file
        .metadata()
        .map_err(|_| "Cargo wrapper metadata cannot be rechecked".to_owned())?;
    if !same_tool_file_metadata(&before, &after) || bytes.len() as u64 != before.len() {
        return Err("Cargo wrapper changed while it was observed".to_owned());
    }
    let immutable_identity = immutable_nix_store_file_identity(&resolved, &before);
    if immutable_identity != immutable_nix_store_file_identity(&resolved, &after) {
        return Err("Cargo wrapper immutable identity changed while observed".to_owned());
    }
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-wrapper-script.v1\0");
    hasher.update(resolved.as_os_str().as_encoded_bytes());
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(&bytes);
    let digest = *hasher.finalize().as_bytes();
    let reuse = immutable_identity.map(|immutable_identity| CargoToolFileReuse {
        canonical_path: resolved.clone(),
        immutable_identity,
        content_digest: digest,
    });
    Ok((
        CargoToolFileWitness {
            canonical_path: resolved,
            content_digest: digest,
            reuse,
        },
        bytes,
    ))
}

fn recognized_nudox_dependency_cache_wrapper(script: &[u8]) -> Option<(PathBuf, PathBuf)> {
    const TEMPLATE: &[u8] = include_bytes!("../../../../.config/scripts/cargo-rustc-cache.sh");
    const PLACEHOLDER: &[u8] = b"@sccache@";
    let source_shebang_end = TEMPLATE.iter().position(|byte| *byte == b'\n')? + 1;
    let placeholder = TEMPLATE
        .windows(PLACEHOLDER.len())
        .position(|window| window == PLACEHOLDER)?;
    if placeholder < source_shebang_end {
        return None;
    }
    if TEMPLATE[placeholder + PLACEHOLDER.len()..]
        .windows(PLACEHOLDER.len())
        .any(|window| window == PLACEHOLDER)
    {
        return None;
    }
    if !script.starts_with(b"#!") {
        return None;
    }
    let line_end = script.iter().position(|byte| *byte == b'\n')?;
    let shebang = std::str::from_utf8(script.get(2..line_end)?).ok()?;
    let (interpreter, body, template_start) = if shebang == "/bin/sh" {
        (PathBuf::from("/bin/sh"), script, 0)
    } else {
        // `writeShellScript` can either prepend its Nix Bash shebang or
        // replace the source shebang. Its generated `bash -e` line is admitted
        // only with the exact tracked body; no other arguments are accepted.
        let (interpreter, argument) = shebang
            .split_once(' ')
            .map_or((shebang, None), |(path, argument)| (path, Some(argument)));
        let interpreter = PathBuf::from(interpreter);
        if interpreter == Path::new("/bin/sh")
            || !is_supported_nudox_cache_interpreter(&interpreter)
            || !matches!(argument, None | Some("-e"))
        {
            return None;
        }
        let body = script.get(line_end + 1..)?;
        let template_start = if body.starts_with(&TEMPLATE[..source_shebang_end]) {
            0
        } else {
            source_shebang_end
        };
        (interpreter, body, template_start)
    };
    let prefix = &TEMPLATE[template_start..placeholder];
    let suffix = &TEMPLATE[placeholder + PLACEHOLDER.len()..];
    if !body.starts_with(prefix)
        || !body.ends_with(suffix)
        || body.len() < prefix.len() + suffix.len()
    {
        return None;
    }
    let path_end = body.len() - suffix.len();
    let sccache_bytes = body.get(prefix.len()..path_end)?;
    let sccache = PathBuf::from(std::str::from_utf8(sccache_bytes).ok()?);
    is_safe_nix_store_sccache_path(&sccache).then_some((interpreter, sccache))
}

fn is_supported_nudox_cache_interpreter(path: &Path) -> bool {
    if path.to_str() == Some("/bin/sh") {
        return true;
    }
    let Some(value) = path.to_str() else {
        return false;
    };
    let Some(object_and_tail) = value.strip_prefix("/nix/store/") else {
        return false;
    };
    let mut parts = object_and_tail.split('/');
    let Some(object) = parts.next() else {
        return false;
    };
    is_safe_nix_store_object_name(object)
        && object
            .split_once('-')
            .is_some_and(|(_, package)| package.starts_with("bash-"))
        && parts.next() == Some("bin")
        && parts.next() == Some("bash")
        && parts.next().is_none()
}

fn is_safe_nix_store_sccache_path(path: &Path) -> bool {
    let Some(value) = path.to_str() else {
        return false;
    };
    let Some(object_and_tail) = value.strip_prefix("/nix/store/") else {
        return false;
    };
    let mut parts = object_and_tail.split('/');
    let Some(object) = parts.next() else {
        return false;
    };
    is_safe_nix_store_object_name(object)
        && parts.next() == Some("bin")
        && parts.next() == Some("sccache")
        && parts.next().is_none()
}

fn is_safe_nix_store_object_name(name: &str) -> bool {
    is_nix_store_object_name(name)
        && name.split_once('-').is_some_and(|(_, package)| {
            package
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.+".contains(&byte))
        })
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
    let selected_rustc = cargo_effective_tool_value(
        "RUSTC",
        "CARGO_BUILD_RUSTC",
        "rustc",
        workspace,
        "rustc",
        false,
    )?;
    let inject_default_rustc = selected_rustc.is_none();
    let rustc = selected_rustc
        .or_else(|| {
            cargo
                .parent()
                .map(|directory| directory.join("rustc"))
                .filter(|path| path.is_file())
        })
        .or_else(|| find_executable_on_path(std::ffi::OsStr::new("rustc"), workspace))
        .ok_or_else(|| "the Cargo-selected rustc executable was not found".to_owned())?;
    let rustc_wrapper = cargo_effective_tool_value(
        "RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "rustc-wrapper",
        workspace,
        "rustc wrapper",
        true,
    )?;
    let rustc_workspace_wrapper = cargo_effective_tool_value(
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        "rustc-workspace-wrapper",
        workspace,
        "rustc workspace wrapper",
        true,
    )?;
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

fn cargo_build_tool_value(
    workspace: &Path,
    key: &str,
) -> Result<Option<CargoConfiguredTool>, String> {
    let mut values = Vec::new();
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
        let Some(configured) = cargo_configured_tool(&path, &document, key)? else {
            continue;
        };
        values.push(configured);
    }
    let values = collapse_configured_tool_values(workspace, key, values)?;
    if values.len() > 1 {
        return Err(format!(
            "Cargo config declares conflicting build.{key} values"
        ));
    }
    Ok(values.into_values().next())
}

fn collapse_configured_tool_values(
    workspace: &Path,
    key: &str,
    configured_values: impl IntoIterator<Item = CargoConfiguredTool>,
) -> Result<BTreeMap<PathBuf, CargoConfiguredTool>, String> {
    let mut values = BTreeMap::new();
    for configured in configured_values {
        // Different config origins remain separately witnessed by the Cargo
        // input set. They are not a conflict when Cargo resolves them to the
        // same executable; relative values still use their own origin here.
        let resolved = resolve_cargo_tool_value(
            &configured.value,
            workspace,
            Some(&configured.config_path),
            key,
        )?
        .canonicalize()
        .map_err(|_| format!("Cargo-selected {key} executable cannot be canonicalized"))?;
        values.entry(resolved).or_insert(configured);
    }
    Ok(values)
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

fn cargo_configured_tool(
    config_path: &Path,
    document: &toml::Value,
    key: &str,
) -> Result<Option<CargoConfiguredTool>, String> {
    let Some(value) = cargo_config_build_value(document, key)? else {
        return Ok(None);
    };
    validate_tool_value(value, key)?;
    Ok(Some(CargoConfiguredTool {
        value: value.to_owned(),
        config_path: config_path.to_path_buf(),
    }))
}

fn cargo_config_has_env_tool_override(document: &toml::Value) -> bool {
    document
        .get("env")
        .and_then(toml::Value::as_table)
        .is_some_and(|env| {
            env.keys().any(|key| {
                key.starts_with("CARGO_")
                    || key.starts_with("NUDOX_")
                    || key.starts_with("RUSTC_")
                    || key.starts_with("RUSTUP_")
                    || key.starts_with("SCCACHE_")
                    || matches!(
                        key.as_str(),
                        "RUSTC"
                            | "RUSTDOC"
                            | "RUSTFLAGS"
                            | "RUSTDOCFLAGS"
                            | "CARGO_ENCODED_RUSTFLAGS"
                            | "CARGO_ENCODED_RUSTDOCFLAGS"
                            | "HOME"
                            | "APPDATA"
                            | "XDG_CONFIG_HOME"
                            | "PATH"
                    )
            })
        })
}

fn cargo_effective_tool_value(
    direct_environment: &str,
    config_environment: &str,
    config_key: &str,
    workspace: &Path,
    label: &str,
    direct_empty_disables: bool,
) -> Result<Option<PathBuf>, String> {
    let direct = environment_tool_value(direct_environment)?;
    if direct.is_some() {
        return resolve_effective_tool_value(
            direct,
            None,
            None,
            workspace,
            label,
            direct_empty_disables,
        );
    }
    let configured_environment = environment_tool_value(config_environment)?;
    if configured_environment.is_some() {
        return resolve_effective_tool_value(
            None,
            configured_environment,
            None,
            workspace,
            label,
            direct_empty_disables,
        );
    }
    let config_value = cargo_build_tool_value(workspace, config_key)?;
    resolve_effective_tool_value(
        None,
        None,
        config_value.as_ref(),
        workspace,
        label,
        direct_empty_disables,
    )
}

fn resolve_effective_tool_value(
    direct: Option<String>,
    configured_environment: Option<String>,
    config_value: Option<&CargoConfiguredTool>,
    workspace: &Path,
    label: &str,
    direct_empty_disables: bool,
) -> Result<Option<PathBuf>, String> {
    if let Some(direct) = direct {
        if direct.is_empty() {
            if direct_empty_disables {
                return Ok(None);
            }
            return Err(format!("Cargo {label} selection is empty"));
        }
        return resolve_cargo_tool_value(&direct, workspace, None, label).map(Some);
    }
    if let Some(configured_environment) = configured_environment {
        if configured_environment.is_empty() {
            if direct_empty_disables {
                return Ok(None);
            }
            return Err(format!("Cargo configured {label} selection is empty"));
        }
        return resolve_cargo_tool_value(&configured_environment, workspace, None, label).map(Some);
    }
    config_value
        .map(|configured| {
            resolve_cargo_tool_value(
                &configured.value,
                workspace,
                Some(&configured.config_path),
                label,
            )
        })
        .transpose()
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
    resolve_cargo_tool_value(value, workspace, None, label)
}

fn resolve_cargo_tool_value(
    value: &str,
    workspace: &Path,
    config_path: Option<&Path>,
    label: &str,
) -> Result<PathBuf, String> {
    validate_tool_value(value, label)?;
    let path = PathBuf::from(value);
    let resolved = if path.is_absolute() {
        Some(path)
    } else if path.components().count() > 1 || value.contains(std::path::MAIN_SEPARATOR) {
        let base = match config_path {
            Some(config_path) => config_path
                .parent()
                .and_then(Path::parent)
                .ok_or_else(|| "Cargo config path has no relative-path base".to_owned())?,
            None => workspace,
        };
        Some(base.join(path))
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
    let path = std::env::var_os("PATH");
    path.as_deref()
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
/// The selected Cargo still opens source paths itself: these checks detect
/// ordinary concurrent edits but are not an atomic snapshot or a sandbox
/// against a hostile writer or Cargo executable that mutates and restores a
/// path between samples.
fn read_project(
    requested: &RequestedCargoManifest,
    cached_tool: Option<&CargoToolWitnessReuse>,
) -> Result<ProjectInputRead, String> {
    match coherent_metadata(requested, cached_tool) {
        Ok(observed) => {
            let input = metadata_input_with_stable_source_witness(
                &observed.metadata,
                &observed.host,
                observed.lockfile.as_deref(),
                observed.input_witness,
            )
            .map_err(|error| error.to_string())?;
            if !tree_input_proves_requested_manifest(&input, requested, &observed.watched) {
                return Err(
                    "Cargo metadata did not admit the exact requested package manifest in its resolved workspace".to_owned(),
                );
            }
            Ok(ProjectInputRead {
                input,
                watched: observed.watched,
                target_roots: observed.target_roots,
                file_witness: observed.file_witness,
                target_witness: observed.target_witness,
                lock_origin_witness: observed.lock_origin_witness,
                tool_witness_reuse: observed.tool_witness_reuse,
            })
        }
        Err(reason) => {
            // A package path may be a member, excluded standalone package, or
            // an explicit member of a non-ancestor workspace. Without Cargo's
            // exact-manifest resolution, an ancestor lockfile is not a safe
            // substitute. A manifest that declares its own workspace root is
            // the only cold fallback whose lockfile scope is unambiguous.
            if !requested.has_workspace {
                return Err(reason);
            }
            observation_budget()?;
            let workspace = &requested.root;
            let default_lockfile = workspace.join("Cargo.lock");
            let selected_lockfile =
                cargo_selected_lockfile_path(workspace, workspace).map_err(|error| {
                    format!("{reason}; cannot safely identify the selected lockfile: {error}")
                })?;
            if selected_lockfile != default_lockfile {
                return Err(format!(
                    "{reason}; refusing lockfile-only fallback because Cargo selects {} instead of {}",
                    selected_lockfile.display(),
                    default_lockfile.display()
                ));
            }
            let mut watched = basic_input_paths(workspace)?;
            watched.extend(cargo_config_paths(workspace)?);
            watched.extend(sccache_configuration_paths(workspace)?);
            watched.extend(rustup_selection_paths(workspace)?);
            watched.sort();
            watched.dedup();
            let observed =
                observation_witness_for_context(workspace, workspace, &watched, cached_tool)?;
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
            if !tree_input_proves_requested_manifest(&input, requested, &watched) {
                return Err(reason);
            }
            let lock_origin_witness = cargo_lock_origin_witness(
                CargoMetadataLockOrigin::Observed,
                &workspace.join("Cargo.lock"),
                *blake3::hash(lockfile.as_bytes()).as_bytes(),
            );
            Ok(ProjectInputRead {
                input,
                watched,
                target_roots: Vec::new(),
                file_witness: observed.digest,
                target_witness: [0; 32],
                lock_origin_witness,
                tool_witness_reuse: observed.tool_witness_reuse,
            })
        }
    }
}

fn coherent_metadata(
    requested: &RequestedCargoManifest,
    cached_tool: Option<&CargoToolWitnessReuse>,
) -> Result<CoherentMetadata, String> {
    let mut session = CargoMetadataResolutionSession::default();
    let mut tool_reuse = cached_tool.cloned();
    coherent_metadata_with(
        requested,
        |manifest| {
            if manifest != requested.manifest {
                return Err("Cargo metadata changed the exact requested manifest".to_owned());
            }
            session.run(requested, cached_tool)
        },
        |workspace, files| {
            let observation =
                strict_observation_witness(workspace, &requested.root, files, tool_reuse.as_ref())?;
            tool_reuse = observation.tool_witness_reuse.clone();
            Ok(observation)
        },
    )
}

fn coherent_metadata_with<R: Into<CargoMetadataRun>>(
    requested: &RequestedCargoManifest,
    mut run_metadata: impl FnMut(&Path) -> Result<R, String>,
    mut observe: impl FnMut(&Path, &[PathBuf]) -> Result<InputObservation, String>,
) -> Result<CoherentMetadata, String> {
    observation_budget()?;
    let discovery = run_metadata(&requested.manifest)?.into();
    observation_budget()?;
    let discovery_workspace = metadata_proves_requested_manifest(&discovery.metadata, requested)?;
    let mut discovery_paths =
        metadata_observation_paths(&requested.root, &discovery_workspace, &discovery.metadata)?;
    if let Some(lock_path) = &discovery.lock_path_witness {
        add_observed_path(&mut discovery_paths, lock_path)?;
    }
    let required_manifests = metadata_required_manifests(
        &discovery.metadata,
        &requested.manifest,
        &discovery_workspace,
    )?;
    let required_registry_checksums = metadata_required_registry_checksums(&discovery.metadata)?;
    let target_roots = metadata_target_roots(&discovery.metadata)?;
    let discovery_target_witness = cargo_auto_target_membership_witness(&target_roots)?;
    let before = observe(&discovery_workspace, &discovery_paths)?;
    observation_budget()?;
    require_required_manifests_present(&before, &required_manifests)?;
    require_required_manifests_present(&before, &required_registry_checksums)?;
    require_lock_path_state(&before, &discovery)?;

    let observed = run_metadata(&requested.manifest)?.into();
    observation_budget()?;
    if observed.metadata != discovery.metadata
        || observed.host != discovery.host
        || observed.tool_witness != discovery.tool_witness
        || observed.no_deps_witness != discovery.no_deps_witness
        || observed.lockfile != discovery.lockfile
        || observed.lockfile_digest != discovery.lockfile_digest
        || observed.lock_origin != discovery.lock_origin
        || observed.lock_path_witness != discovery.lock_path_witness
    {
        return Err("Cargo metadata inputs or tool changed between observation passes".to_owned());
    }
    let effective_workspace = metadata_proves_requested_manifest(&observed.metadata, requested)?;
    if effective_workspace != discovery_workspace {
        return Err("Cargo effective workspace changed between observation passes".to_owned());
    }
    let mut watched =
        metadata_observation_paths(&requested.root, &effective_workspace, &observed.metadata)?;
    if let Some(lock_path) = &observed.lock_path_witness {
        add_observed_path(&mut watched, lock_path)?;
    }
    watched.sort();
    watched.dedup();
    if watched != discovery_paths {
        return Err("Cargo metadata input set changed during observation".to_owned());
    }
    let after = observe(&effective_workspace, &watched)?;
    observation_budget()?;
    require_required_manifests_present(&after, &required_manifests)?;
    require_required_manifests_present(&after, &required_registry_checksums)?;
    require_lock_path_state(&after, &observed)?;
    if before.digest != after.digest {
        return Err(
            "Cargo manifests, lockfile, configuration, or tools changed during metadata".to_owned(),
        );
    }
    let target_witness =
        cargo_auto_target_membership_witness(&metadata_target_roots(&observed.metadata)?)?;
    if discovery_target_witness != target_witness {
        return Err("Cargo automatic target directories changed during metadata".to_owned());
    }
    let lock_origin_witness = lock_origin_witness_for_run(&observed)?;
    let input_witness =
        compose_cargo_input_witness(after.digest, target_witness, lock_origin_witness);
    Ok(CoherentMetadata {
        metadata: observed.metadata,
        host: observed.host,
        input_witness,
        file_witness: after.digest,
        target_witness,
        lock_origin_witness,
        tool_witness_reuse: after.tool_witness_reuse,
        lockfile: observed.lockfile.or(after.lockfile),
        target_roots,
        watched,
    })
}

fn metadata_observation_paths(
    request_context: &Path,
    workspace: &Path,
    metadata: &[u8],
) -> Result<Vec<PathBuf>, String> {
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
        let source = package.get("source").and_then(serde_json::Value::as_str);
        if source
            .is_some_and(|source| source.starts_with("registry+") || source.starts_with("sparse+"))
        {
            let package_root = path
                .parent()
                .ok_or_else(|| "Cargo registry manifest has no package root".to_owned())?;
            paths.push(package_root.join(".cargo-checksum.json"));
        }
    }
    // Cargo was invoked from the exact requested manifest's directory. Keep
    // its config search path, and the effective workspace's lock/manifest,
    // inside the same stable witness even for non-ancestor workspaces.
    paths.extend(cargo_config_paths(request_context)?);
    paths.extend(cargo_config_paths(workspace)?);
    paths.extend(sccache_configuration_paths(request_context)?);
    paths.extend(sccache_configuration_paths(workspace)?);
    paths.extend(rustup_selection_paths(request_context)?);
    paths.extend(rustup_selection_paths(workspace)?);
    paths.sort();
    paths.dedup();
    if paths.len() > MAX_CARGO_OBSERVATION_PATHS {
        return Err("Cargo metadata input set exceeds the observation limit".to_owned());
    }
    Ok(paths)
}

fn metadata_required_registry_checksums(metadata: &[u8]) -> Result<Vec<PathBuf>, String> {
    if metadata.len() > MAX_METADATA_BYTES {
        return Err("Cargo metadata exceeded its bounded response size".to_owned());
    }
    let value: serde_json::Value = serde_json::from_slice(metadata)
        .map_err(|error| format!("Cargo metadata JSON is malformed: {error}"))?;
    let packages = value
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Cargo metadata has no package array".to_owned())?;
    if packages.len() > MAX_CARGO_TARGET_MEMBERSHIP_ROOTS {
        return Err("Cargo metadata package set exceeds the observation limit".to_owned());
    }
    let mut checksums = BTreeSet::new();
    for package in packages {
        let Some(source) = package.get("source").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !source.starts_with("registry+") && !source.starts_with("sparse+") {
            continue;
        }
        let manifest = package
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Cargo registry package omitted manifest_path".to_owned())?;
        let manifest = PathBuf::from(manifest);
        if !manifest.is_absolute()
            || manifest.file_name().and_then(std::ffi::OsStr::to_str) != Some("Cargo.toml")
            || manifest.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err("Cargo registry package returned a noncanonical manifest path".to_owned());
        }
        let package_root = manifest
            .parent()
            .ok_or_else(|| "Cargo registry manifest has no package root".to_owned())?;
        checksums.insert(package_root.join(".cargo-checksum.json"));
    }
    Ok(checksums.into_iter().collect())
}

fn metadata_target_roots(metadata: &[u8]) -> Result<Vec<PathBuf>, String> {
    if metadata.len() > MAX_METADATA_BYTES {
        return Err("Cargo metadata exceeded its bounded response size".to_owned());
    }
    let value: serde_json::Value = serde_json::from_slice(metadata)
        .map_err(|error| format!("Cargo metadata JSON is malformed: {error}"))?;
    let packages = value
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Cargo metadata has no package array".to_owned())?;
    if packages.len() > MAX_CARGO_TARGET_MEMBERSHIP_ROOTS {
        return Err("Cargo metadata package set exceeds the target-root limit".to_owned());
    }
    let mut roots = BTreeSet::new();
    for package in packages {
        let manifest = package
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "Cargo metadata package omitted manifest_path".to_owned())?;
        let manifest = PathBuf::from(manifest);
        if !manifest.is_absolute()
            || manifest.file_name().and_then(std::ffi::OsStr::to_str) != Some("Cargo.toml")
            || manifest.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err("Cargo metadata returned a noncanonical target manifest".to_owned());
        }
        let root = manifest
            .parent()
            .ok_or_else(|| "Cargo metadata package manifest has no parent".to_owned())?;
        roots.insert(root.to_path_buf());
    }
    Ok(roots.into_iter().collect())
}

/// Captures only Cargo auto-target candidate directories. The bounded
/// no-follow membership snapshot catches a newly added `src/bin`, example,
/// test, or benchmark target without reimplementing Cargo's target rules.
fn cargo_auto_target_membership_witness(roots: &[PathBuf]) -> Result<[u8; 32], String> {
    if roots.len() > MAX_CARGO_TARGET_MEMBERSHIP_ROOTS {
        return Err("Cargo automatic target root set exceeds its limit".to_owned());
    }
    if roots.is_empty() {
        return Ok([0; 32]);
    }
    let mut roots = roots.to_vec();
    roots.sort();
    roots.dedup();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-auto-target-directory-membership.v1\0");
    let mut total_entries = 0_usize;
    for root in roots {
        observation_budget()?;
        if !root.is_absolute()
            || root.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err("Cargo automatic target root is not a canonical absolute path".to_owned());
        }
        let path = root.as_os_str().as_encoded_bytes();
        hasher.update(&(path.len() as u64).to_le_bytes());
        hasher.update(path);
        let directory = match DirectoryCapability::open_read_only_source(&root) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                hasher.update(&[0]);
                continue;
            }
            Err(error) => return Err(format!("cannot hold Cargo target package root: {error}")),
        };
        hasher.update(&[1]);
        let entries = directory
            .entries(MAX_SOURCE_DIRECTORY_ENTRIES)
            .map_err(|error| format!("cannot enumerate Cargo target package root: {error}"))?;
        account_target_entries(&mut total_entries, entries.len())?;
        for entry in &entries {
            hash_target_entry(&mut hasher, b".", entry);
            let name = entry.name.as_encoded_bytes();
            if name == b"build.rs" && entry.kind != EntryKind::File {
                return Err("Cargo build.rs is linked or is not a regular file".to_owned());
            }
            if name == b"src" {
                match entry.kind {
                    EntryKind::Directory => {
                        let name = entry.name.to_str().ok_or_else(|| {
                            "Cargo automatic target directory name is not UTF-8".to_owned()
                        })?;
                        let src = directory.open_dir(name).map_err(|error| {
                            format!("cannot open Cargo src target directory safely: {error}")
                        })?;
                        scan_src_target_directory(
                            &src,
                            b"src",
                            &mut hasher,
                            &mut total_entries,
                            1,
                        )?;
                    }
                    EntryKind::Link | EntryKind::Special => {
                        return Err("Cargo src target directory is linked or special".to_owned());
                    }
                    EntryKind::File => {}
                }
            } else if name == b"examples" || name == b"tests" || name == b"benches" {
                match entry.kind {
                    EntryKind::Directory => {
                        let name = entry.name.to_str().ok_or_else(|| {
                            "Cargo automatic target directory name is not UTF-8".to_owned()
                        })?;
                        let child = directory.open_dir(name).map_err(|error| {
                            format!("cannot open Cargo automatic target directory safely: {error}")
                        })?;
                        scan_target_directory(
                            &child,
                            name.as_bytes(),
                            &mut hasher,
                            &mut total_entries,
                            1,
                        )?;
                    }
                    EntryKind::Link | EntryKind::Special => {
                        return Err(
                            "Cargo automatic target directory is linked or special".to_owned()
                        );
                    }
                    EntryKind::File => {}
                }
            }
        }
    }
    Ok(*hasher.finalize().as_bytes())
}

fn account_target_entries(total: &mut usize, count: usize) -> Result<(), String> {
    *total = total.saturating_add(count);
    if *total > MAX_CARGO_TARGET_MEMBERSHIP_ENTRIES {
        return Err("Cargo automatic target directory scan exceeded its entry limit".to_owned());
    }
    observation_budget()
}

fn hash_target_entry(
    hasher: &mut blake3::Hasher,
    parent: &[u8],
    entry: &backend_platform::directory::DirectoryEntry,
) {
    let name = entry.name.as_encoded_bytes();
    hasher.update(&(parent.len() as u64).to_le_bytes());
    hasher.update(parent);
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name);
    hasher.update(&[match entry.kind {
        EntryKind::File => 1,
        EntryKind::Directory => 2,
        EntryKind::Link => 3,
        EntryKind::Special => 4,
    }]);
}

fn scan_src_target_directory(
    directory: &DirectoryCapability,
    prefix: &[u8],
    hasher: &mut blake3::Hasher,
    total_entries: &mut usize,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_CARGO_TARGET_MEMBERSHIP_DEPTH {
        return Err("Cargo src/bin target scan exceeded its depth limit".to_owned());
    }
    let entries = directory
        .entries(MAX_SOURCE_DIRECTORY_ENTRIES)
        .map_err(|error| format!("cannot enumerate Cargo src target directory: {error}"))?;
    account_target_entries(total_entries, entries.len())?;
    for entry in &entries {
        hash_target_entry(hasher, prefix, entry);
        let name = entry.name.as_encoded_bytes();
        if (name == b"main.rs" || name == b"lib.rs") && entry.kind != EntryKind::File {
            return Err("Cargo src target file is linked or is not regular".to_owned());
        }
        if name == b"bin" {
            match entry.kind {
                EntryKind::Directory => {
                    let name = entry.name.to_str().ok_or_else(|| {
                        "Cargo src/bin target directory name is not UTF-8".to_owned()
                    })?;
                    let child = directory.open_dir(name).map_err(|error| {
                        format!("cannot open Cargo src/bin directory safely: {error}")
                    })?;
                    let mut child_prefix = prefix.to_vec();
                    child_prefix.extend_from_slice(b"/bin");
                    scan_target_directory(&child, &child_prefix, hasher, total_entries, depth + 1)?;
                }
                EntryKind::Link | EntryKind::Special => {
                    return Err("Cargo src/bin directory is linked or special".to_owned());
                }
                EntryKind::File => {}
            }
        }
    }
    Ok(())
}

fn scan_target_directory(
    directory: &DirectoryCapability,
    prefix: &[u8],
    hasher: &mut blake3::Hasher,
    total_entries: &mut usize,
    depth: usize,
) -> Result<(), String> {
    if depth > MAX_CARGO_TARGET_MEMBERSHIP_DEPTH {
        return Err("Cargo automatic target scan exceeded its depth limit".to_owned());
    }
    let entries = directory
        .entries(MAX_SOURCE_DIRECTORY_ENTRIES)
        .map_err(|error| format!("cannot enumerate Cargo automatic target directory: {error}"))?;
    account_target_entries(total_entries, entries.len())?;
    for entry in &entries {
        hash_target_entry(hasher, prefix, entry);
        match entry.kind {
            EntryKind::Directory => {
                let name = entry.name.to_str().ok_or_else(|| {
                    "Cargo automatic target subdirectory name is not UTF-8".to_owned()
                })?;
                let child = directory.open_dir(name).map_err(|error| {
                    format!("cannot open Cargo automatic target subdirectory safely: {error}")
                })?;
                let mut child_prefix = prefix.to_vec();
                child_prefix.push(b'/');
                child_prefix.extend_from_slice(entry.name.as_encoded_bytes());
                scan_target_directory(&child, &child_prefix, hasher, total_entries, depth + 1)?;
            }
            EntryKind::Link | EntryKind::Special => {
                return Err(
                    "Cargo automatic target tree contains a link or special file".to_owned(),
                );
            }
            EntryKind::File => {}
        }
    }
    Ok(())
}

fn metadata_required_manifests(
    metadata: &[u8],
    requested_manifest: &Path,
    workspace: &Path,
) -> Result<Vec<PathBuf>, String> {
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
    required.insert(requested_manifest.to_path_buf());
    required.insert(workspace.join("Cargo.toml"));
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

fn add_observed_path(paths: &mut Vec<PathBuf>, path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.file_name().and_then(std::ffi::OsStr::to_str) != Some("Cargo.lock")
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err("Cargo selected a noncanonical lockfile path".to_owned());
    }
    paths.push(path.to_path_buf());
    paths.sort();
    paths.dedup();
    if paths.len() > MAX_CARGO_OBSERVATION_PATHS {
        return Err("Cargo metadata input set exceeds the observation limit".to_owned());
    }
    Ok(())
}

fn require_lock_path_state(
    observation: &InputObservation,
    run: &CargoMetadataRun,
) -> Result<(), String> {
    let Some(origin) = run.lock_origin else {
        return Ok(());
    };
    let path = run
        .lock_path_witness
        .as_ref()
        .ok_or_else(|| "Cargo lockfile provenance omitted its selected path".to_owned())?;
    let missing = observation
        .missing_path_keys
        .contains(&observation_path_key(path));
    let digest = run
        .lockfile
        .as_ref()
        .map(|lockfile| *blake3::hash(lockfile.as_bytes()).as_bytes())
        .ok_or_else(|| "Cargo lockfile provenance omitted its bytes".to_owned())?;
    if Some(digest) != run.lockfile_digest {
        return Err("Cargo lockfile provenance digest does not match its bytes".to_owned());
    }
    match origin {
        CargoMetadataLockOrigin::Observed if missing => {
            Err("Cargo's selected observed lockfile disappeared during metadata".to_owned())
        }
        CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1 if !missing => {
            Err("the Cargo-selected source lockfile appeared during private resolution".to_owned())
        }
        CargoMetadataLockOrigin::Observed
        | CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1 => Ok(()),
    }
}

fn lock_origin_witness_for_run(run: &CargoMetadataRun) -> Result<[u8; 32], String> {
    let Some(origin) = run.lock_origin else {
        return Ok([0; 32]);
    };
    let path = run
        .lock_path_witness
        .as_ref()
        .ok_or_else(|| "Cargo lockfile provenance omitted its selected path".to_owned())?;
    let digest = run
        .lockfile_digest
        .ok_or_else(|| "Cargo lockfile provenance omitted its digest".to_owned())?;
    Ok(cargo_lock_origin_witness(origin, path, digest))
}

fn cargo_lock_origin_witness(
    origin: CargoMetadataLockOrigin,
    path: &Path,
    content_digest: [u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-lock-origin.v1\0");
    hasher.update(&[match origin {
        CargoMetadataLockOrigin::Observed => 1,
        CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1 => 2,
    }]);
    let path = path.as_os_str().as_encoded_bytes();
    hasher.update(&(path.len() as u64).to_le_bytes());
    hasher.update(path);
    hasher.update(&content_digest);
    *hasher.finalize().as_bytes()
}

fn compose_cargo_input_witness(
    file_witness: [u8; 32],
    target_witness: [u8; 32],
    lock_origin_witness: [u8; 32],
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.cargo-source-input-composition.v1\0");
    hasher.update(&file_witness);
    hasher.update(&target_witness);
    hasher.update(&lock_origin_witness);
    *hasher.finalize().as_bytes()
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
        observation_budget()?;
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

/// Reads only the exact requested directory's manifest. This records which
/// Cargo metadata request to make; it does not infer workspace membership.
fn requested_cargo_manifest(root: &Path) -> Result<Option<RequestedCargoManifest>, String> {
    let requested_manifest = root.join("Cargo.toml");
    let Some(requested_bytes) = read_observation_file(&requested_manifest, MAX_CARGO_CONFIG_BYTES)?
    else {
        return Ok(None);
    };
    let requested_document = cargo_manifest_document(&requested_bytes).ok_or_else(|| {
        format!(
            "Cargo.toml at requested project root {} is not valid UTF-8 TOML",
            requested_manifest.display()
        )
    })?;
    if !cargo_manifest_has_project_scope(&requested_document) {
        return Ok(None);
    }

    let canonical_root = root
        .canonicalize()
        .map_err(|error| format!("cannot resolve Cargo project root: {error}"))?;
    Ok(Some(RequestedCargoManifest {
        manifest: canonical_root.join("Cargo.toml"),
        root: canonical_root,
        has_package: requested_document
            .get("package")
            .and_then(toml::Value::as_table)
            .is_some(),
        has_workspace: cargo_manifest_has_workspace(&requested_document),
    }))
}

/// Confirms that Cargo's exact-manifest metadata made the requested package a
/// workspace member, or that a package-less request is the actual virtual
/// workspace root. Workspace globs, exclusions, and `[package].workspace`
/// paths are all resolved by Cargo rather than locally reimplemented here.
fn metadata_proves_requested_manifest(
    metadata: &[u8],
    requested: &RequestedCargoManifest,
) -> Result<PathBuf, String> {
    if metadata.len() > MAX_METADATA_BYTES {
        return Err("Cargo metadata exceeded its bounded response size".to_owned());
    }
    let value: serde_json::Value = serde_json::from_slice(metadata)
        .map_err(|error| format!("Cargo metadata JSON is malformed: {error}"))?;
    let workspace_text = value
        .get("workspace_root")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "Cargo metadata omitted workspace_root".to_owned())?;
    let workspace = PathBuf::from(workspace_text);
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|_| "Cargo metadata returned a missing workspace root".to_owned())?;
    if !workspace.is_absolute()
        || workspace_text.contains("//")
        || workspace.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
        || canonical_workspace != workspace
    {
        return Err("Cargo metadata returned a noncanonical workspace root".to_owned());
    }

    let packages = value
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Cargo metadata has no package array".to_owned())?;
    if packages.len() > 20_000 {
        return Err("Cargo metadata package set exceeds the observation limit".to_owned());
    }
    let members = value
        .get("workspace_members")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "Cargo metadata has no workspace member array".to_owned())?;
    if members.len() > 20_000 {
        return Err("Cargo metadata workspace member set exceeds the observation limit".to_owned());
    }
    let requested_manifest = requested
        .manifest
        .to_str()
        .ok_or_else(|| "requested Cargo manifest path is not UTF-8".to_owned())?;
    let requested_is_member = packages.iter().any(|package| {
        package
            .get("manifest_path")
            .and_then(serde_json::Value::as_str)
            == Some(requested_manifest)
            && package
                .get("id")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|id| members.iter().any(|member| member.as_str() == Some(id)))
    });

    if requested.has_package && !requested_is_member {
        return Err(
            "Cargo metadata did not admit the exact requested package manifest as a workspace member".to_owned(),
        );
    }
    if requested.has_workspace && workspace != requested.root {
        return Err(
            "Cargo resolved the requested workspace manifest to a different workspace root"
                .to_owned(),
        );
    }
    if !requested.has_package && !requested.has_workspace {
        return Err("requested Cargo manifest declares no project scope".to_owned());
    }
    if !requested.has_package && workspace != requested.root {
        return Err(
            "Cargo metadata did not resolve the exact requested virtual workspace root".to_owned(),
        );
    }
    Ok(workspace)
}

fn tree_input_proves_requested_manifest(
    input: &TreeInput,
    requested: &RequestedCargoManifest,
    watched: &[PathBuf],
) -> bool {
    if !watched.iter().any(|path| path == &requested.manifest) {
        return false;
    }
    let effective_workspace = Path::new(&input.root);
    if !effective_workspace.is_absolute() {
        return false;
    }
    if requested.has_package {
        input.packages.iter().any(|package| {
            package.member && package.source_root.as_deref() == Some(requested.root.as_path())
        })
    } else {
        requested.has_workspace && effective_workspace == requested.root
    }
}

fn cargo_manifest_document(bytes: &[u8]) -> Option<toml::Value> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn cargo_manifest_has_project_scope(document: &toml::Value) -> bool {
    cargo_manifest_has_workspace(document)
        || document
            .get("package")
            .and_then(toml::Value::as_table)
            .is_some()
}

fn cargo_manifest_has_workspace(document: &toml::Value) -> bool {
    document
        .get("workspace")
        .and_then(toml::Value::as_table)
        .is_some()
}

enum CargoMetadataLockState {
    Observed {
        path: PathBuf,
        lockfile: String,
        digest: [u8; 32],
    },
    Ephemeral {
        _directory: tempfile::TempDir,
        private_path: PathBuf,
        missing_source_path: PathBuf,
        lockfile: String,
        digest: [u8; 32],
    },
}

#[derive(Default)]
struct CargoMetadataResolutionSession {
    no_deps_witness: Option<[u8; 32]>,
    lock_state: Option<CargoMetadataLockState>,
}

impl CargoMetadataResolutionSession {
    /// Runs Cargo for the exact requested manifest. A missing source lock is
    /// recognized only from Cargo's own `--locked` diagnostic; the only
    /// unlocked full metadata pass is redirected by CLI config to a private
    /// RAII directory, whose lock is then proved with `--locked`. The stable
    /// CLI-config behavior is supported only for Cargo 1.97 or newer and was
    /// exercised against the pinned 1.97.1 binary. Cargo remains a trusted
    /// executable in this design; path checks are not a process sandbox.
    fn run(
        &mut self,
        requested: &RequestedCargoManifest,
        cached: Option<&CargoToolWitnessReuse>,
    ) -> Result<CargoMetadataRun, String> {
        let requested_manifest = &requested.manifest;
        let request_context = requested_manifest
            .parent()
            .ok_or_else(|| "requested Cargo manifest has no parent directory".to_owned())?;
        let environment_before = cargo_environment_witness()?;
        let cargo = selected_cargo_program(request_context)?;
        let version = run(&cargo, request_context, &["-vV"], 64 * 1024)?;
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
        let selection_before = cargo_tool_selection(&cargo, request_context, &version)?;
        let tool_before =
            metadata_tool_witness(request_context, &version, &selection_before, cached)?;

        let no_deps = run_cargo_metadata_query(
            &cargo,
            request_context,
            &selection_before,
            &host,
            requested_manifest,
            true,
            false,
            None,
        )?;
        let no_deps_workspace = metadata_proves_requested_manifest(&no_deps, requested)?;
        let no_deps_witness = *blake3::hash(&no_deps).as_bytes();
        if self
            .no_deps_witness
            .is_some_and(|previous| previous != no_deps_witness)
        {
            return Err("Cargo no-deps workspace membership changed between passes".to_owned());
        }
        self.no_deps_witness = Some(no_deps_witness);

        // Resolve the existing lock path before the full query when Cargo's
        // ordinary inputs are unambiguous. This lets the successful --locked
        // path prove the same lock bytes were present on both sides of Cargo.
        // If the effective configuration is ambiguous, an exact missing-lock
        // diagnostic can still authorize the private-lock path below; a
        // successful query will be refused because its lock cannot be safely
        // bound to an observed path.
        let selected_lock_before =
            match cargo_selected_lockfile_path(request_context, &no_deps_workspace) {
                Ok(path) => Some((
                    path.clone(),
                    read_observation_file(&path, MAX_CARGO_OBSERVATION_FILE_BYTES)?,
                )),
                Err(_) => None,
            };

        let metadata = if self.lock_state.is_none() {
            match run_cargo_metadata_query(
                &cargo,
                request_context,
                &selection_before,
                &host,
                requested_manifest,
                false,
                true,
                None,
            ) {
                Ok(metadata) => {
                    let workspace = metadata_proves_requested_manifest(&metadata, requested)?;
                    if workspace != no_deps_workspace {
                        return Err(
                            "Cargo effective workspace changed between no-deps and full metadata"
                                .to_owned(),
                        );
                    }
                    let (path_before, bytes_before) = selected_lock_before
                        .as_ref()
                        .ok_or_else(|| "Cargo used an existing lockfile whose configured path could not be proven".to_owned())?;
                    let path = cargo_selected_lockfile_path(request_context, &workspace)?;
                    if &path != path_before {
                        return Err(
                            "Cargo selected lockfile path changed during metadata".to_owned()
                        );
                    }
                    let bytes_before = bytes_before.as_ref().ok_or_else(|| {
                        "Cargo succeeded with --locked although its selected lockfile was absent before metadata".to_owned()
                    })?;
                    let (lockfile, digest) = read_required_cargo_lockfile(&path)?;
                    if lockfile.as_bytes() != bytes_before.as_slice() {
                        return Err("Cargo's selected lockfile changed during metadata".to_owned());
                    }
                    self.lock_state = Some(CargoMetadataLockState::Observed {
                        path,
                        lockfile,
                        digest,
                    });
                    metadata
                }
                Err(error) => {
                    let Some(missing_source_path) = missing_cargo_lockfile_path(&error) else {
                        return Err(error);
                    };
                    if read_observation_file(
                        &missing_source_path,
                        MAX_CARGO_OBSERVATION_FILE_BYTES,
                    )?
                    .is_some()
                    {
                        return Err(format!(
                            "Cargo reported a missing lockfile that is present: {}",
                            missing_source_path.display()
                        ));
                    }
                    if let Some((selected_path, selected_bytes)) = &selected_lock_before {
                        if selected_path != &missing_source_path {
                            return Err("Cargo's missing-lock diagnostic disagreed with the observed lock path".to_owned());
                        }
                        if selected_bytes.is_some() {
                            return Err("Cargo reported a missing lockfile that was present before metadata".to_owned());
                        }
                    }
                    if !cargo_supports_resolver_lockfile_path(&version)? {
                        return Err(format!(
                            "{error}; selected Cargo does not support stable private lockfile redirection"
                        ));
                    }
                    let mut source_roots = vec![requested.root.clone(), no_deps_workspace.clone()];
                    source_roots.extend(metadata_target_roots(&no_deps)?);
                    let (directory, private_directory) =
                        create_private_cargo_lock_directory(&source_roots)?;
                    let private_path = private_directory.join("Cargo.lock");
                    validate_cargo_lockfile_path(&private_path)?;
                    let generated = run_cargo_metadata_query(
                        &cargo,
                        request_context,
                        &selection_before,
                        &host,
                        requested_manifest,
                        false,
                        false,
                        Some(&private_path),
                    )?;
                    if metadata_proves_requested_manifest(&generated, requested)?
                        != metadata_proves_requested_manifest(&no_deps, requested)?
                    {
                        return Err(
                            "Cargo workspace changed during private lockfile generation".to_owned()
                        );
                    }
                    let (lockfile, digest) = read_required_cargo_lockfile(&private_path)?;
                    if read_observation_file(
                        &missing_source_path,
                        MAX_CARGO_OBSERVATION_FILE_BYTES,
                    )?
                    .is_some()
                    {
                        return Err(
                            "the Cargo-selected source lock appeared during private resolution"
                                .to_owned(),
                        );
                    }
                    let locked = run_cargo_metadata_query(
                        &cargo,
                        request_context,
                        &selection_before,
                        &host,
                        requested_manifest,
                        false,
                        true,
                        Some(&private_path),
                    )?;
                    if metadata_proves_requested_manifest(&locked, requested)? != no_deps_workspace
                    {
                        return Err(
                            "Cargo workspace changed during private locked metadata".to_owned()
                        );
                    }
                    let (locked_file, locked_digest) = read_required_cargo_lockfile(&private_path)?;
                    if generated != locked || lockfile != locked_file || digest != locked_digest {
                        return Err(
                            "private Cargo lockfile did not reproduce locked metadata".to_owned()
                        );
                    }
                    if read_observation_file(
                        &missing_source_path,
                        MAX_CARGO_OBSERVATION_FILE_BYTES,
                    )?
                    .is_some()
                    {
                        return Err(
                            "the Cargo-selected source lock appeared during private resolution"
                                .to_owned(),
                        );
                    }
                    self.lock_state = Some(CargoMetadataLockState::Ephemeral {
                        _directory: directory,
                        private_path,
                        missing_source_path,
                        lockfile,
                        digest,
                    });
                    locked
                }
            }
        } else {
            match self.lock_state.as_ref().expect("lock state was checked") {
                CargoMetadataLockState::Observed {
                    path,
                    lockfile,
                    digest,
                } => {
                    let selected_path =
                        cargo_selected_lockfile_path(request_context, &no_deps_workspace)?;
                    if &selected_path != path {
                        return Err(
                            "Cargo selected lockfile path changed between metadata passes"
                                .to_owned(),
                        );
                    }
                    let (current, current_digest) = read_required_cargo_lockfile(path)?;
                    if &current != lockfile || &current_digest != digest {
                        return Err(
                            "Cargo's selected lockfile changed between metadata passes".to_owned()
                        );
                    }
                    let metadata = run_cargo_metadata_query(
                        &cargo,
                        request_context,
                        &selection_before,
                        &host,
                        requested_manifest,
                        false,
                        true,
                        None,
                    )?;
                    if metadata_proves_requested_manifest(&metadata, requested)?
                        != no_deps_workspace
                    {
                        return Err(
                            "Cargo effective workspace changed during locked metadata".to_owned()
                        );
                    }
                    let (after, after_digest) = read_required_cargo_lockfile(path)?;
                    if &after != lockfile || &after_digest != digest {
                        return Err("Cargo's selected lockfile changed during metadata".to_owned());
                    }
                    metadata
                }
                CargoMetadataLockState::Ephemeral {
                    private_path,
                    missing_source_path,
                    lockfile,
                    digest,
                    ..
                } => {
                    if read_observation_file(missing_source_path, MAX_CARGO_OBSERVATION_FILE_BYTES)?
                        .is_some()
                    {
                        return Err(
                            "the Cargo-selected source lock appeared during private resolution"
                                .to_owned(),
                        );
                    }
                    let (current, current_digest) = read_required_cargo_lockfile(private_path)?;
                    if &current != lockfile || &current_digest != digest {
                        return Err(
                            "private Cargo lockfile changed between metadata passes".to_owned()
                        );
                    }
                    let metadata = run_cargo_metadata_query(
                        &cargo,
                        request_context,
                        &selection_before,
                        &host,
                        requested_manifest,
                        false,
                        true,
                        Some(private_path),
                    )?;
                    if metadata_proves_requested_manifest(&metadata, requested)?
                        != no_deps_workspace
                    {
                        return Err(
                            "Cargo workspace changed during private locked metadata".to_owned()
                        );
                    }
                    let (after, after_digest) = read_required_cargo_lockfile(private_path)?;
                    if &after != lockfile || &after_digest != digest {
                        return Err("private Cargo lockfile changed during metadata".to_owned());
                    }
                    if read_observation_file(missing_source_path, MAX_CARGO_OBSERVATION_FILE_BYTES)?
                        .is_some()
                    {
                        return Err(
                            "the Cargo-selected source lock appeared during private resolution"
                                .to_owned(),
                        );
                    }
                    metadata
                }
            }
        };

        let (lockfile, lockfile_digest, lock_origin, lock_path_witness) = match self
            .lock_state
            .as_ref()
            .expect("metadata resolution established a lock state")
        {
            CargoMetadataLockState::Observed {
                path,
                lockfile,
                digest,
            } => (
                Some(lockfile.clone()),
                Some(*digest),
                Some(CargoMetadataLockOrigin::Observed),
                Some(path.clone()),
            ),
            CargoMetadataLockState::Ephemeral {
                missing_source_path,
                lockfile,
                digest,
                ..
            } => (
                Some(lockfile.clone()),
                Some(*digest),
                Some(CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1),
                Some(missing_source_path.clone()),
            ),
        };
        let selection_after = cargo_tool_selection(&cargo, request_context, &version)?;
        let tool_after =
            metadata_tool_witness(request_context, &version, &selection_after, cached)?;
        let environment_after = cargo_environment_witness()?;
        if tool_before.digest != tool_after.digest || environment_before != environment_after {
            return Err("Cargo tools or selection environment changed during metadata".to_owned());
        }
        Ok(CargoMetadataRun {
            metadata,
            host,
            tool_witness: tool_after.digest,
            no_deps_witness: Some(no_deps_witness),
            lockfile,
            lockfile_digest,
            lock_origin,
            lock_path_witness,
        })
    }
}

fn run_cargo_metadata_query(
    cargo: &Path,
    request_context: &Path,
    selection: &CargoToolSelection,
    host: &str,
    requested_manifest: &Path,
    no_deps: bool,
    locked: bool,
    private_lockfile: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let manifest = requested_manifest
        .to_str()
        .ok_or_else(|| "requested Cargo manifest path is not UTF-8".to_owned())?;
    let mut arguments = Vec::<String>::new();
    if let Some(lockfile) = private_lockfile {
        validate_cargo_lockfile_path(lockfile)?;
        arguments.push("--config".to_owned());
        arguments.push(cargo_lockfile_path_config(lockfile)?);
    }
    arguments.push("metadata".to_owned());
    arguments.push("--offline".to_owned());
    if no_deps {
        arguments.push("--no-deps".to_owned());
    } else if locked {
        arguments.push("--locked".to_owned());
    }
    arguments.extend(["--format-version".to_owned(), "1".to_owned()]);
    if !no_deps {
        arguments.extend(["--filter-platform".to_owned(), host.to_owned()]);
    }
    arguments.extend(["--manifest-path".to_owned(), manifest.to_owned()]);
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    run_with_default_rustc(
        cargo,
        request_context,
        &arguments,
        MAX_METADATA_BYTES,
        selection
            .inject_default_rustc
            .then_some(selection.rustc.as_path()),
    )
}

fn cargo_lockfile_path_config(lockfile: &Path) -> Result<String, String> {
    validate_cargo_lockfile_path(lockfile)?;
    let value = lockfile
        .to_str()
        .ok_or_else(|| "private Cargo lockfile path is not UTF-8".to_owned())?;
    let quoted = serde_json::to_string(value)
        .map_err(|error| format!("cannot encode private Cargo lockfile path: {error}"))?;
    Ok(format!("resolver.lockfile-path={quoted}"))
}

fn create_private_cargo_lock_directory(
    source_roots: &[PathBuf],
) -> Result<(tempfile::TempDir, PathBuf), String> {
    if source_roots.len() > MAX_CARGO_TARGET_MEMBERSHIP_ROOTS {
        return Err("Cargo source-root set exceeds the private-lock limit".to_owned());
    }
    let mut canonical_roots = Vec::with_capacity(source_roots.len());
    for root in source_roots {
        if !root.is_absolute()
            || root.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::CurDir | std::path::Component::ParentDir
                )
            })
        {
            return Err("Cargo source root is not a canonical absolute path".to_owned());
        }
        let canonical = root
            .canonicalize()
            .map_err(|error| format!("cannot resolve Cargo source root: {error}"))?;
        if &canonical != root {
            return Err("Cargo source root changed through a path alias".to_owned());
        }
        canonical_roots.push(canonical);
    }

    let temporary_root = std::env::temp_dir()
        .canonicalize()
        .map_err(|error| format!("cannot resolve system temporary directory: {error}"))?;
    if canonical_roots
        .iter()
        .any(|source_root| temporary_root.starts_with(source_root))
    {
        return Err("system temporary directory is inside the Cargo source tree".to_owned());
    }
    let directory = tempfile::Builder::new()
        .prefix("backend-cargo-metadata-")
        .tempdir_in(&temporary_root)
        .map_err(|source| format!("cannot create private Cargo lock directory: {source}"))?;
    let private_directory = directory
        .path()
        .canonicalize()
        .map_err(|source| format!("cannot resolve private Cargo lock directory: {source}"))?;
    if canonical_roots
        .iter()
        .any(|source_root| private_directory.starts_with(source_root))
    {
        return Err("private Cargo lock directory was created inside a source root".to_owned());
    }
    Ok((directory, private_directory))
}

fn validate_cargo_lockfile_path(path: &Path) -> Result<(), String> {
    let text = path
        .to_str()
        .ok_or_else(|| "Cargo lockfile path is not UTF-8".to_owned())?;
    if text.is_empty()
        || text.len() > 4 * 1024
        || text.chars().any(char::is_control)
        || !path.is_absolute()
        || path.file_name().and_then(std::ffi::OsStr::to_str) != Some("Cargo.lock")
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
    {
        return Err(
            "Cargo lockfile path is not a bounded canonical absolute Cargo.lock".to_owned(),
        );
    }
    Ok(())
}

fn read_required_cargo_lockfile(path: &Path) -> Result<(String, [u8; 32]), String> {
    validate_cargo_lockfile_path(path)?;
    let bytes = read_observation_file(path, MAX_CARGO_OBSERVATION_FILE_BYTES)?
        .ok_or_else(|| format!("Cargo selected a missing lockfile: {}", path.display()))?;
    let lockfile = String::from_utf8(bytes)
        .map_err(|_| "Cargo selected lockfile is not valid UTF-8".to_owned())?;
    let digest = *blake3::hash(lockfile.as_bytes()).as_bytes();
    Ok((lockfile, digest))
}

fn missing_cargo_lockfile_path(error: &str) -> Option<PathBuf> {
    let path = error
        .strip_prefix("cargo metadata failed: error: cannot create the lock file ")?
        .strip_suffix(" because --locked was passed to prevent this")?;
    let path = PathBuf::from(path);
    validate_cargo_lockfile_path(&path).ok()?;
    Some(path)
}

fn cargo_supports_resolver_lockfile_path(version_output: &[u8]) -> Result<bool, String> {
    let version = String::from_utf8_lossy(version_output);
    let release = version
        .lines()
        .find_map(|line| line.strip_prefix("cargo "))
        .and_then(|line| line.split_whitespace().next())
        .ok_or_else(|| "selected Cargo returned no parseable release version".to_owned())?;
    let mut components = release.split('.');
    let major = components
        .next()
        .and_then(|component| component.parse::<u64>().ok())
        .ok_or_else(|| "selected Cargo returned an invalid major version".to_owned())?;
    let minor = components
        .next()
        .and_then(|component| component.parse::<u64>().ok())
        .ok_or_else(|| "selected Cargo returned an invalid minor version".to_owned())?;
    let _patch = components
        .next()
        .and_then(|component| component.split(['-', '+']).next())
        .and_then(|component| component.parse::<u64>().ok())
        .ok_or_else(|| "selected Cargo returned an invalid patch version".to_owned())?;
    if components.next().is_some() {
        return Err("selected Cargo returned an invalid release version".to_owned());
    }
    Ok(major > 1 || (major == 1 && minor >= 97))
}

/// Resolves only the lockfile override that Cargo successfully used. An
/// environment override is unambiguous; file configuration is admitted only
/// when every observed `resolver.lockfile-path` agrees on one absolute path.
/// Conflicting or relative config semantics are refused instead of guessed.
fn cargo_selected_lockfile_path(
    request_context: &Path,
    workspace: &Path,
) -> Result<PathBuf, String> {
    let environment = std::env::var_os("CARGO_RESOLVER_LOCKFILE_PATH");
    cargo_selected_lockfile_path_with_override(request_context, workspace, environment.as_deref())
}

fn cargo_selected_lockfile_path_with_override(
    request_context: &Path,
    workspace: &Path,
    environment: Option<&std::ffi::OsStr>,
) -> Result<PathBuf, String> {
    if let Some(value) = environment {
        let value = value
            .to_str()
            .map_err(|_| "CARGO_RESOLVER_LOCKFILE_PATH is not UTF-8".to_owned())?;
        let path = PathBuf::from(value);
        validate_cargo_lockfile_path(&path)?;
        return Ok(path);
    }
    let config_paths = cargo_config_paths(request_context)?;
    cargo_selected_lockfile_path_from_config_files(workspace, &config_paths)
}

fn cargo_selected_lockfile_path_from_config_files(
    workspace: &Path,
    config_paths: &[PathBuf],
) -> Result<PathBuf, String> {
    if config_paths.len() > MAX_CARGO_CONFIG_INPUTS {
        return Err("Cargo configuration input set exceeds its limit".to_owned());
    }
    let mut configured = BTreeSet::new();
    for config_path in config_paths {
        let Some(bytes) = read_observation_file(&config_path, MAX_CARGO_CONFIG_BYTES)? else {
            continue;
        };
        let document: toml::Value = std::str::from_utf8(&bytes)
            .map_err(|_| "Cargo config is not UTF-8".to_owned())?
            .parse()
            .map_err(|error: toml::de::Error| format!("Cargo config is malformed: {error}"))?;
        let Some(value) = document
            .get("resolver")
            .and_then(toml::Value::as_table)
            .and_then(|resolver| resolver.get("lockfile-path"))
        else {
            continue;
        };
        let value = value
            .as_str()
            .ok_or_else(|| "Cargo resolver.lockfile-path config is not a string".to_owned())?;
        let path = PathBuf::from(value);
        validate_cargo_lockfile_path(&path)?;
        configured.insert(path);
        if configured.len() > 1 {
            return Err(
                "Cargo resolver.lockfile-path differs across config inputs; refusing to guess Cargo precedence".to_owned(),
            );
        }
    }
    match configured.into_iter().next() {
        Some(path) => Ok(path),
        None => {
            let path = workspace.join("Cargo.lock");
            validate_cargo_lockfile_path(&path)?;
            Ok(path)
        }
    }
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
    let path = std::env::var_os("PATH");
    let mut candidates: Vec<PathBuf> = path
        .as_deref()
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
    observation_budget()?;
    let started = Instant::now();
    let deadline = observation_deadline().map_or(started + CARGO_DEADLINE, |deadline| {
        deadline.min(started + CARGO_DEADLINE)
    });
    let control = observation_control();
    let never_cancel = AtomicBool::new(false);
    let cancelled = control
        .as_ref()
        .map_or(&never_cancel, |control| &control.cancelled);
    let command = CaptureCommand {
        program: program.as_os_str().to_os_string(),
        args: arguments
            .iter()
            .map(|argument| (*argument).into())
            .collect(),
        cwd: Some(directory.to_path_buf()),
        environment: CaptureEnvironment::Inherit,
        // Cargo's selected compiler has already been resolved and witnessed.
        // Override only when no environment or Cargo config selected one.
        overrides: default_rustc
            .map(|rustc| vec![("RUSTC".into(), Some(rustc.as_os_str().to_os_string()))])
            .unwrap_or_default(),
    };
    let output = child_output::capture(
        &command,
        CaptureLimits {
            deadline,
            stdout_bytes: maximum,
            stderr_bytes: 64 * 1024,
        },
        cancelled,
    )
    .map_err(|error| match error {
        CaptureError::Cancelled => "Cargo source observation was cancelled".to_owned(),
        CaptureError::Deadline => format!(
            "cargo {} took longer than {}s",
            arguments[0],
            CARGO_DEADLINE.as_secs()
        ),
        CaptureError::OutputLimit {
            stream: OutputStream::Stdout,
            ..
        } => {
            format!("cargo {} wrote more than {maximum} bytes", arguments[0])
        }
        CaptureError::Unsupported => {
            "bounded Cargo child capture is unavailable on this platform".to_owned()
        }
        CaptureError::Capacity { maximum } => {
            format!("bounded Cargo child capture capacity is exhausted (maximum {maximum})")
        }
        other => format!("cargo {} capture failed: {other:?}", arguments[0]),
    })?;
    if !output.status.success() {
        let first = String::from_utf8_lossy(&output.stderr)
            .lines()
            .find(|line| line.starts_with("error"))
            .map_or_else(
                || format!("exit status {}", output.status),
                ToOwned::to_owned,
            );
        return Err(format!("cargo {} failed: {first}", arguments[0]));
    }
    Ok(output.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn cancelled_cargo_command_retires_its_process_group_and_pipes() {
        let control = Arc::new(ObservationControl::new());
        let cancelling = Arc::clone(&control);
        let trigger = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancelling.cancel();
        });
        let started = Instant::now();
        // This ignored native test binary stands in for a Cargo invocation
        // with a compiler descendant holding both inherited pipes. The exact
        // production run and platform capture path are exercised unchanged.
        let executable = std::env::current_exe().expect("test executable");
        let outcome = with_observation_control(control, || {
            run(
                &executable,
                &std::env::temp_dir(),
                &[
                    "--ignored",
                    "--exact",
                    "builtin::browse::tests::blocked_cargo_capture_fixture",
                    "--nocapture",
                ],
                1024,
            )
        });
        trigger.join().expect("cancellation trigger");
        assert!(
            outcome.is_err_and(|error| error.contains("cancelled")),
            "the child must return a cancellation terminal"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the descendant must not pin the output readers"
        );
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "native child fixture; invoked by the bounded capture test"]
    fn blocked_cargo_capture_fixture() {
        let child = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--ignored",
                "--exact",
                "builtin::browse::tests::blocked_cargo_descendant_fixture",
                "--nocapture",
            ])
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("descendant");
        let _ = child.id();
        std::thread::sleep(Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "native child fixture; invoked by the bounded capture test"]
    fn blocked_cargo_descendant_fixture() {
        std::thread::sleep(Duration::from_secs(10));
    }

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(prefix: &str) -> Scratch {
        Scratch(std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )))
    }

    fn fixture_member_input(workspace: &Path, member: &Path, id: &str) -> TreeInput {
        TreeInput {
            source: TreeSource::Cargo {
                host: "fixture-host".to_owned(),
            },
            root: workspace.to_str().expect("UTF-8 workspace path").to_owned(),
            packages: vec![TreeInputPackage {
                id: id.to_owned(),
                name: id.split_whitespace().next().unwrap_or(id).to_owned(),
                version: "0.1.0".to_owned(),
                member: true,
                has_bin: false,
                origin: None,
                source_root: Some(member.to_path_buf()),
                source_authority: CargoPackageSourceAuthorityStateV1::Unavailable(
                    CargoPackageSourceAuthorityFailureV1::NotObserved,
                ),
                license: None,
                description: None,
                categories: Vec::new(),
                keywords: Vec::new(),
            }],
            edges: Vec::new(),
            locked_inactive: 0,
            locked_inactive_coverage: LockedInactiveCoverage::Unavailable,
        }
    }

    fn project_input_read_for_test(
        input: TreeInput,
        watched: Vec<PathBuf>,
        file_witness: [u8; 32],
        tool_witness_reuse: Option<CargoToolWitnessReuse>,
    ) -> ProjectInputRead {
        let target_roots = input
            .packages
            .iter()
            .filter_map(|package| package.source_root.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let target_witness = cargo_auto_target_membership_witness(&target_roots)
            .expect("test target membership witness");
        ProjectInputRead {
            input,
            watched,
            target_roots,
            file_witness,
            target_witness,
            lock_origin_witness: [0; 32],
            tool_witness_reuse,
        }
    }

    fn plant_fixture_cache_entry(
        cache: &mut BrowseCache,
        input: TreeInput,
        watched: Vec<PathBuf>,
        metadata_context: &Path,
    ) {
        let workspace = PathBuf::from(&input.root);
        let target_roots = input
            .packages
            .iter()
            .filter_map(|package| package.source_root.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let target_witness = cargo_auto_target_membership_witness(&target_roots)
            .expect("fixture target membership witness");
        let lock_origin_witness = [0; 32];
        let input = Arc::new(input);
        let package_rows = source_package_row_index(&input);
        let file_witness =
            observation_witness_for_context(&workspace, metadata_context, &watched, None)
                .expect("fixture observation witness")
                .digest;
        let witness =
            compose_cargo_input_witness(file_witness, target_witness, lock_origin_witness);
        let retained_bytes = browse_entry_retained_bytes(
            &workspace,
            &metadata_context.to_path_buf(),
            &watched,
            watched.capacity(),
            &input,
            &package_rows,
            None,
        );
        cache.remove_workspace(&workspace);
        cache.cached_bytes = cache.cached_bytes.saturating_add(retained_bytes);
        cache.entries.insert(
            workspace,
            CacheEntry {
                witness,
                lock_origin_witness,
                target_roots,
                watched,
                input,
                metadata_context: metadata_context.to_path_buf(),
                retained_bytes,
                package_rows,
                request_bindings: HashMap::new(),
                tool_witness_reuse: None,
                last_used: 1,
            },
        );
    }

    #[test]
    fn requested_manifest_is_exact_and_does_not_guess_an_ancestor_workspace() {
        let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let repository = repository.canonicalize().expect("repository");
        let package = repository.join("crates/present");
        let requested = requested_cargo_manifest(&package)
            .expect("exact manifest read")
            .expect("package manifest");
        assert_eq!(requested.root, package);
        assert_eq!(requested.manifest, package.join("Cargo.toml"));
        assert!(requested.has_package);
        assert!(!requested.has_workspace);
    }

    #[test]
    fn a_manifestless_source_folder_under_a_workspace_never_starts_cargo() {
        let scratch = scratch("backend-browse-manifest-scope");
        let workspace = scratch.0.join("backend");
        let member = workspace.join("tests/journeys");
        let requested = member.join("fixtures/polyglot");
        std::fs::create_dir_all(requested.join("src")).expect("polyglot source folder");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"tests/journeys\"]\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"backend-journeys\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("member manifest");
        let requested = requested.canonicalize().expect("canonical source folder");

        let mut metadata_calls = 0;
        let error = BrowseCache::default()
            .input_with_read_project(&requested, |_requested, _cached| {
                metadata_calls += 1;
                Err("Cargo metadata must not run for this request".to_owned())
            })
            .expect_err("a nested source folder is not a Cargo project");

        assert_eq!(metadata_calls, 0, "no ancestor metadata read was attempted");
        assert!(error.contains("outside Cargo scope"), "{error}");
        assert!(error.contains(requested.to_str().expect("UTF-8 fixture path")));
    }

    #[test]
    fn a_fresh_ancestor_cache_does_not_authorize_an_excluded_package() {
        let scratch = scratch("backend-browse-excluded-package");
        let workspace = scratch.0.join("backend");
        let member = workspace.join("crates/member");
        let excluded = workspace.join("examples/standalone");
        std::fs::create_dir_all(&member).expect("workspace member");
        std::fs::create_dir_all(&excluded).expect("excluded package");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"examples/standalone\"]\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("member manifest");
        std::fs::write(
            excluded.join("Cargo.toml"),
            "[package]\nname = \"standalone\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("excluded package manifest");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let member = member.canonicalize().expect("canonical member");
        let excluded = excluded.canonicalize().expect("canonical excluded package");
        let workspace_tree = fixture_member_input(&workspace, &member, "member 0.1.0 (fixture)");
        let watched = vec![
            workspace.join("Cargo.lock"),
            workspace.join("Cargo.toml"),
            member.join("Cargo.toml"),
        ];
        let mut cache = BrowseCache::default();
        plant_fixture_cache_entry(&mut cache, workspace_tree, watched, &member);

        let reused_member = cache
            .input_with_read_project(&member, |_requested, _cached| {
                panic!("fresh metadata membership should reuse this exact member request")
            })
            .expect("fresh member cache hit");
        assert_eq!(reused_member.root, workspace.to_string_lossy().into_owned());

        let mut metadata_calls = 0;
        let standalone = cache
            .input_with_read_project(&excluded, |requested, _cached| {
                metadata_calls += 1;
                assert_eq!(requested.manifest, excluded.join("Cargo.toml"));
                let id = "standalone 0.1.0 (fixture)";
                let metadata = serde_json::to_vec(&serde_json::json!({
                    "workspace_root": excluded.clone(),
                    "workspace_members": [id],
                    "packages": [{ "id": id, "manifest_path": requested.manifest.clone() }]
                }))
                .expect("standalone Cargo-shaped metadata");
                assert_eq!(
                    metadata_proves_requested_manifest(&metadata, requested)
                        .expect("exact excluded package metadata proof"),
                    excluded,
                    "Cargo metadata for the exact excluded manifest selects its own root"
                );
                let input = fixture_member_input(&excluded, &excluded, id);
                let watched = vec![excluded.join("Cargo.lock"), requested.manifest.clone()];
                let witness =
                    observation_witness_for_context(&excluded, &requested.root, &watched, None)
                        .expect("standalone observation")
                        .digest;
                Ok(project_input_read_for_test(input, watched, witness, None))
            })
            .expect("standalone package is resolved from its own exact manifest");

        assert_eq!(
            metadata_calls, 1,
            "the ancestor cache cannot answer this request"
        );
        assert_eq!(standalone.root, excluded.to_string_lossy().into_owned());
        assert!(
            cache.entries.contains_key(&workspace),
            "the ancestor entry remains reusable for its members"
        );
        assert!(
            cache.entries.contains_key(&excluded),
            "the standalone gets its own cache key"
        );
    }

    #[test]
    fn a_workspace_request_cache_is_not_reused_from_a_member_context() {
        let scratch = scratch("backend-browse-request-context-cache");
        let workspace = scratch.0.join("workspace");
        let member = workspace.join("crates/member");
        std::fs::create_dir_all(member.join(".cargo")).expect("member Cargo config");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/member\"]\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("member manifest");
        std::fs::write(member.join(".cargo/config.toml"), "[net]\noffline = true\n")
            .expect("request-local Cargo config");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let member = member.canonicalize().expect("canonical member");
        let cached = fixture_member_input(&workspace, &member, "member 0.1.0 (fixture)");
        let watched = vec![
            workspace.join("Cargo.lock"),
            workspace.join("Cargo.toml"),
            member.join("Cargo.toml"),
        ];
        let mut cache = BrowseCache::default();
        plant_fixture_cache_entry(&mut cache, cached, watched.clone(), &workspace);

        let mut metadata_calls = 0;
        let member_input = cache
            .input_with_read_project(&member, |requested, _cached| {
                metadata_calls += 1;
                assert_eq!(requested.root, member);
                let input =
                    fixture_member_input(&workspace, &member, "member 0.1.0 (member-context)");
                let witness = observation_witness_for_context(&workspace, &member, &watched, None)
                    .expect("member-context observation")
                    .digest;
                Ok(project_input_read_for_test(
                    input,
                    watched.clone(),
                    witness,
                    None,
                ))
            })
            .expect("member context must be resolved independently");

        assert_eq!(
            metadata_calls, 1,
            "the workspace-context cache cannot answer"
        );
        assert_eq!(member_input.root, workspace.to_string_lossy().into_owned());
        assert!(cache.entries.contains_key(&workspace));
    }

    #[test]
    fn replacing_a_member_manifest_revokes_its_old_workspace_binding() {
        let scratch = scratch("backend-browse-replaced-member-manifest");
        let workspace = scratch.0.join("workspace");
        let package = workspace.join("packages/tool");
        let external = scratch.0.join("external-workspace");
        std::fs::create_dir_all(&package).expect("member package");
        std::fs::create_dir_all(&external).expect("external workspace");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"packages/*\"]\nresolver = \"3\"\n",
        )
        .expect("initial workspace manifest");
        std::fs::write(
            package.join("Cargo.toml"),
            "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("initial member manifest");
        std::fs::write(
            external.join("Cargo.toml"),
            "[workspace]\nresolver = \"3\"\n",
        )
        .expect("external workspace manifest");
        let workspace = workspace.canonicalize().expect("canonical old workspace");
        let package = package.canonicalize().expect("canonical package");
        let external = external
            .canonicalize()
            .expect("canonical external workspace");
        let old_tree = fixture_member_input(&workspace, &package, "tool 0.1.0 (old)");
        let old_watched = vec![
            workspace.join("Cargo.lock"),
            workspace.join("Cargo.toml"),
            package.join("Cargo.toml"),
        ];
        let mut cache = BrowseCache::default();
        plant_fixture_cache_entry(&mut cache, old_tree, old_watched, &package);
        let old_reply = cache
            .project_tree(&package, None)
            .expect("fresh old member cache");
        let old_binding = old_reply.request_binding.expect("bound request");
        assert!(cache.has_current_binding(&workspace, old_binding));

        std::fs::write(
            package.join("Cargo.toml"),
            "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2024\"\nworkspace = \"../../../external-workspace\"\n",
        )
        .expect("replace exact requested manifest with explicit workspace override");

        let mut metadata_calls = 0;
        let updated = cache
            .input_with_read_project(&package, |requested, _cached| {
                metadata_calls += 1;
                assert_eq!(requested.manifest, package.join("Cargo.toml"));
                let id = "tool 0.1.0 (new-workspace)";
                let metadata = serde_json::to_vec(&serde_json::json!({
                    "workspace_root": external.clone(),
                    "workspace_members": [id],
                    "packages": [{ "id": id, "manifest_path": requested.manifest.clone() }]
                }))
                .expect("Cargo-shaped replacement metadata");
                assert_eq!(
                    metadata_proves_requested_manifest(&metadata, requested)
                        .expect("new exact-manifest workspace proof"),
                    external
                );
                let input = fixture_member_input(&external, &package, id);
                let watched = vec![
                    external.join("Cargo.lock"),
                    external.join("Cargo.toml"),
                    requested.manifest.clone(),
                ];
                let witness =
                    observation_witness_for_context(&external, &requested.root, &watched, None)
                        .expect("new workspace witness")
                        .digest;
                Ok(project_input_read_for_test(input, watched, witness, None))
            })
            .expect("replacement manifest resolves through Cargo metadata");

        assert_eq!(metadata_calls, 1);
        assert_eq!(updated.root, external.to_string_lossy().into_owned());
        assert!(!cache.has_current_binding(&workspace, old_binding));
        assert!(
            !cache.entries.contains_key(&workspace),
            "old workspace entry was revoked"
        );
        assert!(
            cache.entries.contains_key(&external),
            "new resolved root owns the replacement"
        );
    }

    #[test]
    fn a_malformed_exact_manifest_never_starts_cargo() {
        let scratch = scratch("backend-browse-malformed-request-manifest");
        let package = scratch.0.join("package");
        std::fs::create_dir_all(&package).expect("package directory");
        std::fs::write(package.join("Cargo.toml"), "[package\nname = \"broken\"\n")
            .expect("malformed manifest");
        let mut metadata_calls = 0;
        let error = BrowseCache::default()
            .input_with_read_project(&package, |_requested, _cached| {
                metadata_calls += 1;
                Err("metadata must not run for a malformed request manifest".to_owned())
            })
            .expect_err("malformed exact manifest is rejected");
        assert!(error.contains("not valid UTF-8 TOML"), "{error}");
        assert_eq!(metadata_calls, 0);
    }

    #[test]
    fn cargo_metadata_membership_binds_an_exact_manifest_and_request_root() {
        let scratch = scratch("backend-browse-member-scope");
        let workspace = scratch.0.join("backend");
        let member = workspace.join("tests/journeys");
        std::fs::create_dir_all(&member).expect("workspace member");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"tests/*\"]\ndefault-members = []\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            member.join("Cargo.toml"),
            "[package]\nname = \"backend-journeys\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("member manifest");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let member = member.canonicalize().expect("canonical member");
        let requested = requested_cargo_manifest(&member)
            .expect("member manifest read")
            .expect("member request");
        let id = "backend-journeys 0.1.0 (path+file:///backend-journeys)";
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace.clone(),
            "workspace_members": [id],
            "packages": [{ "id": id, "manifest_path": requested.manifest.clone() }]
        }))
        .expect("Cargo-shaped metadata");
        // Cargo's metadata is the authority for glob and default-member
        // expansion. This function only checks its exact manifest/member rows.
        let effective = metadata_proves_requested_manifest(&metadata, &requested)
            .expect("Cargo metadata member proof");
        let observed_paths = metadata_observation_paths(&requested.root, &effective, &metadata)
            .expect("bounded Cargo input set");
        let required_paths =
            metadata_required_manifests(&metadata, &requested.manifest, &effective)
                .expect("required metadata manifest set");
        assert!(observed_paths.contains(&requested.manifest));
        assert!(required_paths.contains(&requested.manifest));
        assert!(observed_paths.contains(&workspace.join("Cargo.toml")));
        assert!(required_paths.contains(&workspace.join("Cargo.toml")));
        assert!(observed_paths.contains(&workspace.join("Cargo.lock")));

        assert_eq!(effective, workspace);
        let effective_text = effective.to_str().expect("UTF-8 workspace path");
        let member_binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
            &member,
            effective_text,
        )
        .expect("member request binding");
        let workspace_binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
            &workspace,
            effective_text,
        )
        .expect("workspace request binding");
        assert!(member_binding.matches_requested_root(&member));
        assert!(member_binding.matches_effective_workspace_root(effective_text));
        assert_ne!(
            member_binding.requested_root_digest, workspace_binding.requested_root_digest,
            "the requested member remains explicit in its workspace binding"
        );
        assert_eq!(
            member_binding.effective_workspace_root_digest,
            workspace_binding.effective_workspace_root_digest
        );
    }

    #[test]
    fn metadata_honors_an_explicit_nonancestor_workspace_and_rejects_nonmembers() {
        let scratch = scratch("backend-browse-explicit-workspace");
        let workspace = scratch.0.join("shared-workspace");
        let package = scratch.0.join("separate-tree/packages/tool");
        std::fs::create_dir_all(&workspace).expect("non-ancestor workspace");
        std::fs::create_dir_all(&package).expect("package outside workspace ancestry");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"../separate-tree/packages/tool\"]\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            package.join("Cargo.toml"),
            "[package]\nname = \"tool\"\nversion = \"0.1.0\"\nedition = \"2024\"\nworkspace = \"../../../shared-workspace\"\n",
        )
        .expect("explicit package workspace manifest");
        let workspace = workspace.canonicalize().expect("canonical workspace");
        let package = package.canonicalize().expect("canonical package");
        let requested = requested_cargo_manifest(&package)
            .expect("exact package manifest read")
            .expect("package request");
        let id = "tool 0.1.0 (fixture)";
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace.clone(),
            "workspace_members": [id],
            "packages": [{ "id": id, "manifest_path": requested.manifest.clone() }]
        }))
        .expect("Cargo-shaped metadata");
        assert_eq!(
            metadata_proves_requested_manifest(&metadata, &requested)
                .expect("explicit non-ancestor workspace proof"),
            workspace,
            "the Cargo result, not ancestor scanning, selects the effective workspace"
        );

        let excluded_metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace.clone(),
            "workspace_members": [],
            "packages": [{ "id": id, "manifest_path": requested.manifest.clone() }]
        }))
        .expect("nonmember Cargo-shaped metadata");
        assert!(
            metadata_proves_requested_manifest(&excluded_metadata, &requested)
                .is_err_and(|error| error.contains("workspace member")),
            "a package row without workspace_members membership is not admitted"
        );
    }

    #[test]
    fn cargo_lockfile_path_encoding_and_missing_diagnostic_are_narrow() {
        let scratch = scratch("backend-cargo-lock-path-policy");
        let lockfile = scratch.0.join("private lock directory/Cargo.lock");
        let override_value = cargo_lockfile_path_config(&lockfile).expect("TOML override");
        let parsed: toml::Value = override_value.parse().expect("valid Cargo --config value");
        assert_eq!(
            parsed
                .get("resolver")
                .and_then(toml::Value::as_table)
                .and_then(|resolver| resolver.get("lockfile-path"))
                .and_then(toml::Value::as_str),
            lockfile.to_str(),
            "the private absolute path must survive TOML quoting"
        );
        assert!(validate_cargo_lockfile_path(Path::new("Cargo.lock")).is_err());
        assert!(validate_cargo_lockfile_path(Path::new("/tmp/a/../Cargo.lock")).is_err());

        let missing = format!(
            "cargo metadata failed: error: cannot create the lock file {} because --locked was passed to prevent this",
            lockfile.display()
        );
        assert_eq!(missing_cargo_lockfile_path(&missing), Some(lockfile));
        assert!(
            missing_cargo_lockfile_path("cargo metadata failed: error: registry unavailable")
                .is_none()
        );
        assert!(missing_cargo_lockfile_path(
            "cargo metadata failed: error: cannot create the lock file relative/Cargo.lock because --locked was passed to prevent this"
        )
        .is_none());

        assert!(
            cargo_supports_resolver_lockfile_path(b"cargo 1.97.1 (hash)\n")
                .expect("current release parses")
        );
        assert!(
            cargo_supports_resolver_lockfile_path(b"cargo 1.97.0-nightly (hash)\n")
                .expect("nightly release parses")
        );
        assert!(
            !cargo_supports_resolver_lockfile_path(b"cargo 1.96.9 (hash)\n")
                .expect("older release parses")
        );
        assert!(cargo_supports_resolver_lockfile_path(b"cargo 1.97.invalid (hash)\n").is_err());
    }

    #[test]
    fn configured_lock_path_selection_accepts_only_absolute_unambiguous_inputs() {
        let scratch = scratch("backend-cargo-lock-config-policy");
        std::fs::create_dir_all(&scratch.0).expect("config policy root");
        let workspace = scratch.0.canonicalize().expect("canonical workspace");
        let selected = workspace.join("observed/Cargo.lock");
        let conflicting = workspace.join("other/Cargo.lock");
        let config_a = workspace.join("config-a.toml");
        let config_b = workspace.join("config-b.toml");
        let write_config = |path: &Path, lockfile: &Path| {
            let encoded = serde_json::to_string(lockfile.to_str().expect("UTF-8 path"))
                .expect("TOML string encoding");
            std::fs::write(path, format!("[resolver]\nlockfile-path = {encoded}\n"))
                .expect("Cargo config");
        };
        write_config(&config_a, &selected);
        write_config(&config_b, &selected);

        assert_eq!(
            cargo_selected_lockfile_path_from_config_files(
                &workspace,
                &[config_a.clone(), config_b.clone()]
            )
            .expect("identical absolute config values are unambiguous"),
            selected
        );
        assert_eq!(
            cargo_selected_lockfile_path_from_config_files(&workspace, &[])
                .expect("default workspace lock path"),
            workspace.join("Cargo.lock")
        );

        write_config(&config_b, &conflicting);
        assert!(
            cargo_selected_lockfile_path_from_config_files(
                &workspace,
                &[config_a.clone(), config_b.clone()]
            )
            .is_err()
        );
        std::fs::write(
            &config_b,
            "[resolver]\nlockfile-path = \"relative/Cargo.lock\"\n",
        )
        .expect("relative path config");
        assert!(
            cargo_selected_lockfile_path_from_config_files(&workspace, &[config_b.clone()])
                .is_err()
        );

        let environment_path = workspace.join("environment/Cargo.lock");
        assert_eq!(
            cargo_selected_lockfile_path_with_override(
                &workspace,
                &workspace,
                Some(environment_path.as_os_str())
            )
            .expect("environment override is unambiguous"),
            environment_path,
            "the Cargo environment override takes precedence over file values"
        );
    }

    #[test]
    fn selected_registry_checksum_and_target_membership_are_witnessed() {
        let scratch = scratch("backend-cargo-metadata-input-membership");
        let registry_manifest = scratch.0.join("registry/serde/Cargo.toml");
        let local_manifest = scratch.0.join("workspace/local/Cargo.toml");
        let metadata = serde_json::to_vec(&serde_json::json!({
            "packages": [
                {
                    "source": "registry+https://github.com/rust-lang/crates.io-index",
                    "manifest_path": registry_manifest,
                },
                {"source": null, "manifest_path": local_manifest}
            ]
        }))
        .expect("metadata package rows");
        assert_eq!(
            metadata_required_registry_checksums(&metadata).expect("registry checksum set"),
            [scratch.0.join("registry/serde/.cargo-checksum.json")]
        );

        let package = scratch.0.join("workspace/local");
        std::fs::create_dir_all(package.join("src")).expect("package src");
        std::fs::write(package.join("Cargo.toml"), "[package]\n").expect("package manifest");
        std::fs::write(package.join("src/lib.rs"), "pub fn local() {}\n").expect("lib source");
        let package = package.canonicalize().expect("canonical package root");
        let before = cargo_auto_target_membership_witness(std::slice::from_ref(&package))
            .expect("initial automatic target membership");
        std::fs::create_dir_all(package.join("src/bin")).expect("src/bin");
        std::fs::write(package.join("src/bin/tool.rs"), "fn main() {}\n").expect("new target");
        let after = cargo_auto_target_membership_witness(std::slice::from_ref(&package))
            .expect("updated automatic target membership");
        assert_ne!(
            before, after,
            "new auto-target names invalidate warm membership"
        );

        let lock_bytes = b"version = 4\n";
        let digest = *blake3::hash(lock_bytes).as_bytes();
        let path = package.join("Cargo.lock");
        let observed = cargo_lock_origin_witness(CargoMetadataLockOrigin::Observed, &path, digest);
        let ephemeral = cargo_lock_origin_witness(
            CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1,
            &path,
            digest,
        );
        assert_ne!(
            observed, ephemeral,
            "generated and observed lock bytes have distinct origins"
        );
        assert_ne!(
            compose_cargo_input_witness([1; 32], [2; 32], observed),
            compose_cargo_input_witness([1; 32], [2; 32], ephemeral)
        );
    }

    #[cfg(unix)]
    #[test]
    fn automatic_target_membership_refuses_linked_candidate_directories() {
        let scratch = scratch("backend-cargo-target-link-refusal");
        let package = scratch.0.join("package");
        let outside = scratch.0.join("outside");
        std::fs::create_dir_all(&package).expect("package root");
        std::fs::create_dir_all(&outside).expect("outside directory");
        std::os::unix::fs::symlink(&outside, package.join("examples"))
            .expect("linked Cargo auto-target directory");
        let package = package.canonicalize().expect("canonical package");
        assert!(cargo_auto_target_membership_witness(&[package]).is_err());
    }

    #[test]
    fn private_lock_directory_refuses_a_temporary_root_inside_source() {
        let temporary_root = std::env::temp_dir()
            .canonicalize()
            .expect("canonical system temporary directory");
        assert!(
            create_private_cargo_lock_directory(std::slice::from_ref(&temporary_root)).is_err(),
            "the lock resolver must refuse before creating a temporary directory in source"
        );
    }

    #[cfg(unix)]
    #[test]
    fn configured_lockfile_observation_does_not_follow_a_link() {
        let scratch = scratch("backend-cargo-lock-link-refusal");
        let source = scratch.0.join("source/Cargo.lock");
        let selected = scratch.0.join("selected/Cargo.lock");
        std::fs::create_dir_all(source.parent().expect("source parent"))
            .expect("source lock directory");
        std::fs::create_dir_all(selected.parent().expect("selected parent"))
            .expect("selected lock directory");
        std::fs::write(&source, "version = 4\n").expect("source lockfile");
        std::os::unix::fs::symlink(&source, &selected).expect("selected lock symlink");
        let selected = selected
            .parent()
            .expect("selected parent")
            .canonicalize()
            .expect("canonical selected parent")
            .join("Cargo.lock");
        assert!(
            read_required_cargo_lockfile(&selected).is_err(),
            "configured lockfiles are read through a no-follow directory capability"
        );
    }

    #[test]
    fn lockless_metadata_uses_a_private_lock_and_preserves_the_requested_tree() {
        let scratch = scratch("backend-cargo-lockless-metadata");
        std::fs::create_dir_all(&scratch.0).expect("lockless fixture root");
        let scratch_root = scratch.0.canonicalize().expect("canonical fixture root");
        let workspace = scratch_root.join("workspace");
        let app = workspace.join("app");
        let dependency = scratch_root.join("path-dependency");
        let config_dir = workspace.join(".cargo");
        std::fs::create_dir_all(app.join("src")).expect("app source");
        std::fs::create_dir_all(dependency.join("src")).expect("path dependency source");
        std::fs::create_dir_all(&config_dir).expect("Cargo config directory");
        let redirect = scratch_root.join("unwritten redirected lock/Cargo.lock");
        let config_path = config_dir.join("config.toml");
        let config = cargo_lockfile_path_config(&redirect).expect("absolute private path setting");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"app\"]\nresolver = \"3\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"lockless-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n[dependencies]\nlockless-helper = { path = \"../../path-dependency\" }\n",
        )
        .expect("app manifest");
        std::fs::write(app.join("src/lib.rs"), "pub fn fixture() {}\n").expect("app source file");
        std::fs::write(
            dependency.join("Cargo.toml"),
            "[package]\nname = \"lockless-helper\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("path dependency manifest");
        std::fs::write(dependency.join("src/lib.rs"), "pub fn helper() {}\n")
            .expect("path dependency source file");
        std::fs::write(&config_path, format!("{config}\n")).expect("resolver config");
        assert!(
            std::env::var_os("CARGO_RESOLVER_LOCKFILE_PATH").is_none(),
            "this integration fixture exercises the project-config lock path"
        );

        let workspace = workspace.canonicalize().expect("canonical workspace");
        let app = app.canonicalize().expect("canonical app");
        let requested = requested_cargo_manifest(&app)
            .expect("exact lockless manifest read")
            .expect("lockless package");
        let selected = selected_cargo_program(&app).expect("selected Cargo");
        let version = run(&selected, &app, &["-vV"], 64 * 1024).expect("Cargo version");
        let supports_private_lock =
            cargo_supports_resolver_lockfile_path(&version).expect("Cargo release version");
        let source_before = [
            workspace.join("Cargo.toml"),
            app.join("Cargo.toml"),
            app.join("src/lib.rs"),
            dependency.join("Cargo.toml"),
            dependency.join("src/lib.rs"),
            config_path.clone(),
        ]
        .into_iter()
        .map(|path| std::fs::read(path).expect("source input bytes"))
        .collect::<Vec<_>>();

        let mut session = CargoMetadataResolutionSession::default();
        let first = session.run(&requested, None);
        if !supports_private_lock {
            assert!(
                first.is_err(),
                "older Cargo must refuse private lock redirection"
            );
            assert!(!workspace.join("Cargo.lock").exists());
            assert!(!app.join("Cargo.lock").exists());
            assert!(!redirect.exists());
            return;
        }
        let first = first.expect("lockless exact metadata resolution");
        assert_eq!(
            first.lock_origin,
            Some(CargoMetadataLockOrigin::EphemeralGeneratedCargoLockV1)
        );
        assert_eq!(first.lock_path_witness.as_deref(), Some(redirect.as_path()));
        assert!(first.lockfile.is_some());
        assert_eq!(
            metadata_proves_requested_manifest(&first.metadata, &requested)
                .expect("resolved exact workspace"),
            workspace
        );
        let json: serde_json::Value =
            serde_json::from_slice(&first.metadata).expect("full metadata");
        assert!(
            json.get("resolve")
                .is_some_and(|resolve| !resolve.is_null())
        );
        let graph = metadata_input(&first.metadata, &first.host, first.lockfile.as_deref())
            .expect("resolved lockless dependency graph");
        let helper = graph
            .packages
            .iter()
            .find(|package| package.name == "lockless-helper")
            .expect("exact path dependency package row");
        assert!(
            graph.edges.iter().any(|edge| {
                graph
                    .packages
                    .iter()
                    .any(|package| package.id == edge.from && package.name == "lockless-fixture")
                    && edge.to == helper.id
            }),
            "the graph must contain Cargo's resolved app-to-helper edge: {:?}",
            graph.edges
        );

        let private_path = match session.lock_state.as_ref().expect("retained lock state") {
            CargoMetadataLockState::Ephemeral { private_path, .. } => private_path.clone(),
            CargoMetadataLockState::Observed { .. } => {
                panic!("the lockless fixture must not use a project lockfile")
            }
        };
        let private_directory = private_path
            .parent()
            .expect("private lock directory")
            .to_path_buf();
        assert!(!private_directory.starts_with(&workspace));
        assert!(!private_directory.starts_with(&app));
        assert!(
            private_path.is_file(),
            "the generated lock exists only in the private directory"
        );

        let second = session
            .run(&requested, None)
            .expect("locked metadata repeats from the private generated lock");
        assert_eq!(second.metadata, first.metadata);
        assert_eq!(second.lockfile, first.lockfile);
        assert_eq!(second.lockfile_digest, first.lockfile_digest);
        assert_eq!(second.lock_origin, first.lock_origin);
        assert!(!workspace.join("Cargo.lock").exists());
        assert!(!app.join("Cargo.lock").exists());
        assert!(!redirect.exists());
        for (path, expected) in [
            workspace.join("Cargo.toml"),
            app.join("Cargo.toml"),
            app.join("src/lib.rs"),
            dependency.join("Cargo.toml"),
            dependency.join("src/lib.rs"),
            config_path,
        ]
        .into_iter()
        .zip(source_before)
        {
            assert_eq!(
                std::fs::read(path).expect("source still readable"),
                expected
            );
        }
        drop(session);
        assert!(
            !private_directory.exists(),
            "the private generated lock directory is removed with its RAII owner"
        );
    }

    #[test]
    fn a_nested_workspace_request_requires_cargos_exact_root_witness() {
        let scratch = scratch("backend-browse-nested-workspace-scope");
        let outer = scratch.0.join("outer");
        let nested = outer.join("nested");
        std::fs::create_dir_all(&nested).expect("nested workspace");
        std::fs::write(
            outer.join("Cargo.toml"),
            "[workspace]\nmembers = []\nexclude = [\"nested\"]\nresolver = \"3\"\n",
        )
        .expect("outer workspace manifest");
        std::fs::write(
            nested.join("Cargo.toml"),
            "[workspace]\nmembers = []\nresolver = \"3\"\n",
        )
        .expect("nested workspace manifest");
        let nested = nested.canonicalize().expect("canonical nested workspace");
        let requested = requested_cargo_manifest(&nested)
            .expect("nested manifest read")
            .expect("nested workspace request");
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": nested.clone(),
            "workspace_members": [],
            "packages": []
        }))
        .expect("Cargo-shaped virtual-workspace metadata");
        assert_eq!(
            metadata_proves_requested_manifest(&metadata, &requested)
                .expect("exact nested workspace proof"),
            nested
        );
    }

    #[cfg(unix)]
    #[test]
    fn cargo_project_scope_does_not_follow_requested_directory_or_manifest_symlinks() {
        let scratch = scratch("backend-browse-symlink-scope");
        let target = scratch.0.join("target");
        std::fs::create_dir_all(&target).expect("target project");
        std::fs::write(
            target.join("Cargo.toml"),
            "[package]\nname = \"symlink-target\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .expect("target manifest");

        let directory_link = scratch.0.join("directory-link");
        std::os::unix::fs::symlink(&target, &directory_link).expect("project directory symlink");
        assert!(
            requested_cargo_manifest(&directory_link).is_err(),
            "requested project directories are opened without following links"
        );

        let manifest_link_root = scratch.0.join("manifest-link-root");
        std::fs::create_dir_all(&manifest_link_root).expect("manifest-link project");
        std::os::unix::fs::symlink(
            target.join("Cargo.toml"),
            manifest_link_root.join("Cargo.toml"),
        )
        .expect("manifest symlink");
        assert!(
            requested_cargo_manifest(&manifest_link_root).is_err(),
            "the exact requested manifest is opened without following links"
        );
    }

    #[test]
    fn a_directory_outside_any_cargo_project_says_so() {
        let error = BrowseCache::default()
            .project_tree(Path::new("/"), None)
            .expect_err("no project at the root");
        assert!(error.contains("outside Cargo scope"), "{error}");
    }

    #[test]
    fn package_readme_uses_manifest_path_and_cargo_default_order() {
        let scratch = scratch("backend-package-readme-selection");
        let package = scratch.0.join("package");
        std::fs::create_dir_all(&package).expect("package root");
        std::fs::write(package.join("crates-io.md"), "# Serde release\n").expect("declared README");
        let selected = select_and_read_package_readme(
            &package,
            &scratch.0,
            &CargoPackageReadmeManifestV1::Path("crates-io.md".to_owned()),
        )
        .expect("manifest-selected README");
        let SelectedPackageReadme::Read {
            root_scope,
            path,
            selection,
            contents,
        } = selected
        else {
            panic!("declared README must be returned")
        };
        assert_eq!(root_scope, CargoPackageReadmeRootScopeV1::Package);
        assert_eq!(path.as_str(), "crates-io.md");
        assert_eq!(selection, CargoPackageReadmeSelectionV1::ManifestPath);
        assert_eq!(
            readme_text(contents).expect("UTF-8 README").as_ref(),
            "# Serde release\n"
        );

        std::fs::write(package.join("README.md"), "markdown first\n").expect("default README");
        std::fs::write(package.join("README.txt"), "text second\n").expect("second default");
        let selected = select_and_read_package_readme(
            &package,
            &scratch.0,
            &CargoPackageReadmeManifestV1::Unspecified,
        )
        .expect("Cargo default README");
        let SelectedPackageReadme::Read {
            root_scope,
            path,
            selection,
            contents,
        } = selected
        else {
            panic!("default README must be returned")
        };
        assert_eq!(root_scope, CargoPackageReadmeRootScopeV1::Package);
        assert_eq!(path.as_str(), "README.md");
        assert_eq!(
            selection,
            CargoPackageReadmeSelectionV1::CargoConventionalDefault
        );
        assert_eq!(
            readme_text(contents).expect("UTF-8 README").as_ref(),
            "markdown first\n"
        );
    }

    #[test]
    fn package_readme_reports_absence_bad_content_and_oversize_without_prefixes() {
        let scratch = scratch("backend-package-readme-absence");
        let package = scratch.0.join("package");
        std::fs::create_dir_all(&package).expect("package root");
        assert!(matches!(
            select_and_read_package_readme(
                &package,
                &scratch.0,
                &CargoPackageReadmeManifestV1::Unspecified,
            ),
            Ok(SelectedPackageReadme::Absent(
                CargoPackageReadmeAbsenceV1::NoCargoDefault
            ))
        ));
        assert!(matches!(
            select_and_read_package_readme(
                &package,
                &scratch.0,
                &CargoPackageReadmeManifestV1::Disabled,
            ),
            Ok(SelectedPackageReadme::Absent(
                CargoPackageReadmeAbsenceV1::ManifestDisabled
            ))
        ));

        std::fs::write(package.join("README.md"), [0xff]).expect("non-UTF8 README");
        let selected = select_and_read_package_readme(
            &package,
            &scratch.0,
            &CargoPackageReadmeManifestV1::Unspecified,
        )
        .expect("file selection is independent from text decoding");
        let SelectedPackageReadme::Read { contents, .. } = selected else {
            panic!("present default README must be returned")
        };
        assert_eq!(
            readme_text(contents),
            Err(CargoPackageReadmeFailureV1::NotUtf8Text)
        );

        std::fs::write(
            package.join("README.md"),
            vec![b'x'; backend_library::MAX_CARGO_PACKAGE_README_BYTES + 1],
        )
        .expect("oversize README");
        assert_eq!(
            select_and_read_package_readme(
                &package,
                &scratch.0,
                &CargoPackageReadmeManifestV1::Unspecified,
            ),
            Err(CargoPackageReadmeFailureV1::ContentTooLarge)
        );
    }

    #[test]
    fn exact_manifest_readme_false_overrides_conventional_files() {
        let scratch = scratch("backend-package-readme-false");
        let package = scratch.0.join("package");
        std::fs::create_dir_all(&package).expect("package root");
        std::fs::write(
            package.join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"1.0.0\"\nreadme = false\n",
        )
        .expect("manifest");
        std::fs::write(package.join("README.md"), "not selected\n").expect("conventional README");
        let declaration = package_readme_manifest(&package).expect("exact manifest declaration");
        assert_eq!(declaration, CargoPackageReadmeManifestV1::Disabled);
        assert_eq!(
            select_and_read_package_readme(&package, &scratch.0, &declaration),
            Ok(SelectedPackageReadme::Absent(
                CargoPackageReadmeAbsenceV1::ManifestDisabled
            ))
        );
    }

    #[test]
    fn malformed_or_missing_package_manifest_fails_closed() {
        let scratch = scratch("backend-package-readme-manifest");
        let package = scratch.0.join("package");
        std::fs::create_dir_all(&package).expect("package root");
        assert_eq!(
            package_readme_manifest(&package),
            Err(CargoPackageReadmeFailureV1::PackageManifestUnavailable)
        );
        std::fs::write(package.join("Cargo.toml"), "[package\nreadme = false")
            .expect("bad manifest");
        assert_eq!(
            package_readme_manifest(&package),
            Err(CargoPackageReadmeFailureV1::PackageManifestMalformed)
        );
    }

    #[cfg(unix)]
    #[test]
    fn workspace_inheritance_uses_its_bound_root_and_rejects_escape_or_links() {
        let scratch = scratch("backend-package-readme-authority");
        let package = scratch.0.join("workspace/crates/present");
        let workspace = scratch.0.join("workspace");
        std::fs::create_dir_all(&package).expect("package root");
        std::fs::write(workspace.join("README.md"), "workspace overview\n")
            .expect("workspace README");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\n[workspace.package]\nreadme = \"README.md\"\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            package.join("Cargo.toml"),
            "[package]\nname = \"present\"\nversion = \"1.0.0\"\nreadme = { workspace = true }\n",
        )
        .expect("member manifest");
        let declaration = package_readme_manifest(&package).expect("manifest inheritance");
        assert_eq!(
            declaration,
            CargoPackageReadmeManifestV1::WorkspaceInherited
        );
        let inherited = select_and_read_package_readme(&package, &workspace, &declaration)
            .expect("explicit workspace inheritance");
        let SelectedPackageReadme::Read {
            root_scope,
            path,
            selection,
            contents,
        } = inherited
        else {
            panic!("workspace README should be selected")
        };
        assert_eq!(
            root_scope,
            CargoPackageReadmeRootScopeV1::EffectiveWorkspace
        );
        assert_eq!(path.as_str(), "README.md");
        assert_eq!(selection, CargoPackageReadmeSelectionV1::WorkspaceInherited);
        assert_eq!(
            readme_text(contents)
                .expect("workspace README text")
                .as_ref(),
            "workspace overview\n"
        );

        let outside_workspace = scratch.0.join("outside-workspace.md");
        std::fs::write(&outside_workspace, "outside\n").expect("outside workspace file");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\n[workspace.package]\nreadme = \"../outside-workspace.md\"\n",
        )
        .expect("workspace manifest with escape");
        assert_eq!(
            read_workspace_inherited_readme(&package, &workspace),
            Err(CargoPackageReadmeFailureV1::ReadmeOutsideAuthorizedRoot)
        );

        let outside = scratch.0.join("outside.md");
        std::fs::write(&outside, "outside README\n").expect("outside file");
        std::os::unix::fs::symlink(&outside, package.join("README.md")).expect("README link");
        assert_eq!(
            select_and_read_package_readme(
                &package,
                &workspace,
                &CargoPackageReadmeManifestV1::Unspecified,
            ),
            Err(CargoPackageReadmeFailureV1::SelectedFileUnavailable)
        );
    }

    #[test]
    fn owner_follows_readme_links_only_from_the_current_metadata_admitted_release() {
        let scratch = scratch("backend-package-readme-link-owner");
        let project = scratch.0.join("project");
        let helper = scratch.0.join("helper");
        std::fs::create_dir_all(project.join("src")).expect("project source");
        std::fs::create_dir_all(helper.join("src")).expect("helper source");
        std::fs::create_dir_all(helper.join("docs")).expect("helper docs");
        std::fs::write(
            project.join("Cargo.toml"),
            "[package]\nname = \"link-project\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\nreadme-helper = { path = \"../helper\" }\n",
        )
        .expect("project manifest");
        std::fs::write(
            project.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"link-project\"\nversion = \"0.1.0\"\ndependencies = [\n \"readme-helper\",\n]\n\n[[package]]\nname = \"readme-helper\"\nversion = \"0.1.0\"\n",
        )
        .expect("locked local graph");
        std::fs::write(project.join("src/lib.rs"), "").expect("project library");
        std::fs::write(
            helper.join("Cargo.toml"),
            "[package]\nname = \"readme-helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\nreadme = \"docs/README.md\"\n",
        )
        .expect("helper manifest");
        std::fs::write(helper.join("src/lib.rs"), "").expect("helper library");
        std::fs::write(
            helper.join("docs/README.md"),
            "# helper\n\n[guide](../guide.md#entry) [top](#helper)\n",
        )
        .expect("declared helper README");
        std::fs::write(helper.join("guide.md"), "# entry\nowner bytes\n")
            .expect("linked helper document");
        let project = project.canonicalize().expect("canonical project root");

        let mut owner = BrowseCache::default();
        let tree = owner
            .project_tree(&project, None)
            .expect("real offline Cargo metadata tree");
        let package = tree
            .direct
            .iter()
            .find(|dependency| dependency.name == "readme-helper")
            .and_then(|dependency| dependency.package_references.first())
            .and_then(Option::as_ref)
            .cloned()
            .expect("exact source-qualified path package row");
        let binding = tree.request_binding.expect("exact project tree request");
        let readme = owner.package_readme(CargoPackageReadmeRequestV1::from_tree(package, binding));
        assert!(
            readme.has_admissible_shape(),
            "owner README receipt: {readme:?}"
        );
        let origin = CargoPackageReadmeOriginV1::from_result(&readme)
            .expect("compact exact owner README origin");
        assert_eq!(origin.path.as_str(), "docs/README.md");
        assert_eq!(origin.root_scope, CargoPackageReadmeRootScopeV1::Package);

        let link = owner.package_readme_link(CargoPackageReadmeLinkRequestV1 {
            origin: origin.clone(),
            href: "../guide.md#entry".to_owned(),
        });
        assert!(link.has_admissible_shape(), "owner link receipt: {link:?}");
        assert!(matches!(
            link,
            CargoPackageReadmeLinkResultV1::Read {
                root_scope: CargoPackageReadmeRootScopeV1::Package,
                ref path,
                ref contents,
                ref fragment,
                ..
            } if path.as_str() == "guide.md"
                && contents.as_ref() == "# entry\nowner bytes\n"
                && fragment.as_deref() == Some("entry")
        ));

        let escaped = owner.package_readme_link(CargoPackageReadmeLinkRequestV1 {
            origin: origin.clone(),
            href: "../../outside.md".to_owned(),
        });
        assert!(matches!(
            escaped,
            CargoPackageReadmeLinkResultV1::Unavailable {
                reason: CargoPackageReadmeLinkFailureV1::OutsideScope,
                ..
            }
        ));

        std::fs::write(
            helper.join("docs/README.md"),
            "# helper changed\n\n[guide](../guide.md)\n",
        )
        .expect("replace manifest-selected README");
        assert!(matches!(
            owner.package_readme_link(CargoPackageReadmeLinkRequestV1 {
                origin,
                href: "../guide.md".to_owned(),
            }),
            CargoPackageReadmeLinkResultV1::Stale { .. }
        ));
    }

    #[test]
    fn two_real_workspaces_keep_exact_source_authority_with_bounded_lru_and_shared_inputs() {
        fn make_workspace(root: &Path, name: &str, helper_body: &str) {
            let member = root.join("member");
            std::fs::create_dir_all(member.join("src")).expect("workspace member source directory");
            let helper = root
                .parent()
                .expect("fixture workspace parent")
                .join(format!("{name}-helper"));
            std::fs::create_dir_all(helper.join("src")).expect("local helper source directory");
            std::fs::write(
                root.join("Cargo.toml"),
                "[workspace]\nmembers = [\"member\"]\nresolver = \"2\"\n",
            )
            .expect("workspace manifest");
            std::fs::write(
                root.join("Cargo.lock"),
                format!(
                    "version = 4\n\n[[package]]\nname = \"{name}\"\nversion = \"0.1.0\"\ndependencies = [\n \"cache-shared\",\n]\n\n[[package]]\nname = \"cache-shared\"\nversion = \"0.1.0\"\n"
                ),
            )
            .expect("project lockfile");
            std::fs::write(
                member.join("Cargo.toml"),
                format!(
                    "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ncache-shared = {{ path = \"../../{name}-helper\" }}\n"
                ),
            )
            .expect("workspace member manifest");
            std::fs::write(member.join("src/lib.rs"), "pub fn project_member() {}\n")
                .expect("workspace member source");
            std::fs::write(
                helper.join("Cargo.toml"),
                "[package]\nname = \"cache-shared\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
            )
            .expect("local helper manifest");
            std::fs::write(helper.join("src/lib.rs"), helper_body).expect("local helper source");
            std::fs::write(
                helper.join("README.md"),
                format!("# shared\n\n{helper_body}"),
            )
            .expect("local helper README");
        }

        fn request_for(
            cache: &BrowseCache,
            root: &Path,
            name: &str,
            binding: backend_library::browse::ProjectTreeRequestBindingV1,
        ) -> backend_library::CargoPackageSourceRequestV1 {
            let entry = cache.entries.get(root).expect("cached workspace");
            let authority = entry
                .input
                .packages
                .iter()
                .find_map(|row| match &row.source_authority {
                    CargoPackageSourceAuthorityStateV1::Admitted(authority)
                        if authority.name() == name =>
                    {
                        Some(authority)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| {
                    let row_states = entry
                        .input
                        .packages
                        .iter()
                        .map(|row| {
                            let state = match &row.source_authority {
                                CargoPackageSourceAuthorityStateV1::Admitted(_) => {
                                    "Admitted".to_owned()
                                }
                                CargoPackageSourceAuthorityStateV1::Unavailable(reason) => {
                                    format!("Unavailable({reason:?})")
                                }
                            };
                            (row.name.as_str(), row.version.as_str(), state)
                        })
                        .collect::<Vec<_>>();
                    panic!(
                        "Cargo metadata source receipt for {name}; tree source: {:?}; package rows: {row_states:?}",
                        entry.input.source
                    )
                });
            backend_library::CargoPackageSourceRequestV1::from_tree(
                authority
                    .package_reference()
                    .expect("canonical exact source route"),
                binding,
            )
        }

        let scratch = scratch("backend-browse-two-workspace-cache");
        let a = scratch.0.join("a");
        let b = scratch.0.join("b");
        let c = scratch.0.join("c");
        make_workspace(&a, "cache-a", "pub fn source_a() {}\n");
        make_workspace(&b, "cache-b", "pub fn source_b() {}\n");
        make_workspace(&c, "cache-c", "pub fn source_c() {}\n");
        let a = a.canonicalize().expect("canonical A root");
        let b = b.canonicalize().expect("canonical B root");
        let c = c.canonicalize().expect("canonical C root");

        let mut owner = BrowseCache::default();
        let tree_a = owner
            .project_tree(&a, None)
            .expect("real Cargo observation for workspace A");
        let binding_a = tree_a.request_binding.expect("A request binding");
        let request_a = request_for(&owner, &a, "cache-shared", binding_a);
        let shared_a_input = Arc::clone(&owner.entries.get(&a).expect("A cache entry").input);

        let member_root_a = a.join("member");
        let tree_a_member = owner
            .project_tree(&member_root_a, None)
            .expect("member request shares workspace A's real Cargo observation");
        let binding_a_member = tree_a_member
            .request_binding
            .expect("A member request binding");
        let request_a_member = request_for(&owner, &a, "cache-shared", binding_a_member);
        assert_ne!(
            binding_a.requested_root_digest, binding_a_member.requested_root_digest,
            "the workspace and member requests remain distinct"
        );
        assert_eq!(
            binding_a.effective_workspace_root_digest,
            binding_a_member.effective_workspace_root_digest,
            "both requests resolve to the same exact Cargo workspace"
        );
        assert_eq!(request_a.package, request_a_member.package);

        let tree_b = owner
            .project_tree(&b, None)
            .expect("real Cargo observation for workspace B");
        let binding_b = tree_b.request_binding.expect("B request binding");
        let request_b = request_for(&owner, &b, "cache-shared", binding_b);
        assert_ne!(
            request_a.package, request_b.package,
            "same-name/version local packages still have distinct exact source routes"
        );
        assert_eq!(owner.entries.len(), 2);
        assert_eq!(
            owner.bindings.len(),
            3,
            "A root/member and B root are indexed directly"
        );
        assert_eq!(owner.requested_bindings.len(), owner.bindings.len());
        assert!(
            owner.bindings.len()
                <= MAX_BROWSE_CACHED_WORKSPACES * MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE
        );
        assert!(owner.cached_bytes <= MAX_BROWSE_CACHE_BYTES);
        assert!(Arc::ptr_eq(
            &shared_a_input,
            &owner.entries.get(&a).expect("A retained cache entry").input
        ));

        let source_path = CargoPackageSourcePathV1::new("src/lib.rs").expect("source path");
        let source_a = owner.source_file(request_a.clone(), source_path.clone());
        assert!(source_a.has_admissible_shape(), "A receipt: {source_a:?}");
        assert!(matches!(
            &source_a,
            CargoPackageSourceFileResultV1::Read {
                request_binding,
                contents,
                ..
            } if *request_binding == binding_a && contents.as_ref() == "pub fn source_a() {}\n"
        ));
        let readme_a = owner.package_readme(CargoPackageReadmeRequestV1::from_tree(
            request_a.package.clone(),
            binding_a,
        ));
        assert!(
            readme_a.has_admissible_shape(),
            "A README receipt: {readme_a:?}"
        );
        assert!(matches!(
            readme_a,
            CargoPackageReadmeResultV1::Read { request_binding, .. }
                if request_binding == binding_a
        ));
        let readme_a_member = owner.package_readme(CargoPackageReadmeRequestV1::from_tree(
            request_a_member.package.clone(),
            binding_a_member,
        ));
        assert!(
            readme_a_member.has_admissible_shape(),
            "A member README receipt: {readme_a_member:?}"
        );
        assert!(matches!(
            readme_a_member,
            CargoPackageReadmeResultV1::Read { request_binding, .. }
                if request_binding == binding_a_member
        ));
        let source_a_member = owner.source_file(request_a_member.clone(), source_path.clone());
        assert!(matches!(
            &source_a_member,
            CargoPackageSourceFileResultV1::Read {
                request_binding,
                contents,
                ..
            } if *request_binding == binding_a_member
                && contents.as_ref() == "pub fn source_a() {}\n"
        ));
        let inventory_a = owner.source_inventory(request_a.clone());
        assert!(
            inventory_a.has_admissible_shape(),
            "A inventory: {inventory_a:?}"
        );
        assert!(matches!(
            inventory_a,
            CargoPackageSourceInventoryResultV1::Listed(inventory)
                if inventory.request_binding == binding_a
        ));

        let wrong_binding = backend_library::browse::ProjectTreeRequestBindingV1::for_paths(
            &a.join("never-admitted"),
            &tree_a.root,
        )
        .expect("well-shaped but different requested root");
        assert!(matches!(
            owner.source_file(
                backend_library::CargoPackageSourceRequestV1::from_tree(
                    request_a.package.clone(),
                    wrong_binding,
                ),
                source_path.clone(),
            ),
            CargoPackageSourceFileResultV1::Stale {
                request_binding,
                ..
            } if request_binding == wrong_binding
        ));

        // Adding C evicts the least-recently-used workspace B, while A stays
        // alive because its exact source read touched A after B was admitted.
        owner
            .project_tree(&c, None)
            .expect("real Cargo observation for workspace C");
        assert_eq!(owner.entries.len(), MAX_BROWSE_CACHED_WORKSPACES);
        assert!(matches!(
            owner.source_file(request_b, source_path.clone()),
            CargoPackageSourceFileResultV1::Unavailable {
                reason: CargoPackageSourceReadFailureV1::AuthorityUnavailable,
                ..
            }
        ));
        let source_a_after_c = owner.source_file(request_a.clone(), source_path.clone());
        assert!(
            matches!(
                source_a_after_c,
                CargoPackageSourceFileResultV1::Read { .. }
            ),
            "A must retain its exact unchanged source authority after B and C: {source_a_after_c:?}"
        );
        assert!(matches!(
            owner.source_file(request_a_member.clone(), source_path.clone()),
            CargoPackageSourceFileResultV1::Read {
                request_binding,
                ..
            } if request_binding == binding_a_member
        ));

        // A bounded set preserves multiple real ProjectTree requests for one
        // workspace, then revokes its least-recently-used exact roots once
        // the explicit request-binding limit is reached.
        let mut newest_binding = None;
        for index in 0..MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE {
            let requested = member_root_a
                .join("src")
                .join(format!("request-{index:02}"));
            std::fs::create_dir_all(&requested).expect("additional requested root directory");
            let tree = owner
                .project_tree(&requested, None)
                .expect("real observed request in workspace A");
            let binding = tree.request_binding.expect("additional request binding");
            assert_eq!(
                binding.effective_workspace_root_digest,
                binding_a.effective_workspace_root_digest
            );
            newest_binding = Some(binding);
        }
        assert_eq!(
            owner
                .entries
                .get(&a)
                .expect("workspace A remains cached")
                .request_bindings
                .len(),
            MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE
        );
        assert!(
            owner.bindings.len()
                <= MAX_BROWSE_CACHED_WORKSPACES * MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE
        );
        assert_eq!(owner.requested_bindings.len(), owner.bindings.len());
        assert!(matches!(
            owner.source_file(request_a.clone(), source_path.clone()),
            CargoPackageSourceFileResultV1::Stale { .. }
        ));
        assert!(matches!(
            owner.source_file(request_a_member, source_path.clone()),
            CargoPackageSourceFileResultV1::Stale { .. }
        ));
        let newest_request = request_for(
            &owner,
            &a,
            "cache-shared",
            newest_binding.expect("at least one bounded request binding"),
        );
        assert!(matches!(
            owner.source_file(newest_request.clone(), source_path.clone()),
            CargoPackageSourceFileResultV1::Read { .. }
        ));

        std::fs::write(
            a.parent()
                .expect("fixture parent")
                .join("cache-a-helper/Cargo.toml"),
            "[package]\nname = \"cache-shared\"\nversion = \"0.2.0\"\nedition = \"2021\"\n",
        )
        .expect("mutate A's local path dependency authority");
        assert!(
            matches!(
                owner.source_file(newest_request, source_path),
                CargoPackageSourceFileResultV1::Stale { .. }
            ),
            "an old source selector must be stale after its local dependency changes"
        );
        assert_eq!(owner.counters.tree_input_allocations, 4);
        assert!(owner.counters.cache_hits >= 6);
        assert!(owner.counters.retained_bytes_reused > 0);
        assert_eq!(owner.counters.evictions, 1);
        assert_eq!(owner.counters.request_binding_evictions, 2);
        assert!(owner.cached_bytes <= MAX_BROWSE_CACHE_BYTES);
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
        // they stand right now, but whose Cargo input contains a sentinel
        // host string. An untouched read must come back exactly as planted,
        // proving the fresh metadata membership row is sufficient to reuse
        // the observation without starting Cargo again.
        let watched = vec![root.join("Cargo.lock"), root.join("Cargo.toml")];
        let sentinel = Arc::new(TreeInput {
            source: TreeSource::Cargo {
                host: "planted-by-test".to_owned(),
            },
            root: root.to_string_lossy().into_owned(),
            packages: vec![TreeInputPackage {
                id: "gapfix 0.1.0 (test)".to_owned(),
                name: "gapfix".to_owned(),
                version: "0.1.0".to_owned(),
                member: true,
                has_bin: false,
                origin: None,
                source_root: Some(root.clone()),
                source_authority: CargoPackageSourceAuthorityStateV1::Unavailable(
                    backend_library::CargoPackageSourceAuthorityFailureV1::NotObserved,
                ),
                license: None,
                description: None,
                categories: Vec::new(),
                keywords: Vec::new(),
            }],
            edges: Vec::new(),
            locked_inactive: 0,
            locked_inactive_coverage: LockedInactiveCoverage::Unavailable,
        });
        let mut cache = BrowseCache::default();
        let mut plant_sentinel = |cache: &mut BrowseCache, watched: Vec<PathBuf>| {
            cache.remove_workspace(&root);
            let package_rows = source_package_row_index(&sentinel);
            let target_roots = sentinel
                .packages
                .iter()
                .filter_map(|package| package.source_root.clone())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            let target_witness = cargo_auto_target_membership_witness(&target_roots)
                .expect("sentinel target membership witness");
            let lock_origin_witness = [0; 32];
            let file_witness = witness(&watched);
            let retained_bytes = browse_entry_retained_bytes(
                &root,
                &root,
                &watched,
                watched.capacity(),
                &sentinel,
                &package_rows,
                None,
            );
            cache.cached_bytes = cache.cached_bytes.saturating_add(retained_bytes);
            cache.entries.insert(
                root.clone(),
                CacheEntry {
                    witness: compose_cargo_input_witness(
                        file_witness,
                        target_witness,
                        lock_origin_witness,
                    ),
                    lock_origin_witness,
                    target_roots,
                    watched,
                    input: Arc::clone(&sentinel),
                    metadata_context: root.clone(),
                    retained_bytes,
                    package_rows,
                    request_bindings: HashMap::new(),
                    tool_witness_reuse: None,
                    last_used: 1,
                },
            );
        };
        plant_sentinel(&mut cache, watched.clone());

        let untouched = cache.project_tree(&root, None).expect("untouched read");
        assert_eq!(
            untouched.source, sentinel.source,
            "an untouched workspace must be served from the cache, not recomputed: {untouched:?}"
        );
        assert_eq!(cache.counters.cache_hits, 1);
        assert!(cache.counters.retained_bytes_reused > 0);
        let shared_first = cache.input(&root).expect("first shared input handle");
        let shared_second = cache.input(&root).expect("second shared input handle");
        assert!(Arc::ptr_eq(&shared_first, &shared_second));

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
        assert!(
            matches!(&touched.source, TreeSource::Cargo { host } if !host.is_empty()),
            "a lockfile-only change must force a fresh metadata authority, not the planted sentinel: {touched:?}"
        );
        assert_eq!(cache.counters.tree_input_allocations, 1);

        // Replant the sentinel against the new lockfile, then change only
        // Cargo.toml. Both inputs to Cargo's answer have independent guards.
        plant_sentinel(&mut cache, watched.clone());
        std::fs::write(
            &manifest,
            "[package]\nname = \"gapfix\"\nversion = \"0.1.0\"\nedition = \"2021\"\ndescription = \"manifest-only cache invalidation\"\n",
        )
        .expect("changed manifest");
        let touched = cache.project_tree(&root, None).expect("manifest-only read");
        assert_ne!(
            touched.source, sentinel.source,
            "a changed manifest alone must force a real read: {touched:?}"
        );
        assert_eq!(cache.counters.tree_input_allocations, 2);

        // Cargo discovers `src/bin` targets from directory contents. Adding
        // one must invalidate a warm dependency-tree cache even though neither
        // Cargo.toml nor Cargo.lock changed.
        plant_sentinel(&mut cache, watched);
        std::fs::create_dir_all(root.join("src/bin")).expect("automatic bin directory");
        std::fs::write(root.join("src/bin/sidecar.rs"), "fn main() {}\n")
            .expect("automatic bin target");
        let touched = cache
            .project_tree(&root, None)
            .expect("automatic-target cache invalidation");
        assert!(
            matches!(&touched.source, TreeSource::Cargo { host } if !host.is_empty()),
            "a new Cargo auto-target must force a real metadata refresh: {touched:?}"
        );
        assert!(
            touched
                .packages
                .iter()
                .any(|package| package.name == "gapfix" && package.has_bin),
            "Cargo metadata should report the newly discovered binary target: {touched:?}"
        );
        assert_eq!(cache.counters.tree_input_allocations, 3);
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
        let requested = requested_cargo_manifest(&app)
            .expect("request manifest read")
            .expect("app package request");
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
            &requested,
            |manifest: &Path| {
                assert_eq!(manifest, requested.manifest);
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
                .err()
                .expect("changed path dependency must invalidate source authority")
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
        let requested = requested_cargo_manifest(&app)
            .expect("request manifest read")
            .expect("app package request");
        let dependency_manifest = dependency_manifest
            .canonicalize()
            .expect("canonical dependency manifest");
        let metadata = serde_json::to_vec(&serde_json::json!({
            "workspace_root": workspace,
            "workspace_members": ["app 0.1.0 (path+file:///workspace/app)"],
            "packages": [
                {"id": "app 0.1.0 (path+file:///workspace/app)", "manifest_path": app_manifest},
                {"id": "dep 0.1.0 (path+file:///path-dependency)", "manifest_path": dependency_manifest}
            ]
        }))
        .expect("metadata fixture");
        let required = metadata_required_manifests(&metadata, &requested.manifest, &workspace)
            .expect("required manifest set");
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
            &requested,
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
                .err()
                .expect("required absent manifest must refuse authority")
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
            "[env]\nCARGO_BUILD_RUSTC = '/opt/custom/rustc'\n",
            "[env]\nCARGO_BUILD_RUSTC_WRAPPER = 'sccache'\n",
            "[env]\nCARGO_BUILD_RUSTC_WORKSPACE_WRAPPER = 'sccache'\n",
            "[env]\nCARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER = 'clang'\n",
            "[env]\nPATH = '/custom/tools'\n",
            "[env]\nSCCACHE_CONF = '/tmp/alternate.toml'\n",
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
    fn cargo_tool_environment_precedence_honors_empty_direct_wrapper_disable() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-cargo-tool-selection-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        let workspace = scratch.0.join("workspace");
        let nested_config = workspace.join("nested/.cargo/config.toml");
        std::fs::create_dir_all(workspace.join("direct")).expect("direct tool directory");
        std::fs::create_dir_all(workspace.join("nested/.cargo/bin"))
            .expect("wrong-origin decoy directory");
        std::fs::create_dir_all(workspace.join("nested/bin"))
            .expect("config-origin tool directory");
        std::fs::write(workspace.join("direct/rustc"), b"direct").expect("direct tool");
        std::fs::write(workspace.join("nested/.cargo/bin/rustc"), b"wrong-origin")
            .expect("wrong-origin decoy tool");
        std::fs::write(workspace.join("nested/bin/rustc"), b"configured").expect("configured tool");
        std::fs::create_dir_all(nested_config.parent().unwrap()).expect("nested config directory");
        // Cargo paths in a config file are relative to the parent directory
        // of the directory containing that config: two levels above this file.
        let config_document = "[build]\nrustc = 'bin/rustc'\n"
            .parse::<toml::Value>()
            .expect("nested config contents");
        let configured = cargo_configured_tool(&nested_config, &config_document, "rustc")
            .expect("configured tool schema")
            .expect("nested rustc setting");
        let direct = resolve_effective_tool_value(
            Some("direct/rustc".to_owned()),
            Some("ignored/alias".to_owned()),
            Some(&configured),
            &workspace,
            "rustc",
            false,
        )
        .expect("direct environment override")
        .expect("selected direct tool");
        assert_eq!(direct, workspace.join("direct/rustc"));

        let alias = resolve_effective_tool_value(
            None,
            Some("direct/rustc".to_owned()),
            Some(&configured),
            &workspace,
            "rustc",
            false,
        )
        .expect("config environment override")
        .expect("selected config environment tool");
        assert_eq!(alias, workspace.join("direct/rustc"));

        let configured_path =
            resolve_effective_tool_value(None, None, Some(&configured), &workspace, "rustc", false)
                .expect("nested config path")
                .expect("selected config file tool");
        assert_eq!(configured_path, workspace.join("nested/bin/rustc"));

        assert_eq!(
            resolve_effective_tool_value(
                Some(String::new()),
                Some("direct/rustc".to_owned()),
                Some(&configured),
                &workspace,
                "rustc wrapper",
                true,
            )
            .expect("empty direct wrapper disables lower-priority settings"),
            None,
        );
        assert_eq!(
            resolve_effective_tool_value(
                None,
                Some(String::new()),
                Some(&configured),
                &workspace,
                "rustc wrapper",
                true,
            )
            .expect("empty Cargo config alias disables the lower-priority wrapper"),
            None,
        );
        assert!(
            resolve_effective_tool_value(
                Some(String::new()),
                None,
                Some(&configured),
                &workspace,
                "rustc",
                false,
            )
            .is_err()
        );
    }

    #[test]
    fn equal_configured_tool_paths_from_different_origins_are_not_conflicts() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-cargo-tool-origins-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        let workspace = scratch.0.join("workspace");
        std::fs::create_dir_all(workspace.join(".cargo")).expect("outer config dir");
        std::fs::create_dir_all(workspace.join("nested/.cargo")).expect("nested config dir");
        std::fs::create_dir_all(workspace.join("shared")).expect("shared tool dir");
        let tool = workspace.join("shared/rustc");
        std::fs::write(&tool, b"rustc").expect("shared tool");
        let outer = CargoConfiguredTool {
            value: "shared/rustc".to_owned(),
            config_path: workspace.join(".cargo/config.toml"),
        };
        let nested = CargoConfiguredTool {
            value: "../shared/rustc".to_owned(),
            config_path: workspace.join("nested/.cargo/config.toml"),
        };
        let resolved = collapse_configured_tool_values(&workspace, "rustc", [outer, nested])
            .expect("same executable from separate witnessed origins");
        assert_eq!(resolved.len(), 1);
        assert_eq!(
            resolved.keys().next(),
            Some(&tool.canonicalize().expect("canonical tool"))
        );
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
    fn recognizes_only_the_exact_shipped_dependency_cache_wrapper() {
        let template = include_bytes!("../../../../.config/scripts/cargo-rustc-cache.sh");
        let script = template
            .windows(b"@sccache@".len())
            .position(|window| window == b"@sccache@")
            .expect("one sccache substitution");
        let mut generated = Vec::new();
        generated.extend_from_slice(&template[..script]);
        generated.extend_from_slice(
            b"/nix/store/4n5rm7aink6xcsj5df33sf7wm3m387a2-sccache-0.17.0/bin/sccache",
        );
        generated.extend_from_slice(&template[script + b"@sccache@".len()..]);
        let (interpreter, sccache) = recognized_nudox_dependency_cache_wrapper(&generated)
            .expect("exact generated wrapper is recognized");
        assert_eq!(interpreter, PathBuf::from("/bin/sh"));
        assert_eq!(
            sccache,
            PathBuf::from("/nix/store/4n5rm7aink6xcsj5df33sf7wm3m387a2-sccache-0.17.0/bin/sccache")
        );
        let mut no_direct_shebang = generated.clone();
        no_direct_shebang[..2].copy_from_slice(b"XX");
        assert!(recognized_nudox_dependency_cache_wrapper(&no_direct_shebang).is_none());

        for suffix in [";id", " $(id)", "$HOME", "`id`", "*", "/../other"] {
            let mut injected = Vec::new();
            injected.extend_from_slice(&template[..script]);
            injected.extend_from_slice(
                b"/nix/store/4n5rm7aink6xcsj5df33sf7wm3m387a2-sccache-0.17.0/bin/sccache",
            );
            injected.extend_from_slice(suffix.as_bytes());
            injected.extend_from_slice(&template[script + b"@sccache@".len()..]);
            assert!(
                recognized_nudox_dependency_cache_wrapper(&injected).is_none(),
                "unsafe unquoted shell word suffix must be rejected: {suffix}"
            );
        }

        let mut with_nix_bash =
            b"#!/nix/store/4n5rm7aink6xcsj5df33sf7wm3m387a2-bash-5.2/bin/bash\n".to_vec();
        with_nix_bash.extend_from_slice(&generated);
        assert!(recognized_nudox_dependency_cache_wrapper(&with_nix_bash).is_some());

        let mut write_shell_script =
            b"#!/nix/store/4n5rm7aink6xcsj5df33sf7wm3m387a2-bash-5.2/bin/bash -e\n".to_vec();
        let source_body = &template[template
            .iter()
            .position(|byte| *byte == b'\n')
            .expect("source shebang")
            + 1..];
        let source_placeholder = source_body
            .windows(b"@sccache@".len())
            .position(|window| window == b"@sccache@")
            .expect("source body placeholder");
        write_shell_script.extend_from_slice(&source_body[..source_placeholder]);
        write_shell_script.extend_from_slice(
            b"/nix/store/4n5rm7aink6xcsj5df33sf7wm3m387a2-sccache-0.17.0/bin/sccache",
        );
        write_shell_script
            .extend_from_slice(&source_body[source_placeholder + b"@sccache@".len()..]);
        assert_eq!(
            recognized_nudox_dependency_cache_wrapper(&write_shell_script),
            Some((
                PathBuf::from("/nix/store/4n5rm7aink6xcsj5df33sf7wm3m387a2-bash-5.2/bin/bash"),
                PathBuf::from(
                    "/nix/store/4n5rm7aink6xcsj5df33sf7wm3m387a2-sccache-0.17.0/bin/sccache"
                )
            ))
        );
        let mut no_nix_shebang = write_shell_script.clone();
        no_nix_shebang[..2].copy_from_slice(b"XX");
        assert!(recognized_nudox_dependency_cache_wrapper(&no_nix_shebang).is_none());
        let generated_script =
            String::from_utf8(write_shell_script).expect("ASCII generated wrapper");
        for altered in [
            generated_script.replace(" -e\n", " -x\n"),
            generated_script.replace("set -eu\n", "set -e\n"),
            generated_script.replace("/bin/sccache", "/bin/other"),
        ] {
            assert!(recognized_nudox_dependency_cache_wrapper(altered.as_bytes()).is_none());
        }

        for shebang in [
            b"#!/bin/sh -e\n".as_slice(),
            b"#!/usr/bin/env sh\n".as_slice(),
            b"#! /bin/sh\n".as_slice(),
            b"#!/nix/store/4n5rm7aink6xcsj5df33sf7wm3m387a2-bash  -c\n".as_slice(),
        ] {
            let mut unsupported = shebang.to_vec();
            unsupported.extend_from_slice(template);
            let placeholder = unsupported
                .windows(b"@sccache@".len())
                .position(|window| window == b"@sccache@")
                .expect("template substitution");
            unsupported.splice(
                placeholder..placeholder + b"@sccache@".len(),
                b"/nix/store/4n5rm7aink6xcsj5df33sf7wm3m387a2-sccache-0.17.0/bin/sccache"
                    .iter()
                    .copied(),
            );
            assert!(
                recognized_nudox_dependency_cache_wrapper(&unsupported).is_none(),
                "unsupported shebang must be rejected: {:?}",
                String::from_utf8_lossy(shebang)
            );
        }

        generated.extend_from_slice(b"\nexec /tmp/other-rustc \"$@\"\n");
        assert!(recognized_nudox_dependency_cache_wrapper(&generated).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn nix_store_root_identity_ignores_unrelated_object_installation() {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let scratch = Scratch(std::env::temp_dir().join(format!(
            "backend-nix-root-witness-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        )));
        std::fs::create_dir_all(&scratch.0).expect("store root");
        let before = std::fs::metadata(&scratch.0).expect("root metadata");
        let mut before_hash = blake3::Hasher::new();
        hash_unix_store_root_controls(&mut before_hash, &scratch.0, &before);
        std::fs::write(scratch.0.join("unrelated-object"), b"new object")
            .expect("unrelated store object");
        let after = std::fs::metadata(&scratch.0).expect("updated root metadata");
        assert!(
            before.len() != after.len() || before.modified().unwrap() != after.modified().unwrap()
        );
        let mut after_hash = blake3::Hasher::new();
        hash_unix_store_root_controls(&mut after_hash, &scratch.0, &after);
        assert_eq!(
            before_hash.finalize().as_bytes(),
            after_hash.finalize().as_bytes(),
            "unrelated store entries must not invalidate immutable tool content reuse"
        );
    }

    #[test]
    fn cargo_environment_witness_includes_platform_cache_selectors() {
        for name in ["APPDATA", "SCCACHE_CONF", "CARGO_BUILD_RUSTC_WRAPPER"] {
            assert!(is_cargo_environment_witness_name(name), "{name}");
        }
        assert!(!is_cargo_environment_witness_name("UNRELATED_SETTING"));
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
            "workspace_members": ["app 0.1.0 (path+file:///workspace/app)"],
            "packages": [
                {"id": "app 0.1.0 (path+file:///workspace/app)", "manifest_path": app_manifest},
                {"id": "dep 0.1.0 (path+file:///path-dependency)", "manifest_path": dependency_manifest}
            ]
        }))
        .expect("metadata fixture");
        let requested = requested_cargo_manifest(&app)
            .expect("request manifest read")
            .expect("app package request");
        let result = coherent_metadata_with(
            &requested,
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
