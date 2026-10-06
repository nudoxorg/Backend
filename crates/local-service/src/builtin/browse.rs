//! A project's dependency tree, read from Cargo and the advisory authority.
//!
//! Cargo is the authority for the current target's active dependency graph:
//! `cargo metadata --filter-platform <host>`. Cargo.lock rows outside that
//! graph can also be disabled by feature selection, so the difference is not
//! called "other platforms." When Cargo cannot answer (not installed, no
//! network for a missing download, a stale lockfile under `--locked`), the
//! tree is read from `Cargo.lock` alone and says so.
//!
//! The Cargo half is cached against the full exact-manifest metadata response,
//! selected lock origin, package manifests, exact Cargo target rows,
//! configuration, tools, and registry checksums. Every candidate cache hit
//! first asks Cargo again, so Cargo decides current members, targets, patches,
//! and resolution state. Advisories are observed on every read.

mod cargo_metadata;

use backend_library::browse::{
    ProjectTree, TreeInput, TreeInputPackage, TreeSource, build_tree, lockfile_input,
    metadata_input_with_stable_source_witness,
};
use backend_library::{
    CargoPackageReadmeAbsenceV1, CargoPackageReadmeFailureV1, CargoPackageReadmeLinkFailureV1,
    CargoPackageReadmeLinkRequestV1, CargoPackageReadmeLinkResultV1,
    CargoPackageReadmeLinkTargetV1, CargoPackageReadmeManifestV1, CargoPackageReadmeOriginV1,
    CargoPackageReadmeRequestV1, CargoPackageReadmeResultV1, CargoPackageReadmeRootScopeV1,
    CargoPackageReadmeSelectionV1, CargoPackageReadmeV1, CargoPackageSourceAuthorityStateV1,
    CargoPackageSourceAuthorityV1, CargoPackageSourceFileResultV1,
    CargoPackageSourceInventoryCoverageV1, CargoPackageSourceInventoryFailureV1,
    CargoPackageSourceInventoryGapV1, CargoPackageSourceInventoryResultV1,
    CargoPackageSourceInventoryV1, CargoPackageSourcePathV1, CargoPackageSourceReadFailureV1,
    CargoPackageSourceSemanticStatusV1, MAX_CARGO_PACKAGE_SOURCE_INVENTORY_PATHS,
    MAX_CARGO_PACKAGE_SOURCE_INVENTORY_SCAN_ENTRIES, PackageReference,
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

#[cfg(test)]
use backend_library::CargoPackageSourceAuthorityFailureV1;
#[cfg(test)]
use backend_library::browse::{
    LockedInactiveCoverage, LockfileGraphCoverage, LockfileWorkspaceMembership,
};

/// Largest `cargo metadata` document admitted.
const MAX_METADATA_BYTES: usize = 64 * 1024 * 1024;
/// Maximum time for one Cargo run and one deferred browse observation.
const CARGO_DEADLINE: Duration = Duration::from_secs(90);
const MAX_CARGO_OBSERVATION_PATHS: usize = 42_048;
const MAX_CARGO_OBSERVATION_FILE_BYTES: usize = 16 * 1024 * 1024;
const MAX_CARGO_OBSERVATION_TOTAL_BYTES: usize = 256 * 1024 * 1024;
const MAX_CARGO_METADATA_PACKAGES: usize = 20_000;
const MAX_CARGO_METADATA_TARGETS_PER_PACKAGE: usize = 65_536;
const MAX_CARGO_TOOL_BINARY_BYTES: u64 = 128 * 1024 * 1024;
const MAX_CARGO_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_CARGO_CONFIG_INPUTS: usize = 256;
const MAX_CARGO_CONFIG_DEPTH: usize = 16;
const MAX_SOURCE_DIRECTORY_ENTRIES: usize = 2_048;
const MAX_SOURCE_DIRECTORY_DEPTH: usize = 32;
const MAX_BROWSE_CACHED_WORKSPACES: usize = 2;
const MAX_BROWSE_CACHE_BYTES: usize = 128 * 1024 * 1024;
const MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE: usize = 16;
const MAX_BROWSE_REQUEST_BINDINGS_PER_CONTEXT: usize = 1;
const MAX_BROWSE_CACHED_CONTEXTS_PER_WORKSPACE: usize = MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE;
/// Conservative reserve for the bounded workspace, context, and request-
/// binding index bucket allocations. These maps retain their high-water buckets
/// after row removal, so account for their maximum size once for the cache's
/// lifetime.
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

/// One immutable Cargo observation for an exact invocation directory and the
/// effective workspace that Cargo resolved from it. Different request CWDs
/// in one workspace may have different Cargo config and tool recipes.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct BrowseContextKey {
    workspace: PathBuf,
    invocation_root: PathBuf,
}

#[derive(Clone, Debug)]
struct BoundBrowseRequest {
    context: BrowseContextKey,
    /// Canonical request directory retained by the owner for exact rechecks.
    request_root: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RequestBindingAdmission {
    Retained,
    NoRetainedObservation,
}

/// Bounded, shared immutable Cargo observations for currently admitted trees.
pub(super) struct BrowseCache {
    entries: HashMap<BrowseContextKey, CacheEntry>,
    bindings: HashMap<RequestBindingKey, BrowseContextKey>,
    requested_bindings: HashMap<[u8; 32], RequestBindingKey>,
    cached_bytes: usize,
    /// Includes retained rows and the bounded indexes. Keeping the policy on
    /// the cache lets small-capacity instances exercise the same admission.
    byte_budget: usize,
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
            byte_budget: MAX_BROWSE_CACHE_BYTES,
            use_clock: 0,
            #[cfg(test)]
            counters: BrowseCacheCounters::default(),
        }
    }
}

struct CacheEntry {
    witness: [u8; 32],
    watched: Vec<PathBuf>,
    input: Arc<TreeInput>,
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

#[derive(Clone)]
struct CachedRequestBinding {
    binding: backend_library::browse::ProjectTreeRequestBindingV1,
    /// Literal submitted address committed by the wire request. Never used as
    /// a Cargo invocation or source-file root after initial resolution.
    submitted_root: Box<Path>,
    /// Pinned physical Cargo invocation, independent of submitted spelling.
    request_root: PathBuf,
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
        self.project_tree_with_read_project(root, authority, read_project)
    }

    fn project_tree_with_read_project(
        &mut self,
        root: &Path,
        authority: Option<&backend_engine::advisory::AdvisoryAuthority>,
        read: impl FnMut(
            &RequestedCargoManifest,
            Option<&CargoToolWitnessReuse>,
        ) -> Result<CargoProjectRead, String>,
    ) -> Result<ProjectTree, String> {
        if !root.is_absolute() {
            return Err("project-tree needs an absolute project directory".to_owned());
        }
        if root
            .to_str()
            .is_none_or(|path| path.len() > backend_library::MAX_PRODUCT_TEXT_BYTES)
        {
            return Err("project-tree request directory exceeds the product path bound".to_owned());
        }
        // Resolve the physical invocation once, but bind the reply to the
        // literal submitted address. Source rechecks use this pinned directory
        // and separately refuse a submitted alias that now resolves elsewhere.
        let request_root = root
            .canonicalize()
            .map_err(|error| format!("cannot resolve Cargo request directory: {error}"))?;
        let input = self.input_with_read_project(&request_root, read)?;
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
        if !binding.matches_effective_workspace_root(&input.root) {
            return Err("Cargo project tree root differs from its retained observation".to_owned());
        }
        let context = BrowseContextKey {
            workspace: PathBuf::from(&input.root),
            invocation_root: request_root.clone(),
        };
        observation_budget()?;
        // Keep dependency-tree display available when an input is intentionally
        // not retained (for example, a lockfile fallback or cache-size limit),
        // but issue no source route without an exact owner-held observation.
        use backend_library::browse::ProjectTreeObservationV1;
        tree.observation = Some(
            match self.admit_request_binding(&context, binding, root.into(), request_root)? {
                RequestBindingAdmission::Retained => ProjectTreeObservationV1::Retained { binding },
                RequestBindingAdmission::NoRetainedObservation => {
                    ProjectTreeObservationV1::DisplayOnly { binding }
                }
            },
        );
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
        let Some(bound_request) = self.context_for_binding(request_binding) else {
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
        let input = match self.input(&bound_request.request_root) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_source_file(
                    Some(package),
                    Some(request_binding),
                    CargoPackageSourceReadFailureV1::SourceObservationUnavailable,
                );
            }
        };
        if !self.has_current_binding(&bound_request.context, request_binding) {
            return CargoPackageSourceFileResultV1::Stale {
                package,
                request_binding,
            };
        }
        let Some(package_row_index) = self.package_row_index(&bound_request.context, &package)
        else {
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
        let current = match self.input(&bound_request.request_root) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_source_file(
                    Some(package),
                    Some(request_binding),
                    CargoPackageSourceReadFailureV1::SourceObservationUnavailable,
                );
            }
        };
        let still_current = self.has_current_binding(&bound_request.context, request_binding)
            && self
                .package_row_index(&bound_request.context, &package)
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
        if second.as_slice() != contents.as_bytes()
            || !self.has_current_binding(&bound_request.context, request_binding)
        {
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
        let Some((bound_request, request_binding)) =
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
        if !self.has_current_binding(&bound_request.context, request_binding) {
            return CargoPackageReadmeResultV1::Stale {
                package,
                request_binding: Some(request_binding),
            };
        }
        let input = match self.input(&bound_request.request_root) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_package_readme(
                    Some(package),
                    Some(request_binding),
                    CargoPackageReadmeFailureV1::SourceObservationUnavailable,
                );
            }
        };
        if !self.has_current_binding(&bound_request.context, request_binding)
            || !request_binding.matches_effective_workspace_root(&input.root)
        {
            return CargoPackageReadmeResultV1::Stale {
                package,
                request_binding: Some(request_binding),
            };
        }
        let Some(package_row_index) = self.package_row_index(&bound_request.context, &package)
        else {
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
        let current = match self.input(&bound_request.request_root) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_package_readme(
                    Some(package),
                    Some(request_binding),
                    CargoPackageReadmeFailureV1::SourceObservationUnavailable,
                );
            }
        };
        if !self.has_current_binding(&bound_request.context, request_binding) {
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
        let still_current = self.has_current_binding(&bound_request.context, request_binding)
            && self
                .package_row_index(&bound_request.context, &package)
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
        let Some(bound_request) = self.context_for_binding(origin.request_binding) else {
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

        let input = match self.input(&bound_request.request_root) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_package_readme_link(
                    Some(origin),
                    CargoPackageReadmeLinkFailureV1::ObservationUnavailable,
                );
            }
        };
        if !self.has_current_binding(&bound_request.context, origin.request_binding)
            || !origin
                .request_binding
                .matches_effective_workspace_root(&input.root)
        {
            return CargoPackageReadmeLinkResultV1::Stale { origin };
        }
        let Some(package_row_index) =
            self.package_row_index(&bound_request.context, &origin.package)
        else {
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
            || !self.has_current_binding(&bound_request.context, origin.request_binding)
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
        let Some(bound_request) = self.context_for_binding(request_binding) else {
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
        let input = match self.input(&bound_request.request_root) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_source_inventory(
                    Some(package),
                    Some(request_binding),
                    CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
                );
            }
        };
        if !self.has_current_binding(&bound_request.context, request_binding) {
            return CargoPackageSourceInventoryResultV1::Stale {
                package,
                request_binding,
            };
        }
        let Some(package_row_index) = self.package_row_index(&bound_request.context, &package)
        else {
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
        let current = match self.input(&bound_request.request_root) {
            Ok(input) => input,
            Err(_) => {
                return unavailable_source_inventory(
                    Some(package),
                    Some(request_binding),
                    CargoPackageSourceInventoryFailureV1::AuthorityUnavailable,
                );
            }
        };
        let still_current = self.has_current_binding(&bound_request.context, request_binding)
            && self
                .package_row_index(&bound_request.context, &package)
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
        mut read: impl FnMut(
            &RequestedCargoManifest,
            Option<&CargoToolWitnessReuse>,
        ) -> Result<CargoProjectRead, String>,
    ) -> Result<Arc<TreeInput>, String> {
        let requested = requested_cargo_manifest(root)?.ok_or_else(|| {
            format!(
                "requested project {} is outside Cargo scope: its exact directory has no recognized Cargo package or workspace manifest; ancestor workspaces are not used for project-tree requests",
                root.display()
            )
        })?;

        // Each invocation directory keeps its own Cargo observation because
        // Cargo config, target selection, and tools can differ by CWD. Reuse
        // candidates by that exact canonical context, then let Cargo re-prove
        // membership, targets, and resolution before the cached graph is used.
        let candidates = self
            .entries
            .iter()
            .filter(|(context, _)| context.invocation_root == requested.root)
            .map(|(context, _)| context.clone())
            .collect::<Vec<_>>();
        let fresh = if let Some(context) = candidates.first() {
            let cached_tool = self
                .entries
                .get(context)
                .and_then(|entry| entry.tool_witness_reuse.clone());
            match read(&requested, cached_tool.as_ref()) {
                Ok(read) => Some(read),
                Err(error) => {
                    // Cancellation yields no freshness evidence. Keep the
                    // previous entry for a later request, which must still
                    // run a fresh Cargo observation before reusing it.
                    if !error.contains("cancelled") {
                        for context in &candidates {
                            self.remove_context(context);
                        }
                    }
                    return Err(error);
                }
            }
        } else {
            None
        };
        for context in candidates {
            let current = fresh
                .as_ref()
                .expect("candidate freshness read was performed");
            let matches = self.cached_bytes <= self.byte_budget
                && current.is_metadata_authority()
                && self.entries.get(&context).is_some_and(|entry| {
                    current.workspace_root() == context.workspace.as_path()
                        && current.witness() == entry.witness
                });
            observation_budget()?;
            if matches {
                self.touch_entry(&context);
                let entry = self
                    .entries
                    .get(&context)
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
            // Fresh Cargo output or its complete bounded input witness changed;
            // revoke only this invocation's graph. Other CWDs in the effective
            // workspace have distinct Cargo authorities and remain independent.
            self.remove_context(&context);
        }

        let invocation_root = requested.root.clone();
        let read = match fresh {
            Some(read) => read,
            None => read(&requested, None)?,
        };
        let witness = read.witness();
        let workspace = read.workspace_root().to_path_buf();
        let cacheable = read.is_metadata_authority();
        let read = read.into_input(&requested)?;
        if !tree_input_proves_requested_manifest(&read.input, &requested, &read.watched) {
            return Err(
                "Cargo metadata did not admit the exact requested package manifest in its resolved workspace".to_owned(),
            );
        }
        let canonical_workspace = workspace
            .canonicalize()
            .map_err(|_| "Cargo metadata returned a missing workspace root".to_owned())?;
        if !workspace.is_absolute() || canonical_workspace != workspace {
            return Err("Cargo metadata returned a noncanonical workspace root".to_owned());
        }
        let context = BrowseContextKey {
            workspace: workspace.clone(),
            invocation_root: invocation_root.clone(),
        };
        // The exact invocation context owns this snapshot. Keep other CWDs in
        // the same effective workspace; each is independently revalidated.
        self.remove_context(&context);
        let input = Arc::new(read.input);
        let package_rows = source_package_row_index(&input);
        #[cfg(test)]
        {
            self.counters.tree_input_allocations =
                self.counters.tree_input_allocations.saturating_add(1);
        }
        if !cacheable {
            return Ok(input);
        }
        let retained_bytes = browse_entry_retained_bytes(
            &workspace,
            &invocation_root,
            &read.watched,
            read.watched.capacity(),
            &input,
            &package_rows,
            read.tool_witness_reuse.as_ref(),
        );
        if retained_bytes <= self.byte_budget {
            while !self.workspace_cache_limit_allows(&workspace)
                || self.context_count_for_workspace(&workspace)
                    >= MAX_BROWSE_CACHED_CONTEXTS_PER_WORKSPACE
                || self.cached_bytes.saturating_add(retained_bytes) > self.byte_budget
            {
                let workspace_limit = !self.workspace_cache_limit_allows(&workspace);
                let context_limit = self.context_count_for_workspace(&workspace)
                    >= MAX_BROWSE_CACHED_CONTEXTS_PER_WORKSPACE;
                if workspace_limit {
                    let Some(oldest_workspace) = self.oldest_workspace() else {
                        break;
                    };
                    self.remove_workspace(&oldest_workspace);
                } else {
                    let oldest = if context_limit {
                        self.oldest_context_for_workspace(&workspace)
                    } else {
                        self.oldest_context()
                    };
                    let Some(oldest) = oldest else {
                        break;
                    };
                    #[cfg(test)]
                    if context_limit {
                        self.counters.request_binding_evictions =
                            self.counters.request_binding_evictions.saturating_add(
                                self.entries
                                    .get(&oldest)
                                    .map_or(0, |entry| entry.request_bindings.len()),
                            );
                    }
                    self.remove_context(&oldest);
                }
                #[cfg(test)]
                {
                    self.counters.evictions = self.counters.evictions.saturating_add(1);
                }
            }
            if self.workspace_cache_limit_allows(&workspace)
                && self.context_count_for_workspace(&workspace)
                    < MAX_BROWSE_CACHED_CONTEXTS_PER_WORKSPACE
                && self.cached_bytes.saturating_add(retained_bytes) <= self.byte_budget
            {
                let last_used = self.next_use();
                self.cached_bytes = self.cached_bytes.saturating_add(retained_bytes);
                self.entries.insert(
                    context,
                    CacheEntry {
                        witness,
                        watched: read.watched,
                        input: Arc::clone(&input),
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

    fn has_workspace(&self, workspace: &Path) -> bool {
        self.entries
            .keys()
            .any(|context| context.workspace.as_path() == workspace)
    }

    fn workspace_count(&self) -> usize {
        self.entries
            .keys()
            .map(|context| context.workspace.as_path())
            .collect::<BTreeSet<_>>()
            .len()
    }

    fn workspace_cache_limit_allows(&self, workspace: &Path) -> bool {
        let count = self.workspace_count();
        if self.has_workspace(workspace) {
            count <= MAX_BROWSE_CACHED_WORKSPACES
        } else {
            count < MAX_BROWSE_CACHED_WORKSPACES
        }
    }

    fn context_count_for_workspace(&self, workspace: &Path) -> usize {
        self.entries
            .keys()
            .filter(|context| context.workspace.as_path() == workspace)
            .count()
    }

    fn oldest_context(&self) -> Option<BrowseContextKey> {
        self.entries
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(context, _)| context.clone())
    }

    fn oldest_context_for_workspace(&self, workspace: &Path) -> Option<BrowseContextKey> {
        self.entries
            .iter()
            .filter(|(context, _)| context.workspace.as_path() == workspace)
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(context, _)| context.clone())
    }

    fn oldest_workspace(&self) -> Option<PathBuf> {
        let mut last_used_by_workspace = BTreeMap::<&Path, u64>::new();
        for (context, entry) in &self.entries {
            last_used_by_workspace
                .entry(context.workspace.as_path())
                .and_modify(|last_used| *last_used = (*last_used).max(entry.last_used))
                .or_insert(entry.last_used);
        }
        last_used_by_workspace
            .into_iter()
            .min_by_key(|(_, last_used)| *last_used)
            .map(|(workspace, _)| workspace.to_path_buf())
    }

    fn touch_entry(&mut self, context: &BrowseContextKey) {
        let last_used = self.next_use();
        if let Some(entry) = self.entries.get_mut(context) {
            entry.last_used = last_used;
        }
    }

    fn remove_workspace(&mut self, workspace: &Path) {
        let contexts = self
            .entries
            .keys()
            .filter(|context| context.workspace.as_path() == workspace)
            .cloned()
            .collect::<Vec<_>>();
        for context in contexts {
            self.remove_context(&context);
        }
    }

    fn remove_context(&mut self, context: &BrowseContextKey) {
        let Some(entry) = self.entries.remove(context) else {
            return;
        };
        self.cached_bytes = self.cached_bytes.saturating_sub(entry.retained_bytes);
        for (key, _) in entry.request_bindings {
            if self
                .bindings
                .get(&key)
                .is_some_and(|cached| cached == context)
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
        context: &BrowseContextKey,
        binding: backend_library::browse::ProjectTreeRequestBindingV1,
        submitted_root: Box<Path>,
        request_root: PathBuf,
    ) -> Result<RequestBindingAdmission, String> {
        if !binding.matches_requested_root(&submitted_root)
            || submitted_root
                .to_str()
                .is_none_or(|path| path.len() > backend_library::MAX_PRODUCT_TEXT_BYTES)
            || context.invocation_root != request_root
        {
            return Err(
                "request binding does not match the observed Cargo invocation root".to_owned(),
            );
        }
        if !self.entries.contains_key(context) {
            return Ok(RequestBindingAdmission::NoRetainedObservation);
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
            .is_some_and(|cached_context| cached_context != context)
        {
            self.remove_binding(key);
        }
        let binding_exists = self
            .entries
            .get(context)
            .is_some_and(|entry| entry.request_bindings.contains_key(&key));
        if !binding_exists
            && self.entries.get(context).is_some_and(|entry| {
                entry.request_bindings.len() >= MAX_BROWSE_REQUEST_BINDINGS_PER_CONTEXT
            })
        {
            let oldest = self.entries.get(context).and_then(|entry| {
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
        let workspace_binding_count = self
            .entries
            .iter()
            .filter(|(cached_context, _)| cached_context.workspace == context.workspace)
            .map(|(_, entry)| entry.request_bindings.len())
            .sum::<usize>();
        if !binding_exists && workspace_binding_count >= MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE {
            let oldest = self
                .entries
                .iter()
                .filter(|(cached_context, _)| cached_context.workspace == context.workspace)
                .flat_map(|(_, entry)| entry.request_bindings.iter())
                .min_by_key(|(_, binding)| binding.last_used)
                .map(|(key, _)| *key);
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
        let Some(entry) = self.entries.get_mut(context) else {
            return Ok(RequestBindingAdmission::NoRetainedObservation);
        };
        entry.request_bindings.insert(
            key,
            CachedRequestBinding {
                binding,
                submitted_root,
                request_root,
                last_used,
            },
        );
        entry.last_used = last_used;
        self.bindings.insert(key, context.clone());
        self.requested_bindings
            .insert(binding.requested_root_digest, key);
        Ok(RequestBindingAdmission::Retained)
    }

    fn remove_binding(&mut self, key: RequestBindingKey) {
        if let Some(context) = self.bindings.remove(&key)
            && let Some(entry) = self.entries.get_mut(&context)
        {
            entry.request_bindings.remove(&key);
        }
        if self.requested_bindings.get(&key.requested_root_digest) == Some(&key) {
            self.requested_bindings.remove(&key.requested_root_digest);
        }
    }

    fn context_for_binding(
        &mut self,
        binding: backend_library::browse::ProjectTreeRequestBindingV1,
    ) -> Option<BoundBrowseRequest> {
        let key = request_binding_key(binding);
        let context = self.bindings.get(&key)?.clone();
        if !self.has_current_binding(&context, binding) {
            return None;
        }
        let request_root = self
            .entries
            .get(&context)?
            .request_bindings
            .get(&key)?
            .request_root
            .clone();
        self.touch_binding(&context, key);
        Some(BoundBrowseRequest {
            context,
            request_root,
        })
    }

    fn binding_for_request(
        &mut self,
        requested_root_digest: [u8; 32],
        expected_workspace_root_digest: Option<[u8; 32]>,
    ) -> Option<(
        BoundBrowseRequest,
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
        let context = self.bindings.get(&key)?.clone();
        let binding = self
            .entries
            .get(&context)?
            .request_bindings
            .get(&key)?
            .binding;
        if request_binding_key(binding) != key {
            return None;
        }
        let request_root = self
            .entries
            .get(&context)?
            .request_bindings
            .get(&key)?
            .request_root
            .clone();
        self.touch_binding(&context, key);
        Some((
            BoundBrowseRequest {
                context,
                request_root,
            },
            binding,
        ))
    }

    fn has_current_binding(
        &self,
        context: &BrowseContextKey,
        binding: backend_library::browse::ProjectTreeRequestBindingV1,
    ) -> bool {
        self.entries
            .get(context)
            .and_then(|entry| entry.request_bindings.get(&request_binding_key(binding)))
            .is_some_and(|cached| {
                cached.binding == binding
                    && cached.request_root == context.invocation_root
                    && binding.matches_requested_root(&cached.submitted_root)
                    && cached
                        .submitted_root
                        .canonicalize()
                        .is_ok_and(|current| current == cached.request_root)
            })
            && self
                .bindings
                .get(&request_binding_key(binding))
                .is_some_and(|cached_context| cached_context == context)
    }

    fn touch_binding(&mut self, context: &BrowseContextKey, key: RequestBindingKey) {
        let last_used = self.next_use();
        if let Some(binding) = self
            .entries
            .get_mut(context)
            .and_then(|entry| entry.request_bindings.get_mut(&key))
        {
            binding.last_used = last_used;
        }
        if let Some(entry) = self.entries.get_mut(context) {
            entry.last_used = last_used;
        }
    }

    fn has_cached_authority(&self, package: &PackageReference) -> bool {
        let Some(digest) = CargoPackageSourceAuthorityV1::digest_from_package_reference(package)
        else {
            return false;
        };
        // Contexts share the same bounded workspace and byte budgets, so this
        // scans only a fixed maximum number of direct authority indexes.
        self.entries
            .values()
            .any(|entry| entry.package_rows.contains_key(&digest))
    }

    fn package_row_index(
        &self,
        context: &BrowseContextKey,
        package: &PackageReference,
    ) -> Option<usize> {
        let digest = CargoPackageSourceAuthorityV1::digest_from_package_reference(package)?;
        self.entries
            .get(context)?
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
    invocation_root: &PathBuf,
    watched: &[PathBuf],
    watched_capacity: usize,
    input: &TreeInput,
    package_rows: &HashMap<[u8; 32], Option<usize>>,
    tool_reuse: Option<&CargoToolWitnessReuse>,
) -> usize {
    // Each admitted request binding retains a workspace path clone in the
    // direct binding index. The cache-wide reserve accounts for all bounded
    // hash-map bucket allocations, including high-water capacity after eviction.
    let binding_path_budget = MAX_BROWSE_REQUEST_BINDINGS_PER_CONTEXT.saturating_mul(
        workspace
            .capacity()
            .saturating_add(invocation_root.capacity().saturating_mul(2))
            // A boxed literal path has no spare capacity. Admission bounds it
            // independently of its possibly much shorter canonical spelling.
            .saturating_add(backend_library::MAX_PRODUCT_TEXT_BYTES),
    );
    let mut bytes = size_of::<CacheEntry>()
        .saturating_add(workspace.capacity())
        .saturating_add(invocation_root.capacity())
        .saturating_add(1_024)
        .saturating_add(binding_path_budget)
        .saturating_add(size_of::<Arc<TreeInput>>())
        .saturating_add(size_of::<TreeInput>())
        .saturating_add(input.root.capacity())
        .saturating_add(input.packages.capacity() * size_of::<TreeInputPackage>())
        .saturating_add(input.edges.capacity() * size_of::<backend_library::browse::TreeEdge>())
        .saturating_add(package_rows.capacity().saturating_mul(256))
        .saturating_add(watched_capacity.saturating_mul(size_of::<PathBuf>()));
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
        bytes = bytes.saturating_add(tool_reuse.files.len() * size_of::<CargoToolFileReuse>());
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
            .saturating_add(package.categories.capacity() * size_of::<String>())
            .saturating_add(package.keywords.capacity() * size_of::<String>());
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
                .saturating_add(size_of_val(authority))
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
    // A relative parent component can mean an actual root escape. Report that
    // separately from a path that remains inside the root but is not in the
    // canonical wire format. Do not normalize an in-root `..`: it could cross
    // a symlink before reaching its lexical destination, and selected source
    // paths intentionally reject traversal spellings.
    let mut depth = 0_usize;
    for component in relative.components() {
        match component {
            std::path::Component::Normal(_) => depth = depth.saturating_add(1),
            std::path::Component::ParentDir if depth == 0 => {
                return Err(CargoPackageReadmeFailureV1::ReadmeOutsideAuthorizedRoot);
            }
            std::path::Component::ParentDir => depth -= 1,
            std::path::Component::CurDir
            | std::path::Component::RootDir
            | std::path::Component::Prefix(_) => {
                return Err(CargoPackageReadmeFailureV1::InvalidReadmePath);
            }
        }
    }
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

/// The exact owner-side components that guard a cached Cargo tree input.
struct ProjectInputRead {
    input: TreeInput,
    watched: Vec<PathBuf>,
    file_witness: [u8; 32],
    metadata_witness: [u8; 32],
    lock_origin_witness: [u8; 32],
    no_deps_witness: [u8; 32],
    tool_witness_reuse: Option<CargoToolWitnessReuse>,
}

impl ProjectInputRead {
    fn witness(&self) -> [u8; 32] {
        cargo_metadata::compose_cargo_input_witness(
            self.file_witness,
            self.metadata_witness,
            self.lock_origin_witness,
            self.no_deps_witness,
        )
    }
}

enum CargoProjectRead {
    Metadata(CoherentMetadata),
    LockfileFallback(ProjectInputRead),
    #[cfg(test)]
    TestMetadata(ProjectInputRead),
}

impl CargoProjectRead {
    fn witness(&self) -> [u8; 32] {
        match self {
            Self::Metadata(metadata) => metadata.witness(),
            Self::LockfileFallback(input) => input.witness(),
            #[cfg(test)]
            Self::TestMetadata(input) => input.witness(),
        }
    }

    fn workspace_root(&self) -> &Path {
        match self {
            Self::Metadata(metadata) => &metadata.workspace_root,
            Self::LockfileFallback(input) => Path::new(&input.input.root),
            #[cfg(test)]
            Self::TestMetadata(input) => Path::new(&input.input.root),
        }
    }

    fn is_metadata_authority(&self) -> bool {
        match self {
            Self::Metadata(_) => true,
            Self::LockfileFallback(_) => false,
            #[cfg(test)]
            Self::TestMetadata(_) => true,
        }
    }

    fn into_input(self, requested: &RequestedCargoManifest) -> Result<ProjectInputRead, String> {
        match self {
            Self::Metadata(metadata) => project_input_from_metadata(requested, metadata),
            Self::LockfileFallback(input) => Ok(input),
            #[cfg(test)]
            Self::TestMetadata(input) => Ok(input),
        }
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
    workspace_root: PathBuf,
    input_witness: [u8; 32],
    file_witness: [u8; 32],
    metadata_witness: [u8; 32],
    lock_origin_witness: [u8; 32],
    no_deps_witness: [u8; 32],
    tool_witness_reuse: Option<CargoToolWitnessReuse>,
    lockfile: Option<String>,
    watched: Vec<PathBuf>,
}

impl CoherentMetadata {
    fn witness(&self) -> [u8; 32] {
        self.input_witness
    }
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

/// Reads fresh Cargo metadata even on a cache candidate. The first metadata
/// call discovers Cargo's exact package and target paths; the second is
/// bracketed by a bounded no-follow read-set witness. Cache hits compare the
/// full metadata response and lock origin, so Cargo remains the authority for
/// membership, target discovery, and dependency resolution.
fn read_project(
    requested: &RequestedCargoManifest,
    cached_tool: Option<&CargoToolWitnessReuse>,
) -> Result<CargoProjectRead, String> {
    match cargo_metadata::coherent_metadata(requested, cached_tool) {
        Ok(observed) => Ok(CargoProjectRead::Metadata(observed)),
        Err(reason) => {
            if reason.contains("cancelled") {
                return Err(reason);
            }
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
            let selected_lockfile = cargo_metadata::cargo_selected_lockfile_path(
                workspace, workspace,
            )
            .map_err(|error| {
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
            let lock_origin_witness = cargo_metadata::cargo_lock_origin_witness(
                cargo_metadata::CargoMetadataLockOrigin::Observed,
                &workspace.join("Cargo.lock"),
                *blake3::hash(lockfile.as_bytes()).as_bytes(),
            );
            Ok(CargoProjectRead::LockfileFallback(ProjectInputRead {
                input,
                watched,
                file_witness: observed.digest,
                metadata_witness: [0; 32],
                lock_origin_witness,
                no_deps_witness: [0; 32],
                tool_witness_reuse: observed.tool_witness_reuse,
            }))
        }
    }
}

fn project_input_from_metadata(
    requested: &RequestedCargoManifest,
    observed: CoherentMetadata,
) -> Result<ProjectInputRead, String> {
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
        file_witness: observed.file_witness,
        metadata_witness: observed.metadata_witness,
        lock_origin_witness: observed.lock_origin_witness,
        no_deps_witness: observed.no_deps_witness,
        tool_witness_reuse: observed.tool_witness_reuse,
    })
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
    let paths = vec![workspace.join("Cargo.lock"), workspace.join("Cargo.toml")];
    Ok(paths)
}

/// Captures inherited Cargo config candidates, recursive `include` inputs,
/// and configured custom target JSONs. Unresolvable includes/target files
/// fail closed instead of leaving an unobserved input outside the witness.
fn cargo_config_paths(request_context: &Path) -> Result<Vec<PathBuf>, String> {
    let cargo_home = match std::env::var_os("CARGO_HOME") {
        Some(home) => PathBuf::from(home),
        None => std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| {
                "Cargo home cannot be resolved for configuration observation".to_owned()
            })?
            .join(".cargo"),
    };
    let environment_target = std::env::var_os("CARGO_BUILD_TARGET");
    cargo_config_paths_with(request_context, &cargo_home, environment_target.as_deref())
}

fn cargo_config_paths_with(
    request_context: &Path,
    cargo_home: &Path,
    environment_target: Option<&std::ffi::OsStr>,
) -> Result<Vec<PathBuf>, String> {
    if !cargo_home.is_absolute() {
        return Err("relative CARGO_HOME cannot be safely observed".to_owned());
    }
    // These are candidate config locations, so absence is normal. Included
    // locations carry their own optional bit from Cargo's config schema.
    let mut pending = Vec::<(PathBuf, usize, bool)>::new();
    let ancestors = request_context.ancestors().take(128).collect::<Vec<_>>();
    if ancestors.len() == 128 && ancestors.last().is_some_and(|path| path.parent().is_some()) {
        return Err("Cargo config ancestor chain exceeds its limit".to_owned());
    }
    for ancestor in ancestors {
        pending.push((ancestor.join(".cargo/config"), 0, true));
        pending.push((ancestor.join(".cargo/config.toml"), 0, true));
    }
    pending.push((cargo_home.join("config"), 0, true));
    pending.push((cargo_home.join("config.toml"), 0, true));

    let mut paths = BTreeSet::new();
    let mut presence = BTreeMap::<PathBuf, bool>::new();
    while let Some((path, depth, missing_is_allowed)) = pending.pop() {
        observation_budget()?;
        if paths.contains(&path) {
            if !missing_is_allowed && presence.get(&path) == Some(&false) {
                return Err(format!(
                    "required Cargo config include is missing: {}",
                    path.display()
                ));
            }
            continue;
        }
        paths.insert(path.clone());
        if paths.len() > MAX_CARGO_CONFIG_INPUTS {
            return Err("Cargo configuration input set exceeds its limit".to_owned());
        }
        let Some(bytes) = read_observation_file(&path, MAX_CARGO_CONFIG_BYTES)? else {
            presence.insert(path.clone(), false);
            if !missing_is_allowed {
                return Err(format!(
                    "required Cargo config include is missing: {}",
                    path.display()
                ));
            }
            continue;
        };
        presence.insert(path.clone(), true);
        let document: toml::Value = std::str::from_utf8(&bytes)
            .map_err(|_| "Cargo config is not UTF-8".to_owned())?
            .parse()
            .map_err(|error: toml::de::Error| format!("Cargo config is malformed: {error}"))?;
        if let Some(include) = document.get("include") {
            if depth >= MAX_CARGO_CONFIG_DEPTH {
                return Err("Cargo config include depth exceeds its limit".to_owned());
            }
            let entries = cargo_config_include_entries(include)?;
            if entries.len() > 64 {
                return Err("Cargo config has too many included files".to_owned());
            }
            let parent = path
                .parent()
                .ok_or_else(|| "Cargo config has no parent".to_owned())?;
            for (value, optional) in entries {
                if value.len() > 4 * 1024 || value.contains('\0') {
                    return Err("Cargo config include path is out of bounds".to_owned());
                }
                let included = PathBuf::from(value);
                if included.extension() != Some(std::ffi::OsStr::new("toml")) {
                    return Err("Cargo config include must end in .toml".to_owned());
                }
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
                    optional,
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
                    &cargo_config_relative_path_base(&path)?,
                    target,
                    &mut paths,
                )?;
            }
        }
    }
    if let Some(target) = environment_target {
        let target = target
            .to_str()
            .ok_or_else(|| "CARGO_BUILD_TARGET is not UTF-8".to_owned())?;
        observe_custom_target_path(request_context, target, &mut paths)?;
    }
    Ok(paths.into_iter().collect())
}

/// Reads Cargo's supported `include` array forms without silently accepting
/// keys that Cargo may interpret differently: each item is either a path
/// string or a `{ path, optional }` table.
fn cargo_config_include_entries(include: &toml::Value) -> Result<Vec<(String, bool)>, String> {
    let values = include
        .as_array()
        .ok_or_else(|| "Cargo config include must be an array".to_owned())?;
    if values.len() > 64 {
        return Err("Cargo config has too many included files".to_owned());
    }
    values
        .iter()
        .map(|value| {
            if let Some(path) = value.as_str() {
                return Ok((path.to_owned(), false));
            }
            let table = value
                .as_table()
                .ok_or_else(|| "Cargo config include entry must be a string or table".to_owned())?;
            if table.keys().any(|key| key != "path" && key != "optional") {
                return Err("Cargo config include table has an unsupported field".to_owned());
            }
            let path = table
                .get("path")
                .and_then(toml::Value::as_str)
                .ok_or_else(|| "Cargo config include table requires a string path".to_owned())?;
            let optional = table
                .get("optional")
                .map(|value| {
                    value.as_bool().ok_or_else(|| {
                        "Cargo config include optional field must be a boolean".to_owned()
                    })
                })
                .transpose()?
                .unwrap_or(false);
            Ok((path.to_owned(), optional))
        })
        .collect()
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
    relative_base: &Path,
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
    let resolved = if target.is_absolute() {
        target
    } else {
        relative_base.join(&target)
    };
    if read_observation_file(&resolved, MAX_CARGO_OBSERVATION_FILE_BYTES)?.is_none() {
        return Err(
            "Cargo custom target JSON could not be located for source observation".to_owned(),
        );
    }
    observed_paths.insert(resolved);
    if observed_paths.len() > MAX_CARGO_CONFIG_INPUTS {
        return Err("Cargo configuration input set exceeds its limit".to_owned());
    }
    Ok(())
}

/// Cargo anchors paths from a config file two levels above that file, matching
/// the directory above the `.cargo` directory for hierarchical configs.
fn cargo_config_relative_path_base(config_path: &Path) -> Result<PathBuf, String> {
    config_path
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| "Cargo config path has no relative-path base".to_owned())
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
    run_with_default_rustc_and_overrides(program, directory, arguments, maximum, default_rustc, &[])
}

fn run_with_default_rustc_and_overrides(
    program: &Path,
    directory: &Path,
    arguments: &[&str],
    maximum: usize,
    default_rustc: Option<&Path>,
    environment_overrides: &[(std::ffi::OsString, std::ffi::OsString)],
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
    let mut overrides = default_rustc
        .map(|rustc| vec![("RUSTC".into(), Some(rustc.as_os_str().to_os_string()))])
        .unwrap_or_default();
    overrides.extend(
        environment_overrides
            .iter()
            .map(|(name, value)| (name.clone(), Some(value.clone()))),
    );
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
        overrides,
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
        ProjectInputRead {
            input,
            watched,
            file_witness,
            metadata_witness: [0; 32],
            lock_origin_witness: [0; 32],
            no_deps_witness: [0; 32],
            tool_witness_reuse,
        }
    }

    fn plant_fixture_cache_entry(
        cache: &mut BrowseCache,
        input: TreeInput,
        watched: Vec<PathBuf>,
        invocation_root: &Path,
    ) {
        let workspace = PathBuf::from(&input.root);
        let context = BrowseContextKey {
            workspace: workspace.clone(),
            invocation_root: invocation_root.to_path_buf(),
        };
        let lock_origin_witness = [0; 32];
        let input = Arc::new(input);
        let package_rows = source_package_row_index(&input);
        let file_witness =
            observation_witness_for_context(&workspace, invocation_root, &watched, None)
                .expect("fixture observation witness")
                .digest;
        let witness = cargo_metadata::compose_cargo_input_witness(
            file_witness,
            [0; 32],
            lock_origin_witness,
            [0; 32],
        );
        let retained_bytes = browse_entry_retained_bytes(
            &workspace,
            &invocation_root.to_path_buf(),
            &watched,
            watched.capacity(),
            &input,
            &package_rows,
            None,
        );
        cache.remove_context(&context);
        cache.cached_bytes = cache.cached_bytes.saturating_add(retained_bytes);
        cache.entries.insert(
            context,
            CacheEntry {
                witness,
                watched,
                input,
                retained_bytes,
                package_rows,
                request_bindings: HashMap::new(),
                tool_witness_reuse: None,
                last_used: 1,
            },
        );
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

        let fresh_member_watched = vec![
            workspace.join("Cargo.lock"),
            workspace.join("Cargo.toml"),
            member.join("Cargo.toml"),
        ];
        let reused_member = cache
            .input_with_read_project(&member, |requested, _cached| {
                let input = fixture_member_input(&workspace, &member, "member 0.1.0 (fixture)");
                let file_witness = observation_witness_for_context(
                    &workspace,
                    &requested.root,
                    &fresh_member_watched,
                    None,
                )
                .expect("fresh exact-manifest observation")
                .digest;
                Ok(CargoProjectRead::TestMetadata(project_input_read_for_test(
                    input,
                    fresh_member_watched.clone(),
                    file_witness,
                    None,
                )))
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
                    "packages": [{ "id": id, "manifest_path": requested.manifest.clone(), "dependencies": [], "targets": [] }]
                }))
                .expect("standalone Cargo-shaped metadata");
                assert_eq!(
                    cargo_metadata::metadata_proves_requested_manifest(&metadata, requested)
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
                Ok(CargoProjectRead::TestMetadata(project_input_read_for_test(
                    input, watched, witness, None,
                )))
            })
            .expect("standalone package is resolved from its own exact manifest");

        assert_eq!(
            metadata_calls, 1,
            "the ancestor cache cannot answer this request"
        );
        assert_eq!(standalone.root, excluded.to_string_lossy().into_owned());
        assert!(
            cache.has_workspace(&workspace),
            "the ancestor entry remains reusable for its members"
        );
        assert!(
            cache.has_workspace(&excluded),
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
                Ok(CargoProjectRead::TestMetadata(project_input_read_for_test(
                    input,
                    watched.clone(),
                    witness,
                    None,
                )))
            })
            .expect("member context must be resolved independently");

        assert_eq!(
            metadata_calls, 1,
            "the workspace-context cache cannot answer"
        );
        assert_eq!(member_input.root, workspace.to_string_lossy().into_owned());
        assert!(cache.has_workspace(&workspace));
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
        std::fs::create_dir_all(package.join("src")).expect("member source directory");
        std::fs::write(package.join("src/lib.rs"), "pub fn tool() {}\n")
            .expect("initial member target");
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
        let old_binding = old_reply.retained_request_binding().expect("bound request");
        let old_context = cache
            .bindings
            .get(&request_binding_key(old_binding))
            .expect("old request's exact context")
            .clone();
        assert!(cache.has_current_binding(&old_context, old_binding));

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
                    "packages": [{ "id": id, "manifest_path": requested.manifest.clone(), "dependencies": [], "targets": [] }]
                }))
                .expect("Cargo-shaped replacement metadata");
                assert_eq!(
                    cargo_metadata::metadata_proves_requested_manifest(&metadata, requested)
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
                Ok(CargoProjectRead::TestMetadata(project_input_read_for_test(
                    input, watched, witness, None,
                )))
            })
            .expect("replacement manifest resolves through Cargo metadata");

        assert_eq!(metadata_calls, 1);
        assert_eq!(updated.root, external.to_string_lossy().into_owned());
        assert!(!cache.has_current_binding(&old_context, old_binding));
        assert!(
            !cache.has_workspace(&workspace),
            "old workspace entry was revoked"
        );
        assert!(
            cache.has_workspace(&external),
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
    fn warm_cache_rejects_changed_registry_git_and_patch_resolution_rows() {
        let scratch = scratch("backend-browse-registry-git-refresh");
        let root = scratch.0.join("workspace/app");
        std::fs::create_dir_all(&root).expect("app directory");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"cache-root\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("app manifest");
        std::fs::write(root.join("Cargo.lock"), "version = 4\n").expect("lockfile");
        let root = root.canonicalize().expect("canonical app root");
        let requested = requested_cargo_manifest(&root)
            .expect("exact app manifest")
            .expect("app package");
        let registry_manifest = scratch.0.join("registry/cache-indexed/Cargo.toml");
        let git_manifest = scratch.0.join("git/cache-git/Cargo.toml");
        let patch_manifest = scratch.0.join("patches/cache-patch/Cargo.toml");
        let watched = vec![root.join("Cargo.lock"), requested.manifest.clone()];
        std::fs::create_dir_all(patch_manifest.parent().expect("patch parent"))
            .expect("unselected patch candidate directory");
        std::fs::write(
            &patch_manifest,
            "[package]\nname = \"cache-patch\"\nversion = \"1.2.0\"\nedition = \"2021\"\n",
        )
        .expect("unselected patch candidate manifest");
        assert!(!watched.contains(&patch_manifest));

        fn selected_rows(
            requested: &RequestedCargoManifest,
            registry_manifest: &Path,
            git_manifest: &Path,
            patch_manifest: &Path,
            registry_version: &str,
            git_revision: &str,
            patch_version: Option<&str>,
            watched: &[PathBuf],
        ) -> Result<CargoProjectRead, String> {
            let mut package_rows = vec![
                serde_json::json!({
                    "id": "cache-root 0.1.0 (path+file:///cache-root)",
                    "name": "cache-root",
                    "version": "0.1.0",
                    "manifest_path": requested.manifest,
                    "dependencies": [],
                    "source": null,
                    "targets": []
                }),
                serde_json::json!({
                    "id": format!("cache-indexed {registry_version} (registry+https://index.example)"),
                    "name": "cache-indexed",
                    "version": registry_version,
                    "manifest_path": registry_manifest,
                    "dependencies": [],
                    "source": "registry+https://index.example",
                    "targets": []
                }),
                serde_json::json!({
                    "id": format!("cache-git {git_revision} (git+https://git.example/cache-git#{git_revision})"),
                    "name": "cache-git",
                    "version": "0.4.0",
                    "manifest_path": git_manifest,
                    "dependencies": [],
                    "source": format!("git+https://git.example/cache-git?rev={git_revision}#{git_revision}"),
                    "targets": []
                }),
            ];
            if let Some(version) = patch_version {
                package_rows.push(serde_json::json!({
                    "id": format!("cache-patch {version} (path+file:///patches/cache-patch)"),
                    "name": "cache-patch",
                    "version": version,
                    "manifest_path": patch_manifest,
                    "dependencies": [],
                    "source": null,
                    "targets": []
                }));
            }
            let metadata = serde_json::to_vec(&serde_json::json!({
                "workspace_root": requested.root,
                "workspace_members": ["cache-root 0.1.0 (path+file:///cache-root)"],
                "packages": package_rows
            }))
            .map_err(|error| error.to_string())?;
            let file_witness =
                observation_witness_for_context(&requested.root, &requested.root, watched, None)?
                    .digest;
            let mut input = fixture_member_input(
                &requested.root,
                &requested.root,
                "cache-root 0.1.0 (fresh metadata)",
            );
            input.packages.extend([
                TreeInputPackage {
                    id: format!("cache-indexed {registry_version} (registry)"),
                    name: "cache-indexed".to_owned(),
                    version: registry_version.to_owned(),
                    member: false,
                    has_bin: false,
                    origin: None,
                    source_root: None,
                    source_authority: CargoPackageSourceAuthorityStateV1::Unavailable(
                        CargoPackageSourceAuthorityFailureV1::NotObserved,
                    ),
                    license: None,
                    description: None,
                    categories: Vec::new(),
                    keywords: Vec::new(),
                },
                TreeInputPackage {
                    id: format!("cache-git {git_revision} (git)"),
                    name: "cache-git".to_owned(),
                    version: "0.4.0".to_owned(),
                    member: false,
                    has_bin: false,
                    origin: None,
                    source_root: None,
                    source_authority: CargoPackageSourceAuthorityStateV1::Unavailable(
                        CargoPackageSourceAuthorityFailureV1::NotObserved,
                    ),
                    license: None,
                    description: None,
                    categories: Vec::new(),
                    keywords: Vec::new(),
                },
            ]);
            if let Some(version) = patch_version {
                input.packages.push(TreeInputPackage {
                    id: format!("cache-patch {version} (path)"),
                    name: "cache-patch".to_owned(),
                    version: version.to_owned(),
                    member: false,
                    has_bin: false,
                    origin: None,
                    source_root: patch_manifest.parent().map(Path::to_path_buf),
                    source_authority: CargoPackageSourceAuthorityStateV1::Unavailable(
                        CargoPackageSourceAuthorityFailureV1::NotObserved,
                    ),
                    license: None,
                    description: None,
                    categories: Vec::new(),
                    keywords: Vec::new(),
                });
            }
            let mut read = project_input_read_for_test(input, watched.to_vec(), file_witness, None);
            read.metadata_witness =
                cargo_metadata::cargo_metadata_output_witness(&metadata, "fixture-host");
            Ok(CargoProjectRead::TestMetadata(read))
        }

        let mut cache = BrowseCache::default();
        let first = cache
            .input_with_read_project(&root, |requested, _| {
                selected_rows(
                    requested,
                    &registry_manifest,
                    &git_manifest,
                    &patch_manifest,
                    "1.0.0",
                    "aaaaaaaa",
                    None,
                    &watched,
                )
            })
            .expect("initial selected registry and git rows");
        let unchanged = cache
            .input_with_read_project(&root, |requested, _| {
                selected_rows(
                    requested,
                    &registry_manifest,
                    &git_manifest,
                    &patch_manifest,
                    "1.0.0",
                    "aaaaaaaa",
                    None,
                    &watched,
                )
            })
            .expect("unchanged exact Cargo output reuses its graph");
        assert!(Arc::ptr_eq(&first, &unchanged));
        let updated = cache
            .input_with_read_project(&root, |requested, _| {
                selected_rows(
                    requested,
                    &registry_manifest,
                    &git_manifest,
                    &patch_manifest,
                    "1.1.0",
                    "bbbbbbbb",
                    None,
                    &watched,
                )
            })
            .expect("updated resolver output replaces the warm graph");
        assert_eq!(updated.packages[1].version, "1.1.0");
        assert_eq!(updated.packages[2].id, "cache-git bbbbbbbb (git)");
        let patch_candidate = cache
            .input_with_read_project(&root, |requested, _| {
                selected_rows(
                    requested,
                    &registry_manifest,
                    &git_manifest,
                    &patch_manifest,
                    "1.1.0",
                    "bbbbbbbb",
                    Some("1.2.0"),
                    &watched,
                )
            })
            .expect("Cargo selects a changed local patch candidate outside the old graph");
        assert_eq!(patch_candidate.packages[3].name, "cache-patch");
        assert_eq!(patch_candidate.packages[3].version, "1.2.0");
        assert_eq!(cache.counters.cache_hits, 1);
        assert_eq!(cache.counters.tree_input_allocations, 3);
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
        assert_eq!(
            manifest_readme_path(&workspace, "nested/../README.md"),
            Err(CargoPackageReadmeFailureV1::InvalidReadmePath),
            "in-root traversal remains inadmissible even when it does not escape"
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
        let binding = tree.retained_request_binding().expect("exact project tree request");
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

    #[cfg(unix)]
    #[test]
    fn project_tree_binds_submitted_alias_but_keeps_cargo_pinned_during_retarget() {
        fn write_package(root: &Path, name: &str, contents: &str) {
            std::fs::create_dir_all(root.join("src")).expect("package source directory");
            std::fs::write(
                root.join("Cargo.toml"),
                format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
            )
            .expect("package manifest");
            std::fs::write(
                root.join("Cargo.lock"),
                format!("version = 4\n\n[[package]]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
            )
            .expect("package lockfile");
            std::fs::write(root.join("src/lib.rs"), contents).expect("package source file");
        }

        let scratch = scratch("backend-browse-canonical-request-root");
        let a = scratch.0.join("a");
        let b = scratch.0.join("b");
        let alias = scratch.0.join("request");
        write_package(&a, "canonical-a", "pub fn selected() { /* A */ }\n");
        write_package(&b, "canonical-b", "pub fn selected() { /* B */ }\n");
        let a = a.canonicalize().expect("canonical package A");
        let b = b.canonicalize().expect("canonical package B");
        std::os::unix::fs::symlink(&a, &alias).expect("request symlink to A");

        let mut owner = BrowseCache::default();
        let mut changed_link = false;
        let tree_a = owner
            .project_tree_with_read_project(&alias, None, |requested, cached_tool| {
                assert_eq!(requested.root, a);
                if !changed_link {
                    std::fs::remove_file(&alias).expect("remove request symlink to A");
                    std::os::unix::fs::symlink(&b, &alias)
                        .expect("change request symlink to B during Cargo admission");
                    changed_link = true;
                }
                read_project(requested, cached_tool)
            })
            .expect("Cargo observation remains bound to canonical package A");
        assert!(changed_link);
        assert_eq!(alias.canonicalize().expect("updated request symlink"), b);
        assert_eq!(tree_a.root, a.to_string_lossy().into_owned());
        let binding_a = tree_a.retained_request_binding().expect("retained A request binding");
        assert_eq!(
            binding_a,
            backend_library::browse::ProjectTreeRequestBindingV1::for_paths(&alias, &tree_a.root)
                .expect("binding for the exact submitted alias request")
        );
        let context_a = BrowseContextKey {
            workspace: a.clone(),
            invocation_root: a.clone(),
        };
        assert!(!owner.has_current_binding(&context_a, binding_a));

        let request_a = {
            let entry = owner.entries.get(&context_a).expect("retained A context");
            let authority = entry
                .input
                .packages
                .iter()
                .find_map(|row| match &row.source_authority {
                    CargoPackageSourceAuthorityStateV1::Admitted(authority)
                        if authority.name() == "canonical-a" =>
                    {
                        Some(authority)
                    }
                    _ => None,
                })
                .expect("A package source authority");
            backend_library::CargoPackageSourceRequestV1::from_tree(
                authority
                    .package_reference()
                    .expect("exact A package reference"),
                binding_a,
            )
        };
        let source_a = owner.source_file(
            request_a.clone(),
            CargoPackageSourcePathV1::new("src/lib.rs").expect("source path"),
        );
        assert!(matches!(
            source_a,
            CargoPackageSourceFileResultV1::Stale { .. }
        ));

        let tree_b = owner
            .project_tree(&alias, None)
            .expect("later request follows the changed symlink to B");
        assert_eq!(tree_b.root, b.to_string_lossy().into_owned());
        let binding_b = tree_b.retained_request_binding().expect("retained B request binding");
        assert_eq!(binding_a.requested_root_digest, binding_b.requested_root_digest);
        assert_ne!(binding_a.effective_workspace_root_digest, binding_b.effective_workspace_root_digest);
        assert!(matches!(
            owner.source_file(
                request_a,
                CargoPackageSourcePathV1::new("src/lib.rs").expect("source path"),
            ),
            CargoPackageSourceFileResultV1::Stale { .. }
        ));
        assert_eq!(owner.workspace_count(), 2);
    }

    #[test]
    fn project_tree_lockfile_observation_remains_wire_readable_without_source_capability() {
        let scratch = scratch("backend-browse-unretained-observation");
        let root = scratch.0.join("project");
        std::fs::create_dir_all(&root).expect("project directory");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"unretained\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("package manifest");
        let root = root.canonicalize().expect("canonical project root");
        let requested_manifest = root.join("Cargo.toml");
        let mut input = fixture_member_input(&root, &root, "unretained");
        input.source = TreeSource::Lockfile {
            reason: "Cargo unavailable".to_owned(),
            coverage: LockfileGraphCoverage::Partial {
                ambiguous_edges: 0,
                ambiguous_package_rows: 0,
            },
            workspace_membership: LockfileWorkspaceMembership::Unknown,
        };
        let fallback = ProjectInputRead {
            input,
            watched: vec![requested_manifest],
            file_witness: [1; 32],
            metadata_witness: [0; 32],
            lock_origin_witness: [2; 32],
            no_deps_witness: [0; 32],
            tool_witness_reuse: None,
        };
        let mut fallback = Some(fallback);
        let mut owner = BrowseCache::default();
        let tree = owner
            .project_tree_with_read_project(&root, None, |_, _| {
                Ok(CargoProjectRead::LockfileFallback(
                    fallback.take().expect("one fallback observation"),
                ))
            })
            .expect("lockfile-only tree remains displayable");

        assert!(tree.retained_request_binding().is_none());
        let binding = tree
            .request_binding()
            .expect("display-only exact request identity");
        assert!(matches!(
            tree.observation,
            Some(backend_library::browse::ProjectTreeObservationV1::DisplayOnly { .. })
        ));
        assert!(binding.matches_requested_root(&root));
        assert!(binding.matches_effective_workspace_root(&tree.root));
        let reply = backend_library::SurfaceReply::ProjectTree(Box::new(tree));
        assert!(
            reply.admit(backend_library::CommandId::ProjectTree).is_ok(),
            "the owner-attached display-only fallback must cross the product boundary"
        );
        let dto = backend_library::ReplyDto::new(31, backend_library::CommandReply::Surface(reply));
        let decoded =
            backend_library::decode_reply_body(&serde_json::to_vec(&dto).expect("fallback wire"))
                .expect("the strict client admits the display-only Tree");
        assert_eq!(decoded.reply, dto.reply);
        assert!(owner.entries.is_empty());
        assert!(owner.bindings.is_empty());
        assert!(owner.requested_bindings.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn submitted_alias_roundtrips_exactly_and_retarget_revokes_source_inventory_and_readme() {
        let scratch = scratch("backend-browse-literal-alias");
        let a = scratch.0.join("a");
        let b = scratch.0.join("b");
        for (root, name) in [(&a, "literal-a"), (&b, "literal-b")] {
            std::fs::create_dir_all(root.join("src")).expect("physical package source");
            std::fs::write(root.join("Cargo.toml"), format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\nreadme = \"README.md\"\n"
            )).expect("physical package manifest");
            std::fs::write(
                root.join("Cargo.lock"),
                format!("version = 4\n\n[[package]]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
            )
            .expect("locked physical package");
            std::fs::write(
                root.join("src/lib.rs"),
                format!("pub fn {name}() {{}}\n").replace('-', "_"),
            )
            .expect("physical source bytes");
            std::fs::write(root.join("README.md"), format!("# {name}\n"))
                .expect("manifest-selected README");
        }
        let a = a.canonicalize().expect("canonical A");
        let b = b.canonicalize().expect("canonical B");
        let alias = scratch.0.join("submitted");
        std::os::unix::fs::symlink(&a, &alias).expect("submitted alias to A");
        let request = |root: &Path| {
            backend_library::CommandDto::new(
                63,
                backend_library::Command::Surface(backend_library::SurfaceCommand::ProjectTree {
                    root: backend_library::ProductText::new(root.to_str().expect("UTF-8 request"))
                        .expect("bounded exact submitted address"),
                }),
            )
        };
        let roundtrip = |tree: &ProjectTree| {
            let reply = backend_library::ReplyDto::new(
                63,
                backend_library::CommandReply::Surface(backend_library::SurfaceReply::ProjectTree(
                    Box::new(tree.clone()),
                )),
            );
            backend_library::decode_reply_body(
                &serde_json::to_vec(&reply).expect("owner reply wire"),
            )
            .expect("strict current tree DTO")
        };
        let mut owner = BrowseCache::default();
        let tree = owner
            .project_tree(&alias, None)
            .expect("real aliased Cargo observation");
        let binding = tree
            .retained_request_binding()
            .expect("retained alias observation");
        assert!(binding.matches_requested_root(&alias));
        assert!(
            !binding.matches_requested_root(&a),
            "physical equivalence cannot replace submitted identity"
        );
        assert!(binding.matches_effective_workspace_root(a.to_str().expect("canonical root")));
        let wire = roundtrip(&tree);
        assert!(backend_library::admit_reply(&request(&alias), &wire).is_ok());
        assert!(backend_library::admit_reply(&request(&a), &wire).is_err());
        let context = BrowseContextKey {
            workspace: a.clone(),
            invocation_root: a.clone(),
        };
        let package = owner
            .entries
            .get(&context)
            .expect("owner-held physical A observation")
            .input
            .packages
            .iter()
            .find_map(|row| match &row.source_authority {
                CargoPackageSourceAuthorityStateV1::Admitted(authority)
                    if authority.name() == "literal-a" =>
                {
                    authority.package_reference().ok()
                }
                _ => None,
            })
            .expect("exact physical A package");
        let source_request =
            backend_library::CargoPackageSourceRequestV1::from_tree(package.clone(), binding);
        let readme_request = CargoPackageReadmeRequestV1::from_tree(package.clone(), binding);
        let path = CargoPackageSourcePathV1::new("src/lib.rs").expect("source path");
        assert!(
            matches!(owner.source_file(source_request.clone(), path.clone()),
            CargoPackageSourceFileResultV1::Read { contents, .. } if contents.as_ref() == "pub fn literal_a() {}\n")
        );
        assert!(matches!(
            owner.source_inventory(source_request.clone()),
            CargoPackageSourceInventoryResultV1::Listed(_)
        ));
        assert!(matches!(owner.package_readme(readme_request.clone()),
            CargoPackageReadmeResultV1::Read { ref readme, .. } if readme.contents.as_ref() == "# literal-a\n"));
        std::fs::remove_file(&alias).expect("remove submitted alias to A");
        std::os::unix::fs::symlink(&b, &alias).expect("retarget submitted alias to B");
        assert!(!owner.has_current_binding(&context, binding));
        assert!(matches!(
            owner.source_file(source_request.clone(), path),
            CargoPackageSourceFileResultV1::Stale { .. }
        ));
        assert!(matches!(
            owner.source_inventory(source_request),
            CargoPackageSourceInventoryResultV1::Stale { .. }
        ));
        assert!(matches!(
            owner.package_readme(readme_request),
            CargoPackageReadmeResultV1::Stale { .. }
        ));
        // A fresh display-only observation is also committed to the exact
        // submitted alias, even when source retention has no remaining budget.
        owner.byte_budget = BROWSE_CACHE_INDEX_RETAINED_BYTES;
        let display = owner
            .project_tree(&alias, None)
            .expect("bounded fresh alias display");
        assert!(matches!(
            display.observation,
            Some(backend_library::browse::ProjectTreeObservationV1::DisplayOnly { .. })
        ));
        let display_binding = display.request_binding().expect("display request identity");
        assert!(display_binding.matches_requested_root(&alias));
        assert!(display_binding.matches_effective_workspace_root(b.to_str().expect("physical B")));
        assert_eq!(
            binding.requested_root_digest,
            display_binding.requested_root_digest
        );
        assert_ne!(
            binding.effective_workspace_root_digest,
            display_binding.effective_workspace_root_digest
        );
        let wire = roundtrip(&display);
        assert!(backend_library::admit_reply(&request(&alias), &wire).is_ok());
        assert!(backend_library::admit_reply(&request(&b), &wire).is_err());
        assert!(owner.entries.is_empty() && owner.bindings.is_empty());
        assert!(owner.cached_bytes <= owner.byte_budget);
    }

    #[test]
    fn project_tree_cache_budget_keeps_display_and_revokes_evicted_source_and_readme() {
        let scratch = scratch("backend-browse-display-budget");
        let project = scratch.0.join("project");
        let helper = scratch.0.join("helper");
        for root in [&project, &helper] {
            std::fs::create_dir_all(root.join("src")).expect("physical source root");
            std::fs::write(root.join("src/lib.rs"), "pub fn physical_source() {}\n")
                .expect("physical source");
        }
        std::fs::write(project.join("Cargo.toml"),
            "[package]\nname = \"budget-project\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n[dependencies]\nbudget-helper = { path = \"../helper\" }\n").expect("project manifest");
        std::fs::write(helper.join("Cargo.toml"),
            "[package]\nname = \"budget-helper\"\nversion = \"0.1.0\"\nedition = \"2021\"\nreadme = \"README.md\"\n").expect("helper manifest");
        std::fs::write(helper.join("README.md"), "# Physical helper README\n")
            .expect("physical README");
        std::fs::write(project.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"budget-project\"\nversion = \"0.1.0\"\ndependencies = [\n \"budget-helper\",\n]\n\n[[package]]\nname = \"budget-helper\"\nversion = \"0.1.0\"\n").expect("locked local dependency graph");
        let project = project.canonicalize().expect("canonical exact project");
        let mut owner = BrowseCache::default();
        let retained = owner
            .project_tree(&project, None)
            .expect("real Cargo observation");
        let binding = retained
            .retained_request_binding()
            .expect("retained exact source context");
        let package = retained
            .direct
            .iter()
            .find(|row| row.name == "budget-helper")
            .and_then(|row| row.package_references.first())
            .and_then(Option::as_ref)
            .cloned()
            .expect("Cargo's exact helper authority");
        let request = backend_library::CargoPackageSourceRequestV1::from_tree(package.clone(), binding);
        let path = CargoPackageSourcePathV1::new("src/lib.rs").expect("source path");
        assert!(matches!(
            owner.source_file(request.clone(), path.clone()),
            CargoPackageSourceFileResultV1::Read { .. }
        ));
        assert!(matches!(
            owner.package_readme(CargoPackageReadmeRequestV1::from_tree(
                package.clone(),
                binding
            )),
            CargoPackageReadmeResultV1::Read { .. }
        ));
        // The real production byte admission now has no room for its fresh
        // observation. No synthetic cache entry or display packet is planted.
        owner.byte_budget = BROWSE_CACHE_INDEX_RETAINED_BYTES;
        let display = owner
            .project_tree(&project, None)
            .expect("bounded display survives source eviction");
        assert!(matches!(
            display.observation,
            Some(backend_library::browse::ProjectTreeObservationV1::DisplayOnly { .. })
        ));
        assert_eq!(display.request_binding(), Some(binding));
        assert!(display.has_admissible_shape());
        assert!(owner.entries.is_empty() && owner.bindings.is_empty());
        assert!(owner.cached_bytes <= owner.byte_budget);
        assert!(
            !matches!(
                owner.source_file(request, path),
                CargoPackageSourceFileResultV1::Read { .. }
            ),
            "old exact source requests cannot survive eviction into display-only state"
        );
        assert!(
            !matches!(
                owner.package_readme(CargoPackageReadmeRequestV1::from_tree(package, binding)),
                CargoPackageReadmeResultV1::Read { .. }
            ),
            "old exact README requests cannot survive eviction into display-only state"
        );
        let dto = backend_library::ReplyDto::new(
            32,
            backend_library::CommandReply::Surface(backend_library::SurfaceReply::ProjectTree(
                Box::new(display),
            )),
        );
        assert!(
            backend_library::decode_reply_body(
                &serde_json::to_vec(&dto).expect("budget fallback wire")
            )
            .is_ok()
        );
    }

    #[test]
    fn two_real_workspaces_keep_exact_context_authority_with_bounded_lru() {
        fn make_workspace(root: &Path, name: &str, helper_body: &str) {
            let member = root.join("member");
            std::fs::create_dir_all(member.join("src")).expect("workspace member source directory");
            let fixture_members = root.join("members");
            for index in 0..MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE {
                let member_name = format!("{name}-request-{index:02}");
                let member_root = fixture_members.join(format!("request-{index:02}"));
                std::fs::create_dir_all(member_root.join("src"))
                    .expect("additional workspace member source directory");
                std::fs::write(
                    member_root.join("Cargo.toml"),
                    format!(
                        "[package]\nname = \"{member_name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"
                    ),
                )
                .expect("additional workspace member manifest");
                std::fs::write(
                    member_root.join("src/lib.rs"),
                    "pub fn request_member() {}\n",
                )
                .expect("additional workspace member target");
            }
            let helper = root
                .parent()
                .expect("fixture workspace parent")
                .join(format!("{name}-helper"));
            std::fs::create_dir_all(helper.join("src")).expect("local helper source directory");
            std::fs::write(
                root.join("Cargo.toml"),
                "[workspace]\nmembers = [\"member\", \"members/*\"]\nresolver = \"2\"\n",
            )
            .expect("workspace manifest");
            std::fs::create_dir_all(root.join(".cargo")).expect("workspace Cargo config directory");
            std::fs::write(
                root.join(".cargo/config.toml"),
                "[build]\ntarget = \"x86_64-unknown-linux-gnu\"\n\n[profile.dev]\nopt-level = 1\n",
            )
            .expect("workspace target/profile config");
            let mut lock_packages = vec![name.to_owned(), "cache-shared".to_owned()];
            lock_packages.extend(
                (0..MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE)
                    .map(|index| format!("{name}-request-{index:02}")),
            );
            lock_packages.sort();
            let mut lockfile = String::from("version = 4\n\n");
            for package_name in lock_packages {
                lockfile.push_str(&format!(
                    "[[package]]\nname = \"{package_name}\"\nversion = \"0.1.0\"\n"
                ));
                if package_name == name {
                    lockfile.push_str("dependencies = [\n \"cache-shared\",\n]\n");
                }
                lockfile.push('\n');
            }
            std::fs::write(root.join("Cargo.lock"), lockfile).expect("project lockfile");
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
            std::fs::create_dir_all(member.join(".cargo")).expect("nested Cargo config dir");
            std::fs::write(
                member.join(".cargo/config.toml"),
                "[term]\ncolor = \"never\"\n\n[build]\ntarget = \"aarch64-unknown-linux-gnu\"\nrustc = \"rustc\"\n\n[profile.dev]\nopt-level = 2\n",
            )
            .expect("member-only target/profile Cargo config");
        }

        fn request_for(
            cache: &BrowseCache,
            root: &Path,
            name: &str,
            binding: backend_library::browse::ProjectTreeRequestBindingV1,
        ) -> backend_library::CargoPackageSourceRequestV1 {
            let key = request_binding_key(binding);
            let context = cache
                .bindings
                .get(&key)
                .expect("request binding has a context key");
            assert_eq!(context.workspace.as_path(), root);
            let entry = cache.entries.get(context).expect("cached request context");
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
        let binding_a = tree_a.retained_request_binding().expect("A request binding");
        let request_a = request_for(&owner, &a, "cache-shared", binding_a);
        let root_context_a = owner
            .bindings
            .get(&request_binding_key(binding_a))
            .expect("A root invocation context")
            .clone();
        let shared_a_input = Arc::clone(
            &owner
                .entries
                .get(&root_context_a)
                .expect("A root cache entry")
                .input,
        );

        let member_root_a = a.join("member");
        let tree_a_member = owner
            .project_tree(&member_root_a, None)
            .expect("member request gets its own real Cargo observation");
        let mut binding_a_member = tree_a_member
            .retained_request_binding()
            .expect("A member request binding");
        let mut request_a_member = request_for(&owner, &a, "cache-shared", binding_a_member);
        let member_context_a = owner
            .bindings
            .get(&request_binding_key(binding_a_member))
            .expect("A member invocation context")
            .clone();
        assert_ne!(
            binding_a.requested_root_digest, binding_a_member.requested_root_digest,
            "the workspace and member requests remain distinct"
        );
        assert_eq!(
            binding_a.effective_workspace_root_digest,
            binding_a_member.effective_workspace_root_digest,
            "both requests resolve to the same exact Cargo workspace"
        );
        assert_ne!(root_context_a, member_context_a);
        assert_ne!(request_a.package, request_a_member.package);
        assert!(owner.has_current_binding(&root_context_a, binding_a));
        assert!(owner.has_current_binding(&member_context_a, binding_a_member));
        assert_eq!(owner.context_count_for_workspace(&a), 2);
        assert!(Arc::ptr_eq(
            &shared_a_input,
            &owner
                .entries
                .get(&root_context_a)
                .expect("A root context remains cached")
                .input
        ));

        // A nested member config changes only the member CWD's observation.
        // The root-context receipt remains active and can still read its row.
        let stale_member_request = request_a_member.clone();
        std::fs::write(
            member_root_a.join(".cargo/config.toml"),
            "[term]\ncolor = \"always\"\n\n[build]\ntarget = \"x86_64-unknown-linux-gnu\"\nrustc = \"rustc\"\n\n[profile.dev]\nopt-level = 3\n",
        )
        .expect("change the member-only target/profile Cargo configuration");
        let source_path = CargoPackageSourcePathV1::new("src/lib.rs").expect("source path");
        assert!(matches!(
            owner.source_file(request_a_member.clone(), source_path.clone()),
            CargoPackageSourceFileResultV1::Stale { .. }
        ));
        assert!(matches!(
            owner.source_file(request_a.clone(), source_path.clone()),
            CargoPackageSourceFileResultV1::Read { .. }
        ));
        let refreshed_member = owner
            .project_tree(&member_root_a, None)
            .expect("re-admit the changed member context");
        binding_a_member = refreshed_member
            .retained_request_binding()
            .expect("refreshed member binding");
        let previous_member_route = request_a_member.package.clone();
        request_a_member = request_for(&owner, &a, "cache-shared", binding_a_member);
        assert_ne!(previous_member_route, request_a_member.package);
        assert!(matches!(
            owner.source_file(stale_member_request, source_path.clone()),
            CargoPackageSourceFileResultV1::Stale { .. }
        ));

        // The local path dependency manifest is shared input for both Cargo
        // contexts. Each route becomes stale when its own exact context sees
        // that edit, then each context can be independently re-admitted.
        let helper_manifest = a
            .parent()
            .expect("fixture parent")
            .join("cache-a-helper/Cargo.toml");
        let mut helper_manifest_bytes =
            std::fs::read(&helper_manifest).expect("read shared local dependency manifest");
        helper_manifest_bytes.extend_from_slice(b"\n# shared input changed\n");
        std::fs::write(&helper_manifest, helper_manifest_bytes)
            .expect("change shared local dependency manifest");
        let stale_root_request = request_a.clone();
        let stale_member_request = request_a_member.clone();
        let old_root_route = request_a.package.clone();
        let old_member_route = request_a_member.package.clone();
        assert!(matches!(
            owner.source_file(request_a.clone(), source_path.clone()),
            CargoPackageSourceFileResultV1::Stale { .. }
        ));
        assert!(matches!(
            owner.source_file(request_a_member.clone(), source_path.clone()),
            CargoPackageSourceFileResultV1::Stale { .. }
        ));
        let refreshed_root = owner
            .project_tree(&a, None)
            .expect("re-admit the changed workspace context");
        let binding_a = refreshed_root.retained_request_binding().expect("refreshed A binding");
        let request_a = request_for(&owner, &a, "cache-shared", binding_a);
        let refreshed_member = owner
            .project_tree(&member_root_a, None)
            .expect("re-admit the changed member context");
        binding_a_member = refreshed_member
            .retained_request_binding()
            .expect("refreshed member binding");
        request_a_member = request_for(&owner, &a, "cache-shared", binding_a_member);
        assert_ne!(old_root_route, request_a.package);
        assert_ne!(old_member_route, request_a_member.package);
        assert!(matches!(
            owner.source_file(stale_root_request, source_path.clone()),
            CargoPackageSourceFileResultV1::Stale { .. }
        ));
        assert!(matches!(
            owner.source_file(stale_member_request, source_path.clone()),
            CargoPackageSourceFileResultV1::Stale { .. }
        ));
        let root_context_a = owner
            .bindings
            .get(&request_binding_key(binding_a))
            .expect("refreshed A root invocation context")
            .clone();
        let member_context_a = owner
            .bindings
            .get(&request_binding_key(binding_a_member))
            .expect("refreshed A member invocation context")
            .clone();
        assert!(owner.has_current_binding(&member_context_a, binding_a_member));
        assert!(!Arc::ptr_eq(
            &shared_a_input,
            &owner
                .entries
                .get(&root_context_a)
                .expect("refreshed A root cache entry")
                .input
        ));
        let current_root_input = Arc::clone(
            &owner
                .entries
                .get(&root_context_a)
                .expect("refreshed A root cache entry")
                .input,
        );

        let tree_b = owner
            .project_tree(&b, None)
            .expect("real Cargo observation for workspace B");
        let binding_b = tree_b.retained_request_binding().expect("B request binding");
        let request_b = request_for(&owner, &b, "cache-shared", binding_b);
        assert_ne!(
            request_a.package, request_b.package,
            "same-name/version local packages still have distinct exact source routes"
        );
        assert_eq!(owner.workspace_count(), 2);
        assert_eq!(owner.context_count_for_workspace(&a), 2);
        assert_eq!(owner.entries.len(), 3);
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
            &current_root_input,
            &owner
                .entries
                .get(&root_context_a)
                .expect("A root context remains cached after admitting B")
                .input
        ));

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
        assert_eq!(owner.workspace_count(), MAX_BROWSE_CACHED_WORKSPACES);
        assert_eq!(owner.entries.len(), 3);
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

        // A bounded set preserves distinct real member contexts for one
        // workspace, then evicts the least-recently-used context when the
        // per-workspace context limit is reached.
        let mut newest_binding = None;
        for index in 0..MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE {
            let requested = a.join("members").join(format!("request-{index:02}"));
            let tree = owner
                .project_tree(&requested, None)
                .expect("real Cargo member request in workspace A");
            let binding = tree.retained_request_binding().expect("additional request binding");
            assert_eq!(
                binding.effective_workspace_root_digest,
                binding_a.effective_workspace_root_digest
            );
            newest_binding = Some(binding);
        }
        assert_eq!(
            owner.context_count_for_workspace(&a),
            MAX_BROWSE_CACHED_CONTEXTS_PER_WORKSPACE,
            "only the bounded most-recent exact invocation contexts remain"
        );
        assert!(
            owner.bindings.len()
                <= MAX_BROWSE_CACHED_WORKSPACES * MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE
        );
        assert_eq!(owner.requested_bindings.len(), owner.bindings.len());
        for evicted_request in [&request_a, &request_a_member] {
            assert!(matches!(
                owner.source_file(evicted_request.clone(), source_path.clone()),
                CargoPackageSourceFileResultV1::Unavailable {
                    reason: CargoPackageSourceReadFailureV1::AuthorityUnavailable,
                    ..
                }
            ));
        }
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

        let mut helper_manifest_bytes =
            std::fs::read(&helper_manifest).expect("read shared local dependency manifest");
        helper_manifest_bytes.extend_from_slice(b"\n# second shared input change\n");
        std::fs::write(&helper_manifest, helper_manifest_bytes)
            .expect("mutate A's shared local path dependency input");
        assert!(
            matches!(
                owner.source_file(newest_request, source_path),
                CargoPackageSourceFileResultV1::Stale { .. }
            ),
            "an old source selector must be stale after its local dependency changes"
        );
        assert_eq!(
            owner.counters.tree_input_allocations,
            7 + MAX_BROWSE_REQUEST_BINDINGS_PER_WORKSPACE
        );
        assert!(owner.counters.cache_hits >= 6);
        assert!(owner.counters.retained_bytes_reused > 0);
        assert_eq!(owner.counters.evictions, 3);
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
        let _scratch = Scratch(package.clone());
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
    fn each_warm_cache_candidate_runs_a_fresh_observation_before_reuse() {
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

        let mut cache = BrowseCache::default();
        let mut read_count = 0;
        fn fresh_read(
            requested: &RequestedCargoManifest,
            read_count: &mut usize,
        ) -> Result<CargoProjectRead, String> {
            *read_count += 1;
            let workspace = requested.root.clone();
            let target = workspace.join("src/bin/sidecar.rs");
            let mut watched = vec![
                workspace.join("Cargo.lock"),
                workspace.join("Cargo.toml"),
                workspace.join("src/lib.rs"),
            ];
            let has_auto_bin = target.is_file();
            if has_auto_bin {
                watched.push(target);
            }
            let file_witness =
                observation_witness_for_context(&workspace, &requested.root, &watched, None)?
                    .digest;
            let mut input = fixture_member_input(&workspace, &workspace, "gapfix 0.1.0 (fresh)");
            input.source = TreeSource::Cargo {
                host: format!("fresh-read-{read_count}"),
            };
            input.packages[0].has_bin = has_auto_bin;
            Ok(CargoProjectRead::TestMetadata(project_input_read_for_test(
                input,
                watched,
                file_witness,
                None,
            )))
        }

        let first = cache
            .input_with_read_project(&root, |requested, _| fresh_read(requested, &mut read_count))
            .expect("initial exact observation");
        let second = cache
            .input_with_read_project(&root, |requested, _| fresh_read(requested, &mut read_count))
            .expect("fresh observation reuses unchanged graph");
        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(
            read_count, 2,
            "a warm candidate still runs a fresh observer"
        );
        assert_eq!(cache.counters.cache_hits, 1);
        assert_eq!(cache.counters.tree_input_allocations, 1);

        let cancelled = cache
            .input_with_read_project(&root, |_requested, _| {
                Err("Cargo source observation was cancelled".to_owned())
            })
            .expect_err("cancelled freshness reads return no cached graph");
        assert!(cancelled.contains("cancelled"));
        assert_eq!(cache.entries.len(), 1, "cancellation preserves the entry");
        let after_cancel = cache
            .input_with_read_project(&root, |requested, _| fresh_read(requested, &mut read_count))
            .expect("later request freshly validates the retained graph");
        assert!(Arc::ptr_eq(&first, &after_cancel));
        assert_eq!(cache.counters.cache_hits, 2);

        // A lockfile-only edit invalidates the cached Cargo resolution. Its
        // manifest stays byte-for-byte identical.
        let initial_lock = std::fs::read_to_string(&lockfile).expect("initial lockfile");
        std::fs::write(
            &lockfile,
            format!("{initial_lock}\n# lockfile changed alone\n"),
        )
        .expect("changed lockfile");
        let touched = cache
            .input_with_read_project(&root, |requested, _| fresh_read(requested, &mut read_count))
            .expect("lockfile-only read");
        assert!(
            matches!(&touched.source, TreeSource::Cargo { host } if host == "fresh-read-4"),
            "a lockfile-only change must admit the new exact observation"
        );
        assert_eq!(cache.counters.tree_input_allocations, 2);

        // Replant the sentinel against the new lockfile, then change only
        // Cargo.toml. Both inputs to Cargo's answer have independent guards.
        std::fs::write(
            &manifest,
            "[package]\nname = \"gapfix\"\nversion = \"0.1.0\"\nedition = \"2021\"\ndescription = \"manifest-only cache invalidation\"\n",
        )
        .expect("changed manifest");
        let touched = cache
            .input_with_read_project(&root, |requested, _| fresh_read(requested, &mut read_count))
            .expect("manifest-only read");
        assert_ne!(
            &touched.source, &first.source,
            "a changed manifest alone must force a real read: {touched:?}"
        );
        assert_eq!(cache.counters.tree_input_allocations, 3);

        // Cargo discovers `src/bin` targets from directory contents. Adding
        // one must invalidate a warm dependency-tree cache even though neither
        // Cargo.toml nor Cargo.lock changed.
        std::fs::create_dir_all(root.join("src/bin")).expect("automatic bin directory");
        std::fs::write(root.join("src/bin/sidecar.rs"), "fn main() {}\n")
            .expect("automatic bin target");
        let touched = cache
            .input_with_read_project(&root, |requested, _| fresh_read(requested, &mut read_count))
            .expect("fresh Cargo target observation");
        assert!(
            matches!(&touched.source, TreeSource::Cargo { host } if !host.is_empty()),
            "a new Cargo auto-target path must force a new observation: {touched:?}"
        );
        assert!(touched.packages[0].has_bin);
        assert_eq!(cache.counters.tree_input_allocations, 4);
    }

    #[test]
    fn warm_lockless_workspace_refreshes_members_custom_targets_and_local_patches() {
        let scratch = scratch("backend-browse-lockless-warm-refresh");
        let workspace = scratch.0.join("workspace");
        let app = workspace.join("crates/app");
        let patch = scratch.0.join("patches/patchy");
        let external_target = scratch.0.join("targets/outside-main.rs");
        std::fs::create_dir_all(app.join("src")).expect("app source");
        std::fs::create_dir_all(patch.join("src")).expect("patch source");
        std::fs::create_dir_all(external_target.parent().expect("target parent"))
            .expect("external target directory");
        std::fs::write(
            workspace.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n\n[patch.crates-io]\npatchy = { path = \"../patches/patchy\" }\n",
        )
        .expect("workspace manifest");
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"cache-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\npatchy = \"1\"\n",
        )
        .expect("app manifest");
        std::fs::write(app.join("src/lib.rs"), "pub fn app() {}\n").expect("app source file");
        std::fs::write(
            patch.join("Cargo.toml"),
            "[package]\nname = \"patchy\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
        )
        .expect("patch package manifest");
        std::fs::write(patch.join("src/lib.rs"), "pub fn patched() {}\n")
            .expect("patch source file");

        let workspace = workspace.canonicalize().expect("canonical workspace");
        let app = app.canonicalize().expect("canonical app");
        let patch = patch.canonicalize().expect("canonical patch package");
        let mut cache = BrowseCache::default();
        let first = cache
            .project_tree(&app, None)
            .expect("first exact Cargo metadata graph");
        assert!(
            first
                .packages
                .iter()
                .any(|package| { package.name == "patchy" && package.version == "1.0.0" })
        );
        assert!(!workspace.join("Cargo.lock").exists());

        let unchanged = cache
            .project_tree(&app, None)
            .expect("fresh metadata confirms unchanged warm graph");
        assert_eq!(cache.counters.cache_hits, 1);
        assert_eq!(cache.counters.tree_input_allocations, 1);
        assert!(!workspace.join("Cargo.lock").exists());
        assert_eq!(
            unchanged.packages.len(),
            first.packages.len(),
            "the exact metadata response remains reusable when unchanged"
        );

        // Cargo's glob expansion, rather than a local directory walker,
        // decides that this newly added package belongs to the workspace.
        let new_member = workspace.join("crates/new-member");
        std::fs::create_dir_all(new_member.join("src")).expect("new member source");
        std::fs::write(
            new_member.join("Cargo.toml"),
            "[package]\nname = \"cache-new-member\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("new member manifest");
        std::fs::write(new_member.join("src/lib.rs"), "pub fn member() {}\n")
            .expect("new member source file");
        let after_member = cache
            .project_tree(&app, None)
            .expect("fresh Cargo metadata notices the new glob member");
        assert!(
            after_member
                .members
                .iter()
                .any(|member| member.name == "cache-new-member")
        );
        assert_eq!(cache.counters.tree_input_allocations, 2);
        assert!(!workspace.join("Cargo.lock").exists());

        // Lockless re-resolution sees a changed local patch manifest and
        // returns the new selected package row and generated lock digest.
        std::fs::write(
            patch.join("Cargo.toml"),
            "[package]\nname = \"patchy\"\nversion = \"1.1.0\"\nedition = \"2021\"\n",
        )
        .expect("updated patch candidate manifest");
        let after_patch = cache
            .project_tree(&app, None)
            .expect("fresh lockless resolution notices the local patch update");
        assert!(
            after_patch
                .packages
                .iter()
                .any(|package| package.name == "patchy" && package.version == "1.1.0")
        );
        assert_eq!(cache.counters.tree_input_allocations, 3);
        assert!(!workspace.join("Cargo.lock").exists());

        // Cargo reports this explicit target's source outside the package
        // directory. The source path is admitted from target.src_path itself.
        std::fs::write(
            app.join("Cargo.toml"),
            "[package]\nname = \"cache-app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\npatchy = \"1\"\n\n[[bin]]\nname = \"outside-target\"\npath = \"../../../targets/outside-main.rs\"\n",
        )
        .expect("custom target manifest");
        std::fs::write(&external_target, "fn main() {}\n").expect("external target source");
        let with_target = cache
            .project_tree(&app, None)
            .expect("fresh Cargo metadata reports custom target");
        assert!(
            with_target
                .members
                .iter()
                .any(|member| member.name == "cache-app" && member.has_bin)
        );
        let target_binding = with_target
            .retained_request_binding()
            .expect("custom target request binding");
        let target_context = cache
            .bindings
            .get(&request_binding_key(target_binding))
            .expect("custom target request context");
        let entry = cache
            .entries
            .get(target_context)
            .expect("warm metadata cache entry");
        assert!(!entry.watched.contains(&external_target));
        assert_eq!(cache.counters.tree_input_allocations, 4);

        // Editing an existing target body leaves Cargo's package graph and
        // target row unchanged, so the graph cache can reuse its tree input.
        std::fs::write(&external_target, "fn main() { let _ = 1; }\n")
            .expect("change external target source");
        let after_target_edit = cache
            .project_tree(&app, None)
            .expect("fresh Cargo metadata confirms unchanged target row");
        assert!(
            after_target_edit
                .members
                .iter()
                .any(|member| member.name == "cache-app" && member.has_bin)
        );
        assert_eq!(cache.counters.cache_hits, 2);
        assert_eq!(cache.counters.tree_input_allocations, 4);
        assert!(!workspace.join("Cargo.lock").exists());
        assert!(patch.join("Cargo.toml").is_file());
    }

    #[test]
    fn custom_target_paths_use_the_parent_of_each_cargo_config_directory() {
        let scratch = scratch("backend-cargo-custom-target-config-base");
        let workspace = scratch.0.join("workspace");
        let member = workspace.join("crates/member");
        let workspace_config = workspace.join(".cargo/config.toml");
        let member_config = member.join(".cargo/config.toml");
        let workspace_target = workspace.join("targets/workspace.json");
        let member_target = member.join("targets/member.json");
        let workspace_decoy = workspace.join(".cargo/targets/workspace.json");
        let member_decoy = member.join(".cargo/targets/member.json");

        for path in [
            workspace_config
                .parent()
                .expect("workspace config directory"),
            member_config.parent().expect("member config directory"),
            workspace_target
                .parent()
                .expect("workspace target directory"),
            member_target.parent().expect("member target directory"),
            workspace_decoy.parent().expect("workspace decoy directory"),
            member_decoy.parent().expect("member decoy directory"),
        ] {
            std::fs::create_dir_all(path).expect("fixture directory");
        }
        std::fs::write(
            &workspace_config,
            "[build]\ntarget = \"targets/workspace.json\"\n",
        )
        .expect("workspace Cargo config");
        std::fs::write(
            &member_config,
            "[build]\ntarget = \"targets/member.json\"\n",
        )
        .expect("member Cargo config");
        for path in [
            &workspace_target,
            &member_target,
            &workspace_decoy,
            &member_decoy,
        ] {
            std::fs::write(path, b"{}\n").expect("target JSON");
        }

        let mut observed = BTreeSet::new();
        for (config, value) in [
            (&workspace_config, "targets/workspace.json"),
            (&member_config, "targets/member.json"),
        ] {
            let base = cargo_config_relative_path_base(config).expect("Cargo config path base");
            observe_custom_target_path(&base, value, &mut observed)
                .expect("Cargo target from config-relative base");
        }

        assert_eq!(
            observed,
            BTreeSet::from([workspace_target, member_target]),
            "Cargo resolves each hierarchical config target from the directory above its .cargo directory"
        );
        assert!(!observed.contains(&workspace_decoy));
        assert!(!observed.contains(&member_decoy));
    }

    #[test]
    fn cargo_config_include_supports_required_and_optional_table_forms() {
        let scratch = scratch("backend-cargo-config-include-forms");
        let project = scratch.0.join("project");
        let cargo_home = scratch.0.join("cargo-home");
        let config = project.join(".cargo/config.toml");
        let required = project.join(".cargo/required.toml");
        let optional_present = project.join(".cargo/optional-present.toml");
        std::fs::create_dir_all(config.parent().expect("config parent"))
            .expect("project config directory");
        std::fs::create_dir_all(&cargo_home).expect("isolated Cargo home");
        std::fs::write(
            &config,
            "include = [\n  \"required.toml\",\n  { path = \"optional-present.toml\", optional = false },\n  { path = \"optional-missing.toml\", optional = true },\n]\n",
        )
        .expect("Cargo config with both include forms");
        std::fs::write(&required, "[net]\noffline = true\n").expect("required include");
        std::fs::write(&optional_present, "[build]\njobs = 1\n").expect("present optional include");

        let project = project.canonicalize().expect("canonical project");
        let config = project.join(".cargo/config.toml");
        let required = project.join(".cargo/required.toml");
        let optional_present = project.join(".cargo/optional-present.toml");
        let optional_missing = project.join(".cargo/optional-missing.toml");
        let observed = cargo_config_paths_with(&project, &cargo_home, None)
            .expect("valid required and optional include forms");
        assert!(observed.contains(&config));
        assert!(observed.contains(&required));
        assert!(observed.contains(&optional_present));
        assert!(
            observed.contains(&optional_missing),
            "a missing optional include remains an observed absence"
        );
        let absent_witness =
            observation_witness_with_context(&project, &observed, [1; 32], [2; 32])
                .expect("optional absence observation");
        std::fs::write(&optional_missing, "[net]\nretry = 1\n").expect("optional include appears");
        let present_witness =
            observation_witness_with_context(&project, &observed, [1; 32], [2; 32])
                .expect("optional include appearance observation");
        assert_ne!(
            absent_witness.digest, present_witness.digest,
            "appearance of a missing optional include invalidates its saved absence witness"
        );

        std::fs::write(
            &config,
            "include = [{ path = \"required-missing.toml\" }]\n",
        )
        .expect("required missing include config");
        assert!(
            cargo_config_paths_with(&project, &cargo_home, None)
                .is_err_and(|error| error.contains("required Cargo config include is missing")),
            "a missing required include makes the configuration observation fail"
        );

        std::fs::write(
            &config,
            "include = [{ path = \"required.toml\", optional = true, ignored = false }]\n",
        )
        .expect("unknown include field config");
        assert!(
            cargo_config_paths_with(&project, &cargo_home, None)
                .is_err_and(|error| error.contains("unsupported field")),
            "unknown include table fields must not be guessed"
        );

        std::fs::write(&config, "include = [\"required.txt\"]\n")
            .expect("unsupported include suffix config");
        assert!(
            cargo_config_paths_with(&project, &cargo_home, None)
                .is_err_and(|error| error.contains("must end in .toml")),
            "Cargo includes are limited to TOML files"
        );
    }

    #[cfg(unix)]
    #[test]
    fn tool_identity_detects_same_size_same_mtime_executable_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
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
            before_metadata.modified()?,
            replacement_metadata.modified()?
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
        Ok(())
    }

    #[test]
    fn cargo_tool_config_preserves_selected_compiler_and_rejects_unmeasured_env_overrides()
    -> Result<(), String> {
        let build = "[build]\nrustc = '/opt/custom/rustc'\nrustc-wrapper = 'sccache'\nrustc-workspace-wrapper = 'sccache'\n"
            .parse::<toml::Value>()
            .expect("valid Cargo config");
        assert_eq!(
            cargo_config_build_value(&build, "rustc")?,
            Some("/opt/custom/rustc")
        );
        assert_eq!(
            cargo_config_build_value(&build, "rustc-wrapper")?,
            Some("sccache")
        );
        assert_eq!(
            cargo_config_build_value(&build, "rustc-workspace-wrapper")?,
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
        assert_eq!(cargo_config_build_value(&ordinary, "rustc")?, None);
        assert!(!cargo_config_has_env_tool_override(&ordinary));
        Ok(())
    }

    #[test]
    fn cargo_tool_environment_precedence_honors_empty_direct_wrapper_disable()
    -> Result<(), Box<dyn std::error::Error>> {
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
        let config_directory = nested_config
            .parent()
            .ok_or_else(|| std::io::Error::other("nested Cargo config has a parent directory"))?;
        std::fs::create_dir_all(config_directory).expect("nested config directory");
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
        Ok(())
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
        // `e`, `o`, `t` and `u` are not in the store's base-32 alphabet.
        assert!(!is_nix_store_object_name(
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee-unknown-hash-alphabet"
        ));
        assert!(is_nix_store_object_name(
            "ffffffffffffffffffffffffffffffff-valid-hash-alphabet"
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
    fn nix_store_root_identity_ignores_unrelated_object_installation()
    -> Result<(), Box<dyn std::error::Error>> {
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
        assert!(before.len() != after.len() || before.modified()? != after.modified()?);
        let mut after_hash = blake3::Hasher::new();
        hash_unix_store_root_controls(&mut after_hash, &scratch.0, &after);
        assert_eq!(
            before_hash.finalize().as_bytes(),
            after_hash.finalize().as_bytes(),
            "unrelated store entries must not invalidate immutable tool content reuse"
        );
        Ok(())
    }

    #[test]
    fn cargo_environment_witness_includes_platform_cache_selectors() {
        for name in ["APPDATA", "SCCACHE_CONF", "CARGO_BUILD_RUSTC_WRAPPER"] {
            assert!(is_cargo_environment_witness_name(name), "{name}");
        }
        assert!(!is_cargo_environment_witness_name("UNRELATED_SETTING"));
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
