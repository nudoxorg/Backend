//! Owns the bounded rust-analyzer authority transaction for one Cargo source root.
//! Borrows HIR definitions, types, substitutions, and source maps directly into a caller closure.
//! Never renders, copies, or serializes semantic facts before the shared IR lowerer consumes them.

use std::{
    collections::{HashMap, HashSet},
    fmt, fs,
    io::Read,
    ops::Deref,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use backend_semantic::vocabulary::{RustEdition, Stage};
use ra_ap_base_db::{
    EditionedFileId, FileSet, SourceDatabase, SourceRoot, SourceRootId, all_crates,
};
use ra_ap_hir::{
    Adt, AssocItem, Const, EnumVariant, Field, FieldSource, Function, HasSource, Impl, Macro,
    Module, ModuleDef, PathResolution, Semantics, Static, Trait, TypeAlias, TypeInfo,
};
use ra_ap_ide_db::{ChangeWithProcMacros, LibraryRoots, LocalRoots, RootDatabase};
use ra_ap_project_model::{CargoConfig, CargoFeatures, RustLibSource};
use ra_ap_syntax::{
    AstNode,
    ast::{self, HasName, HasVisibility},
};
use ra_ap_vfs::{AbsPathBuf, FileExcluded, Vfs, VfsPath};

use crate::legacy::{LoadError, RustToolchain};

/// Largest sorted package source path set admitted by one Rust authority lane.
pub const MAX_RUST_WORKSPACE_SESSION_SOURCES: usize = 100_000;

/// Maximum RA source-root path entries copied while adding virtual files.
const MAX_RUST_WORKSPACE_ROOT_MEMBERSHIP_FILES: usize = 250_000;

/// Caller-owned Cargo root selected for one semantic authority transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustProject {
    /// Absolute Cargo package root.
    pub root: PathBuf,
    /// Absolute Rust source selected by the caller.
    pub source_path: PathBuf,
    /// Exact native toolchain whose sysroot establishes semantic context.
    pub toolchain: RustToolchain,
    /// Closed Rust edition expected by the compile recipe.
    pub edition: RustEdition,
}

/// One loaded Cargo package graph borrowed by every source in a package compile.
///
/// The analyzer database and VFS stay private to this frontend owner. Callers
/// can only enter one exact source at a time through [`Self::analyze_source`];
/// the higher-ranked callback prevents borrowed analyzer data from being
/// returned from that source transaction.
pub struct RustWorkspace {
    root: PathBuf,
    edition: RustEdition,
    database: RootDatabase,
    vfs: Vfs,
    /// Package-relative selected paths mapped to their lexical RA VFS paths.
    selected_source_paths: HashMap<PathBuf, PathBuf>,
}

impl fmt::Debug for RustWorkspace {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RustWorkspace")
            .field("root", &self.root)
            .field("edition", &self.edition)
            .finish_non_exhaustive()
    }
}

/// One selected package path and exact editor buffer for this Rust operation.
///
/// The path may name a new editor buffer that does not exist on disk. This
/// list is not a complete package inventory: omitting a disk file does not
/// delete it from RA's workspace. Deletions require a separately authorized
/// editor tombstone, which this API does not yet accept.
#[derive(Clone, Copy, Debug)]
pub struct RustWorkspaceFile<'source> {
    /// Normalized path relative to the admitted Cargo package root.
    pub relative_path: &'source Path,
    /// Exact UTF-8 bytes selected by the compiler request.
    pub source: &'source str,
}

/// Stable identity for one rust-analyzer workspace operation.
///
/// The key carries the full compiler authority identity even though cross-call
/// workspace reuse is currently disabled for lack of a complete RA read observer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustWorkspaceSessionKey {
    root: PathBuf,
    toolchain: RustToolchain,
    edition: RustEdition,
    stage: Stage,
    features: RustWorkspaceFeatureKey,
    toolchain_identity: Option<[u8; 32]>,
    environment_identity: Option<[u8; 32]>,
    local_authority_identity: Option<[u8; 32]>,
    package_target_identity: [u8; 32],
    environment_policy: &'static str,
    source_paths: Box<[PathBuf]>,
    canonical_source_paths: Box<[PathBuf]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RustWorkspaceFeatureKey {
    all_features: bool,
    no_default_features: bool,
    features: Box<[String]>,
}

/// Resolves existing path components while retaining a safe canonical spelling
/// for any missing suffix created by an unsaved editor buffer.
fn resolve_package_source_path(
    root: &Path,
    requested_path: &Path,
) -> Result<(PathBuf, bool), std::io::Error> {
    let relative = requested_path
        .strip_prefix(root)
        .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
    let mut resolved = root.to_path_buf();
    let mut exists = true;
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
        };
        resolved.push(component);
        if exists {
            match fs::symlink_metadata(&resolved) {
                Ok(_) => resolved = fs::canonicalize(&resolved)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => exists = false,
                Err(error) => return Err(error),
            }
        }
    }
    Ok((resolved, exists))
}

/// Canonicalizes an absolute editor path while preserving any missing suffix.
///
/// This also resolves aliases in an existing package-root prefix (for example,
/// `/var/...` versus `/private/var/...` on macOS) before matching a virtual
/// path against the selected source frontier.
fn resolve_absolute_source_path(
    root: &Path,
    requested_path: &Path,
) -> Result<(PathBuf, bool, Option<PathBuf>), std::io::Error> {
    if !requested_path.is_absolute() {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
    }
    let mut resolved = PathBuf::new();
    let mut exists = true;
    let components = requested_path.components().collect::<Vec<_>>();
    let mut package_root_end = None;
    for (index, component) in components.iter().copied().enumerate() {
        match component {
            std::path::Component::Prefix(prefix) => resolved.push(prefix.as_os_str()),
            std::path::Component::RootDir => resolved.push(component.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                return Err(std::io::Error::from(std::io::ErrorKind::InvalidInput));
            }
            std::path::Component::Normal(component) => {
                resolved.push(component);
                if exists {
                    match fs::symlink_metadata(&resolved) {
                        Ok(_) => resolved = fs::canonicalize(&resolved)?,
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            exists = false;
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        }
        if exists && package_root_end.is_none() && resolved == root {
            package_root_end = Some(index + 1);
        }
    }
    let package_relative_path = package_root_end.map(|root_end| {
        components[root_end..]
            .iter()
            .copied()
            .fold(PathBuf::new(), |mut relative, component| {
                if let std::path::Component::Normal(component) = component {
                    relative.push(component);
                }
                relative
            })
    });
    Ok((resolved, exists, package_relative_path))
}

impl RustWorkspaceSessionKey {
    /// Creates a key from the exact authority, environment, target, and current source path set.
    ///
    /// Paths are relative to `root` and must be normalized and strictly ordered.
    pub fn new(
        root: impl AsRef<Path>,
        toolchain: &RustToolchain,
        edition: RustEdition,
        stage: Stage,
        features: RustFeatureControl<'_>,
        toolchain_identity: Option<[u8; 32]>,
        environment_identity: Option<[u8; 32]>,
        local_authority_identity: Option<[u8; 32]>,
        package_target_identity: [u8; 32],
        source_paths: &[PathBuf],
    ) -> Result<Self, RustAuthorityError> {
        let root = RustProject::validate_root(root)?;
        if source_paths.is_empty() || source_paths.len() > MAX_RUST_WORKSPACE_SESSION_SOURCES {
            return Err(RustAuthorityError::SessionSourceCardinality {
                actual: source_paths.len(),
                maximum: MAX_RUST_WORKSPACE_SESSION_SOURCES,
            });
        }
        let mut previous: Option<&Path> = None;
        let mut canonical_source_paths = Vec::with_capacity(source_paths.len());
        for path in source_paths {
            if !is_normalized_relative_path(path)
                || previous.is_some_and(|previous| previous >= path.as_path())
            {
                return Err(RustAuthorityError::SessionSourcePath { path: path.clone() });
            }
            let requested_path = root.join(path);
            let (canonical, exists) =
                resolve_package_source_path(&root, &requested_path).map_err(|source| {
                    RustAuthorityError::ProjectSource {
                        path: requested_path.clone(),
                        source,
                    }
                })?;
            if exists && !canonical.is_file() {
                return Err(RustAuthorityError::SourceNotFile { path: canonical });
            }
            if !canonical.starts_with(&root) {
                return Err(RustAuthorityError::SourceOutsidePackage {
                    root: root.clone(),
                    path: canonical,
                });
            }
            canonical_source_paths.push(canonical);
            previous = Some(path);
        }
        let mut selected_features = features
            .features
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect::<Vec<_>>();
        selected_features.sort_unstable();
        selected_features.dedup();
        Ok(Self {
            root,
            toolchain: toolchain.clone(),
            edition,
            stage,
            features: RustWorkspaceFeatureKey {
                all_features: features.all_features,
                no_default_features: features.no_default_features,
                features: selected_features.into_boxed_slice(),
            },
            toolchain_identity,
            environment_identity,
            local_authority_identity,
            package_target_identity,
            environment_policy: crate::legacy::RUST_PACKAGE_CHILD_ENVIRONMENT_POLICY_ID_V1,
            source_paths: source_paths.to_vec().into_boxed_slice(),
            canonical_source_paths: canonical_source_paths.into_boxed_slice(),
        })
    }

    /// Returns the exact sorted package source path set.
    #[must_use]
    pub fn source_paths(&self) -> &[PathBuf] {
        &self.source_paths
    }
}

fn is_normalized_relative_path(path: &Path) -> bool {
    let Some(spelling) = path.to_str() else {
        return false;
    };
    !spelling.is_empty()
        && !spelling.contains('\\')
        && !path.is_absolute()
        && spelling
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
}

fn elapsed_nanos(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Work counts for Rust workspace operations in one serialized compiler lane.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RustWorkspaceSessionStats {
    /// Fresh Cargo/rust-analyzer workspace loads.
    pub workspace_loads: u64,
    /// Cross-call Rust workspace reuse admissions; this stays zero until a complete read observer exists.
    pub workspace_reuses: u64,
    /// Operations forced to load fresh because complete filesystem change observation is unavailable.
    pub workspace_reuse_disabled_requests: u64,
    /// Source texts changed in the rust-analyzer database.
    pub source_updates: u64,
    /// New source paths added to this operation's RA VFS and source root.
    pub overlay_sources_added: u64,
    /// Explicit editor tombstone paths removed from RA; omission currently never removes paths.
    pub overlay_sources_removed: u64,
    /// RA source-root path entries copied when membership changes.
    pub overlay_root_entries_rebuilt: u64,
    /// Sum of distinct local RA source roots touched by selected paths per operation.
    pub selected_source_roots_touched: u64,
    /// Admitted source texts already current in the rust-analyzer database.
    pub unchanged_sources: u64,
    /// Package operations that failed and discarded their in-progress workspace.
    pub failed_transactions: u64,
    /// Fresh RA workspace load wall time in nanoseconds.
    pub workspace_load_nanos: u64,
    /// Exact source-frontier update wall time in nanoseconds.
    pub source_update_nanos: u64,
}

struct RustWorkspaceSession {
    workspace: RustWorkspace,
}

/// Single-owner Rust workspace operation lane for a serialized compiler lane.
///
/// Cross-call workspace reuse is disabled until rust-analyzer exposes a
/// complete positive and negative read observer. Every operation opens a
/// fresh workspace, and the package lease keeps it alive only through staging.
/// The canonical IR publication owner is independent, so a failed package
/// operation cannot publish partial compiler state.
#[derive(Default)]
pub struct RustWorkspaceSessionLane {
    stats: RustWorkspaceSessionStats,
}

/// Mutably borrowed in-progress Rust analyzer workspace operation.
pub struct RustWorkspaceSessionLease<'cache> {
    lane: &'cache mut RustWorkspaceSessionLane,
    session: Option<RustWorkspaceSession>,
    committed: bool,
}

impl RustWorkspaceSessionLane {
    /// Opens a fresh analyzer workspace and admits the exact package buffers.
    ///
    /// The returned lease must be committed after the complete package
    /// operation succeeds. Dropping it discards the analyzer workspace,
    /// including during unwinding. Cross-call reuse stays disabled because
    /// the available rust-analyzer API does not report every positive and
    /// negative filesystem read.
    pub fn begin<'cache>(
        &'cache mut self,
        key: RustWorkspaceSessionKey,
        files: &[RustWorkspaceFile<'_>],
        control: RustAnalysisControl<'_>,
    ) -> Result<RustWorkspaceSessionLease<'cache>, RustAuthorityError> {
        control.check()?;
        if files.len() != key.source_paths.len()
            || files
                .iter()
                .zip(key.source_paths.iter())
                .any(|(file, expected)| file.relative_path != expected)
        {
            return Err(RustAuthorityError::SessionFrontierMismatch);
        }
        self.stats.workspace_reuse_disabled_requests = self
            .stats
            .workspace_reuse_disabled_requests
            .saturating_add(1);
        let load_started = Instant::now();
        let selected_features = key
            .features
            .features
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let features = RustFeatureControl {
            all_features: key.features.all_features,
            no_default_features: key.features.no_default_features,
            features: &selected_features,
        };
        let workspace = RustWorkspace::open_with_features(
            &key.root,
            &key.toolchain,
            key.edition,
            features,
            control,
        );
        self.stats.workspace_load_nanos = self
            .stats
            .workspace_load_nanos
            .saturating_add(elapsed_nanos(load_started));
        let mut workspace = workspace.map_err(|error| {
            self.stats.failed_transactions = self.stats.failed_transactions.saturating_add(1);
            error
        })?;
        self.stats.workspace_loads = self.stats.workspace_loads.saturating_add(1);
        let update_started = Instant::now();
        let result = workspace.apply_selected_sources(files, &key, control);
        self.stats.source_update_nanos = self
            .stats
            .source_update_nanos
            .saturating_add(elapsed_nanos(update_started));
        let (updated, unchanged, added, root_entries_rebuilt, roots_touched) =
            result.map_err(|error| {
                self.stats.failed_transactions = self.stats.failed_transactions.saturating_add(1);
                error
            })?;
        self.stats.source_updates = self.stats.source_updates.saturating_add(updated as u64);
        self.stats.overlay_sources_added = self
            .stats
            .overlay_sources_added
            .saturating_add(added as u64);
        self.stats.overlay_root_entries_rebuilt = self
            .stats
            .overlay_root_entries_rebuilt
            .saturating_add(root_entries_rebuilt as u64);
        self.stats.selected_source_roots_touched = self
            .stats
            .selected_source_roots_touched
            .saturating_add(roots_touched as u64);
        self.stats.unchanged_sources = self
            .stats
            .unchanged_sources
            .saturating_add(unchanged as u64);
        control.check().map_err(|error| {
            self.stats.failed_transactions = self.stats.failed_transactions.saturating_add(1);
            error
        })?;
        Ok(RustWorkspaceSessionLease {
            lane: self,
            session: Some(RustWorkspaceSession { workspace }),
            committed: false,
        })
    }

    /// Returns cumulative workspace work counts for this compiler lane.
    #[must_use]
    pub const fn stats(&self) -> RustWorkspaceSessionStats {
        self.stats
    }
}

impl RustWorkspaceSessionLease<'_> {
    /// Borrows the analyzer workspace for the current package operation.
    #[must_use]
    pub fn workspace(&self) -> &RustWorkspace {
        &self
            .session
            .as_ref()
            .expect("session lease is active")
            .workspace
    }

    /// Ends the successful package transaction and drops its analyzer workspace.
    pub fn commit(mut self) {
        drop(self.session.take().expect("session lease is active"));
        self.committed = true;
    }
}

impl Drop for RustWorkspaceSessionLease<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.session.take();
            self.lane.stats.failed_transactions =
                self.lane.stats.failed_transactions.saturating_add(1);
        }
    }
}

/// Cargo relationship established for one selected Rust source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RustSourceScope {
    /// The selected file is the root of an active Cargo target, including build scripts.
    CargoTargetRoot,
    /// The selected file is an active module of a Cargo target.
    CargoModule,
}

impl RustProject {
    pub(crate) fn validate_root(root: impl AsRef<Path>) -> Result<PathBuf, RustAuthorityError> {
        let root =
            root.as_ref()
                .canonicalize()
                .map_err(|source| RustAuthorityError::ProjectRoot {
                    path: root.as_ref().to_path_buf(),
                    source,
                })?;
        let manifest = root.join("Cargo.toml");
        if !manifest.is_file() {
            return Err(RustAuthorityError::MissingManifest { path: manifest });
        }
        Ok(root)
    }
    /// Validates one caller-selected Cargo root and seals its language profile.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError`] when `root` cannot identify a Cargo package.
    pub fn open(
        root: impl AsRef<Path>,
        toolchain: &RustToolchain,
        edition: RustEdition,
    ) -> Result<Self, RustAuthorityError> {
        let source_path = root.as_ref().join("src/lib.rs");
        Self::open_with_source(root, source_path, toolchain, edition)
    }

    /// Validates a caller-selected Cargo package and exact Rust source.
    ///
    /// The driver uses this form so the source authority cannot be guessed
    /// from a package layout or filename.
    ///
    /// # Errors
    ///
    /// Returns a typed authority failure when either caller-selected path is
    /// unavailable or the Cargo package lacks a package manifest.
    pub fn open_with_source(
        root: impl AsRef<Path>,
        source_path: impl AsRef<Path>,
        toolchain: &RustToolchain,
        edition: RustEdition,
    ) -> Result<Self, RustAuthorityError> {
        let root =
            root.as_ref()
                .canonicalize()
                .map_err(|source| RustAuthorityError::ProjectRoot {
                    path: root.as_ref().to_path_buf(),
                    source,
                })?;
        let manifest = root.join("Cargo.toml");
        if !manifest.is_file() {
            return Err(RustAuthorityError::MissingManifest { path: manifest });
        }
        let source_path = source_path.as_ref().canonicalize().map_err(|source| {
            RustAuthorityError::ProjectSource {
                path: source_path.as_ref().to_path_buf(),
                source,
            }
        })?;
        if !source_path.is_file() {
            return Err(RustAuthorityError::SourceNotFile { path: source_path });
        }
        Ok(Self {
            root,
            source_path,
            toolchain: toolchain.clone(),
            edition,
        })
    }

    /// Runs one non-escaping semantic transaction over the selected Cargo root.
    ///
    /// The closure is universally quantified over the analyzer lifetime, so no HIR value can
    /// escape the loaded database. A lowerer must emit directly into its caller-owned compact IR
    /// arena while this closure runs.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError`] when Cargo loading, profile validation, source loading, or
    /// source-coordinate validation fails; preserves a closure-returned authority failure exactly.
    pub fn analyze<Output>(
        &self,
        control: RustAnalysisControl<'_>,
        lower: impl for<'analysis> FnOnce(
            RustAuthority<'analysis>,
        ) -> Result<Output, RustAuthorityError>,
    ) -> Result<Output, RustAuthorityError> {
        self.analyze_with_features(control, RustFeatureControl::default(), lower)
    }

    /// Runs analysis with explicit Cargo feature unification controls.
    pub fn analyze_with_features<Output>(
        &self,
        control: RustAnalysisControl<'_>,
        features: RustFeatureControl<'_>,
        lower: impl for<'analysis> FnOnce(
            RustAuthority<'analysis>,
        ) -> Result<Output, RustAuthorityError>,
    ) -> Result<Output, RustAuthorityError> {
        control.check()?;
        let maximum = u64::from(*control.maximum_source_bytes);
        let metadata =
            fs::metadata(&self.source_path).map_err(|source| RustAuthorityError::SourceRead {
                path: self.source_path.clone(),
                source,
            })?;
        if metadata.len() > maximum {
            return Err(RustAuthorityError::SourceBudget {
                actual: metadata.len(),
                maximum: control.maximum_source_bytes,
            });
        }
        let mut source = Vec::new();
        fs::File::open(&self.source_path)
            .map_err(|source| RustAuthorityError::SourceRead {
                path: self.source_path.clone(),
                source,
            })?
            .take(maximum.saturating_add(1))
            .read_to_end(&mut source)
            .map_err(|source| RustAuthorityError::SourceRead {
                path: self.source_path.clone(),
                source,
            })?;
        if source.len() as u64 > maximum {
            return Err(RustAuthorityError::SourceBudget {
                actual: source.len() as u64,
                maximum: control.maximum_source_bytes,
            });
        }
        let workspace = RustWorkspace::open_with_features(
            &self.root,
            &self.toolchain,
            self.edition,
            features,
            control,
        )?;
        workspace.analyze_source(&self.source_path, &source, control, lower)
    }
}

impl RustWorkspace {
    /// Confirms that a borrowed package owner addresses this workspace's exact root and edition.
    pub fn validate_binding(
        &self,
        root: impl AsRef<Path>,
        edition: RustEdition,
    ) -> Result<(), RustAuthorityError> {
        let root = RustProject::validate_root(root)?;
        if self.root != root || self.edition != edition {
            return Err(RustAuthorityError::WorkspaceBindingMismatch);
        }
        Ok(())
    }

    /// Loads one Cargo package graph under its exact toolchain, edition, and feature policy.
    ///
    /// No source-specific HIR is returned here. Each admitted package source
    /// enters later through [`Self::analyze_source`] while borrowing this same
    /// database and VFS.
    pub fn open(
        root: impl AsRef<Path>,
        toolchain: &RustToolchain,
        edition: RustEdition,
        control: RustAnalysisControl<'_>,
    ) -> Result<Self, RustAuthorityError> {
        Self::open_with_features(
            root,
            toolchain,
            edition,
            RustFeatureControl::default(),
            control,
        )
    }

    /// Loads one Cargo package graph with explicit Cargo feature unification controls.
    pub fn open_with_features(
        root: impl AsRef<Path>,
        toolchain: &RustToolchain,
        edition: RustEdition,
        features: RustFeatureControl<'_>,
        control: RustAnalysisControl<'_>,
    ) -> Result<Self, RustAuthorityError> {
        control.check()?;
        let root = RustProject::validate_root(root)?;
        let cargo = toolchain
            .cargo
            .as_ref()
            .ok_or(LoadError::MissingCargoConfiguration)?;
        let cargo_home = toolchain
            .cargo_home
            .as_ref()
            .ok_or(LoadError::MissingCargoConfiguration)?;
        let path = toolchain.authority_path()?;
        let extra_env = [
            (
                "CARGO".to_owned(),
                Some(cargo.to_string_lossy().into_owned()),
            ),
            (
                "CARGO_HOME".to_owned(),
                Some(cargo_home.to_string_lossy().into_owned()),
            ),
            // A selected Cargo wrapper can still need HOME even when
            // CARGO_HOME is explicit. Use the admitted cache root itself so
            // isolated children never inherit the caller's ambient HOME.
            (
                "HOME".to_owned(),
                Some(cargo_home.to_string_lossy().into_owned()),
            ),
            (
                "RUSTC".to_owned(),
                Some(toolchain.tool.to_string_lossy().into_owned()),
            ),
            (
                "RUSTUP_HOME".to_owned(),
                toolchain
                    .rustup_home
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
            ),
            (
                "RUSTUP_TOOLCHAIN".to_owned(),
                toolchain.rustup_toolchain.clone(),
            ),
            ("PATH".to_owned(), Some(path)),
        ]
        .into_iter()
        .collect();
        let config = CargoConfig {
            sysroot: Some(RustLibSource::Path(AbsPathBuf::assert_utf8(
                toolchain.sysroot.clone(),
            ))),
            no_deps: false,
            metadata_extra_args: vec!["--offline".to_owned()],
            features: features.cargo_features(),
            extra_env,
            isolate_env: true,
            ..CargoConfig::default()
        };
        let load = ra_ap_load_cargo::LoadCargoConfig {
            load_out_dirs_from_check: false,
            with_proc_macro_server: ra_ap_load_cargo::ProcMacroServerChoice::None,
            prefill_caches: false,
            num_worker_threads: 1,
            proc_macro_processes: 0,
        };
        let (database, vfs, _proc_macros) =
            ra_ap_load_cargo::load_workspace_at(&root, &config, &load, &|_| {}).map_err(
                |source| RustAuthorityError::Workspace {
                    root: root.clone(),
                    source,
                },
            )?;
        control.check()?;
        Ok(Self {
            root,
            edition,
            database,
            vfs,
            selected_source_paths: HashMap::new(),
        })
    }

    /// Runs one exact source transaction against this workspace's shared analyzer database.
    ///
    /// `source` is the admitted package-frontier buffer. Before HIR is exposed,
    /// its bytes must exactly match the text rust-analyzer loaded into this
    /// workspace's VFS. The callback cannot return a value borrowing the HIR
    /// transaction.
    pub fn analyze_source<Output>(
        &self,
        source_path: impl AsRef<Path>,
        source: &[u8],
        control: RustAnalysisControl<'_>,
        lower: impl for<'analysis> FnOnce(
            RustAuthority<'analysis>,
        ) -> Result<Output, RustAuthorityError>,
    ) -> Result<Output, RustAuthorityError> {
        control.check()?;
        if source.len() > *control.maximum_source_bytes as usize {
            return Err(RustAuthorityError::SourceBudget {
                actual: u64::try_from(source.len()).unwrap_or(u64::MAX),
                maximum: control.maximum_source_bytes,
            });
        }
        let requested_path = source_path.as_ref();
        let relative_path = requested_path.strip_prefix(&self.root).ok();
        let (source_path, exists) = if let Some(relative_path) = relative_path {
            if !is_normalized_relative_path(relative_path) {
                return Err(RustAuthorityError::SessionSourcePath {
                    path: relative_path.to_path_buf(),
                });
            }
            if let Some(source_path) = self.selected_source_paths.get(relative_path) {
                (source_path.clone(), false)
            } else {
                let (canonical, exists) = resolve_package_source_path(&self.root, requested_path)
                    .map_err(|source| RustAuthorityError::ProjectSource {
                    path: requested_path.to_path_buf(),
                    source,
                })?;
                if !canonical.starts_with(&self.root) {
                    return Err(RustAuthorityError::SourceOutsidePackage {
                        root: self.root.clone(),
                        path: canonical,
                    });
                }
                (requested_path.to_path_buf(), exists)
            }
        } else {
            let (canonical, exists, package_relative_path) = if requested_path.is_absolute() {
                resolve_absolute_source_path(&self.root, requested_path)
            } else {
                requested_path
                    .canonicalize()
                    .map(|canonical| (canonical, true, None))
            }
            .map_err(|source| RustAuthorityError::ProjectSource {
                path: requested_path.to_path_buf(),
                source,
            })?;
            if !canonical.starts_with(&self.root) {
                return Err(RustAuthorityError::SourceOutsidePackage {
                    root: self.root.clone(),
                    path: canonical,
                });
            }
            if exists && !canonical.is_file() {
                return Err(RustAuthorityError::SourceNotFile { path: canonical });
            }
            let selected = if let Some(relative_path) = package_relative_path {
                if !relative_path.as_os_str().is_empty() {
                    self.selected_source_paths
                        .get(&relative_path)
                        .cloned()
                        .unwrap_or_else(|| self.root.join(relative_path))
                } else {
                    self.root.clone()
                }
            } else {
                canonical
            };
            (selected, exists)
        };
        if exists && !source_path.is_file() {
            return Err(RustAuthorityError::SourceNotFile { path: source_path });
        }
        if !source_path.starts_with(&self.root) {
            return Err(RustAuthorityError::SourceOutsidePackage {
                root: self.root.clone(),
                path: source_path,
            });
        }
        let vfs_path = VfsPath::from(AbsPathBuf::assert_utf8(source_path.clone()));
        let file_id = self
            .vfs
            .file_id(&vfs_path)
            .map(|(id, _excluded)| id)
            .ok_or(RustAuthorityError::SourceNotLoaded {
                path: source_path.clone(),
            })?;
        let observed_source = SourceDatabase::file_text(&self.database, file_id);
        let observed_text = observed_source.text(&self.database);
        if observed_text.as_bytes() != source {
            return Err(RustAuthorityError::SourceBinding {
                expected: source.len(),
                observed: observed_text.len(),
            });
        }
        // Cargo's VFS contains package Rust files even when cfg removes them
        // from every active module tree. Resolve an ordinary source through
        // its HIR module owner; use root-file identity for standalone Cargo
        // targets such as build scripts. Never treat a merely present VFS
        // file as a Cargo crate root.
        let package_root = AbsPathBuf::assert_utf8(self.root.clone());
        let semantics = Semantics::new(&self.database);
        let crate_belongs_to_package = |krate: ra_ap_hir::Crate| {
            let root_file = krate.root_file(&self.database);
            self.vfs
                .file_path(root_file)
                .as_path()
                .is_some_and(|path| path.starts_with(package_root.as_path()))
        };
        let owner = semantics
            .file_to_module_defs(file_id)
            .map(|module| module.krate(&self.database))
            .find(|krate| crate_belongs_to_package(*krate))
            // `all_crates` is topologically ordered, so shared roots resolve
            // to the first crate in the loader's deterministic graph order.
            .or_else(|| {
                all_crates(&self.database)
                    .iter()
                    .copied()
                    .map(ra_ap_hir::Crate::from)
                    .find(|krate| {
                        krate.root_file(&self.database) == file_id
                            && crate_belongs_to_package(*krate)
                    })
            })
            .ok_or_else(|| RustAuthorityError::DetachedSource {
                path: source_path.clone(),
            })?;
        let source_scope = if owner.root_file(&self.database) == file_id {
            RustSourceScope::CargoTargetRoot
        } else {
            RustSourceScope::CargoModule
        };
        let observed_edition = owner.edition(&self.database);
        let source_file = EditionedFileId::new(&self.database, file_id, observed_edition);
        let observed = rust_edition(observed_edition);
        if observed != self.edition {
            return Err(RustAuthorityError::EditionMismatch {
                requested: self.edition,
                observed,
            });
        }
        control.check()?;
        ra_ap_hir_ty::next_solver::interner::attach_db(&self.database, || {
            let semantics = Semantics::new(&self.database);
            let root = semantics.parse(source_file);
            lower(RustAuthority {
                database: &self.database,
                semantics,
                root,
                source,
                source_file,
                edition: self.edition,
                source_scope,
            })
        })
    }

    fn apply_selected_sources(
        &mut self,
        files: &[RustWorkspaceFile<'_>],
        key: &RustWorkspaceSessionKey,
        control: RustAnalysisControl<'_>,
    ) -> Result<(usize, usize, usize, usize, usize), RustAuthorityError> {
        if files.len() != key.source_paths.len() {
            return Err(RustAuthorityError::SessionFrontierMismatch);
        }
        let mut desired_vfs_paths = HashSet::with_capacity(files.len());
        for (index, (file, expected_path)) in files.iter().zip(key.source_paths.iter()).enumerate()
        {
            control.check()?;
            if file.relative_path != expected_path
                || !is_normalized_relative_path(file.relative_path)
            {
                return Err(RustAuthorityError::SessionSourcePath {
                    path: file.relative_path.to_path_buf(),
                });
            }
            if file.source.len() > *control.maximum_source_bytes as usize {
                return Err(RustAuthorityError::SourceBudget {
                    actual: u64::try_from(file.source.len()).unwrap_or(u64::MAX),
                    maximum: control.maximum_source_bytes,
                });
            }
            let requested_path = self.root.join(file.relative_path);
            let (source_path, exists) = resolve_package_source_path(&self.root, &requested_path)
                .map_err(|source| RustAuthorityError::ProjectSource {
                    path: requested_path.clone(),
                    source,
                })?;
            if exists && !source_path.is_file() {
                return Err(RustAuthorityError::SourceNotFile { path: source_path });
            }
            if source_path != key.canonical_source_paths[index] {
                return Err(RustAuthorityError::SessionSourcePath {
                    path: file.relative_path.to_path_buf(),
                });
            }
            if !source_path.starts_with(&self.root) {
                return Err(RustAuthorityError::SourceOutsidePackage {
                    root: self.root.clone(),
                    path: source_path,
                });
            }
            if !desired_vfs_paths.insert(requested_path.clone()) {
                return Err(RustAuthorityError::SessionSourcePath {
                    path: file.relative_path.to_path_buf(),
                });
            }
        }

        let local_roots = LocalRoots::get(&self.database)
            .roots(&self.database)
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let mut change = ChangeWithProcMacros::default();
        let mut updated = 0_usize;
        let mut unchanged = 0_usize;
        let mut added = 0_usize;
        let mut added_ids = Vec::new();
        let mut selected_source_roots = HashSet::new();
        let mut local_roots_by_directory = None;
        for (index, file) in files.iter().enumerate() {
            control.check()?;
            let source_path = &key.canonical_source_paths[index];
            let requested_path = self.root.join(file.relative_path);
            let Some(path_text) = requested_path.to_str() else {
                return Err(RustAuthorityError::SessionSourcePath {
                    path: file.relative_path.to_path_buf(),
                });
            };
            let vfs_path = VfsPath::from(AbsPathBuf::assert_utf8(PathBuf::from(path_text)));
            let (file_id, newly_added) =
                if let Some((file_id, excluded)) = self.vfs.file_id(&vfs_path) {
                    if excluded == FileExcluded::Yes {
                        return Err(RustAuthorityError::SourceNotLoaded {
                            path: requested_path,
                        });
                    }
                    let source_root = self
                        .database
                        .file_source_root(file_id)
                        .source_root_id(&self.database);
                    if !local_roots.contains(&source_root) {
                        return Err(RustAuthorityError::SessionSourceRootAmbiguous);
                    }
                    selected_source_roots.insert(source_root);
                    (file_id, false)
                } else {
                    if local_roots_by_directory.is_none() {
                        local_roots_by_directory =
                            Some(self.local_source_roots_by_directory(&local_roots, control)?);
                    }
                    let source_roots_by_directory = local_roots_by_directory
                        .as_mut()
                        .expect("local source-root index was initialized");
                    let source_root = Self::source_root_for_new_file(
                        &self.root,
                        &requested_path,
                        source_roots_by_directory,
                    )?;
                    selected_source_roots.insert(source_root);
                    self.vfs
                        .set_file_contents(vfs_path.clone(), Some(file.source.as_bytes().to_vec()));
                    let Some((file_id, FileExcluded::No)) = self.vfs.file_id(&vfs_path) else {
                        return Err(RustAuthorityError::SourceNotLoaded {
                            path: source_path.clone(),
                        });
                    };
                    added_ids.push((source_root, file_id, vfs_path.clone()));
                    if let Some(parent) = requested_path.parent() {
                        source_roots_by_directory
                            .entry(parent.to_path_buf())
                            .or_default()
                            .insert(source_root);
                    }
                    added = added.saturating_add(1);
                    (file_id, true)
                };
            if newly_added {
                change.change_file(file_id, Some(file.source.to_owned()));
                updated = updated.saturating_add(1);
                continue;
            }
            let observed = SourceDatabase::file_text(&self.database, file_id);
            let observed_text = observed.text(&self.database);
            if observed_text.as_ref() == file.source {
                unchanged = unchanged.saturating_add(1);
            } else {
                change.change_file(file_id, Some(file.source.to_owned()));
                updated = updated.saturating_add(1);
            }
        }
        let mut root_entries_rebuilt = 0_usize;
        if added != 0 {
            let mut roots = Vec::new();
            let local_root_ids = LocalRoots::get(&self.database).roots(&self.database);
            let library_root_ids = LibraryRoots::get(&self.database).roots(&self.database);
            let all_root_ids = local_root_ids
                .iter()
                .chain(library_root_ids.iter())
                .copied()
                .collect::<HashSet<_>>();
            let Some(max_root_id) = all_root_ids.iter().map(|id| id.0).max() else {
                return Err(RustAuthorityError::SessionSourceRootAmbiguous);
            };
            if all_root_ids.len() != max_root_id as usize + 1 {
                return Err(RustAuthorityError::SessionSourceRootAmbiguous);
            }
            roots.reserve(max_root_id as usize + 1);
            for raw_id in 0..=max_root_id {
                control.check()?;
                let root_id = SourceRootId(raw_id);
                let old_root = self
                    .database
                    .source_root(root_id)
                    .source_root(&self.database);
                let mut file_set = FileSet::default();
                for file_id in old_root.iter() {
                    control.check()?;
                    let Some(path) = old_root.path_for_file(&file_id) else {
                        return Err(RustAuthorityError::SessionSourceRootAmbiguous);
                    };
                    file_set.insert(file_id, path.clone());
                    root_entries_rebuilt = root_entries_rebuilt.saturating_add(1);
                    if root_entries_rebuilt > MAX_RUST_WORKSPACE_ROOT_MEMBERSHIP_FILES {
                        return Err(RustAuthorityError::SessionSourceRootLimit {
                            maximum: MAX_RUST_WORKSPACE_ROOT_MEMBERSHIP_FILES,
                        });
                    }
                }
                for (added_root, file_id, path) in &added_ids {
                    if *added_root == root_id {
                        file_set.insert(*file_id, path.clone());
                    }
                }
                roots.push(if old_root.is_library {
                    SourceRoot::new_library(file_set)
                } else {
                    SourceRoot::new_local(file_set)
                });
            }
            change.set_roots(roots);
        }
        control.check()?;
        if updated != 0 || added != 0 {
            self.database.apply_change(change);
        }
        control.check()?;
        self.selected_source_paths = files
            .iter()
            .map(|file| {
                (
                    file.relative_path.to_path_buf(),
                    self.root.join(file.relative_path),
                )
            })
            .collect();
        Ok((
            updated,
            unchanged,
            added,
            root_entries_rebuilt,
            selected_source_roots.len(),
        ))
    }

    fn local_source_roots_by_directory(
        &self,
        local_roots: &HashSet<SourceRootId>,
        control: RustAnalysisControl<'_>,
    ) -> Result<HashMap<PathBuf, HashSet<SourceRootId>>, RustAuthorityError> {
        let mut roots_by_directory = HashMap::<PathBuf, HashSet<SourceRootId>>::new();
        for (file_id, vfs_path) in self.vfs.iter() {
            control.check()?;
            let Some(path) = vfs_path.as_path() else {
                continue;
            };
            let path = Path::new(path.as_str());
            if !path.starts_with(&self.root)
                || !path.extension().is_some_and(|extension| extension == "rs")
            {
                continue;
            }
            let source_root = self
                .database
                .file_source_root(file_id)
                .source_root_id(&self.database);
            if local_roots.contains(&source_root) {
                if let Some(parent) = path.parent() {
                    roots_by_directory
                        .entry(parent.to_path_buf())
                        .or_default()
                        .insert(source_root);
                }
            }
        }
        Ok(roots_by_directory)
    }

    fn source_root_for_new_file(
        root: &Path,
        requested_path: &Path,
        roots_by_directory: &HashMap<PathBuf, HashSet<SourceRootId>>,
    ) -> Result<SourceRootId, RustAuthorityError> {
        let mut directory = requested_path.parent();
        while let Some(parent) = directory {
            if !parent.starts_with(root) {
                break;
            }
            match roots_by_directory
                .get(parent)
                .map_or(0, |roots| roots.len())
            {
                1 => {
                    return Ok(*roots_by_directory
                        .get(parent)
                        .and_then(|roots| roots.iter().next())
                        .expect("one source root was checked"));
                }
                0 => directory = parent.parent(),
                _ => return Err(RustAuthorityError::SessionSourceRootAmbiguous),
            }
        }
        Err(RustAuthorityError::SessionSourceRootAmbiguous)
    }
}

/// Bounded original-source byte budget admitted before Cargo workspace loading.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceByteLimit(pub u32);

impl Deref for SourceByteLimit {
    type Target = u32;

    /// Borrows the caller-selected maximum without erasing its source-budget meaning.
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<u32> for SourceByteLimit {
    /// Binds one checked-u32 transport limit to Rust source admission.
    fn from(bytes: u32) -> Self {
        Self(bytes)
    }
}

/// Lock-free cancellation and bounded-work facts for one authority transaction.
#[derive(Clone, Copy, Debug)]
pub struct RustAnalysisControl<'cancel> {
    /// Caller-owned cancellation flag, checked at every controllable phase boundary.
    pub cancelled: &'cancel AtomicBool,
    /// Maximum root-source size admitted before Cargo workspace loading.
    pub maximum_source_bytes: SourceByteLimit,
    /// Monotonic deadline checked before every controllable expensive phase.
    pub deadline: Instant,
}

/// Caller-selected Cargo feature policy, borrowing the requested spellings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RustFeatureControl<'features> {
    /// Enable every declared feature.
    pub all_features: bool,
    /// Suppress the package's default feature set.
    pub no_default_features: bool,
    /// Exact feature names requested by the caller.
    pub features: &'features [&'features str],
}

impl<'features> RustFeatureControl<'features> {
    /// The unchanged Cargo default.
    #[must_use]
    pub const fn default() -> Self {
        Self {
            all_features: false,
            no_default_features: false,
            features: &[],
        }
    }
}

impl<'features> RustFeatureControl<'features> {
    fn cargo_features(self) -> CargoFeatures {
        if self.all_features {
            CargoFeatures::All
        } else {
            CargoFeatures::Selected {
                features: self
                    .features
                    .iter()
                    .map(|feature| (*feature).to_owned())
                    .collect(),
                no_default_features: self.no_default_features,
            }
        }
    }
}

impl RustAnalysisControl<'_> {
    /// Stops a transaction before it begins another controllable expensive phase.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError::Cancelled`] when the caller has released the work permit.
    fn check(self) -> Result<(), RustAuthorityError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(RustAuthorityError::Cancelled)
        } else if Instant::now() >= self.deadline {
            Err(RustAuthorityError::DeadlineExceeded)
        } else {
            Ok(())
        }
    }
}

/// Non-escaping rust-analyzer view whose lifetimes prove all semantic values remain borrowed.
pub struct RustAuthority<'analysis> {
    /// HIR database holding project-model, source-map, resolution, and inference state.
    pub database: &'analysis ra_ap_ide_db::RootDatabase,
    /// Source-to-HIR resolver scoped to `database`.
    pub semantics: Semantics<'analysis, ra_ap_ide_db::RootDatabase>,
    /// Parsed root module bound to `source_file`.
    pub root: ast::SourceFile,
    /// Exact caller-owned original source bytes.
    pub source: &'analysis [u8],
    /// Analyzer file identity of `source` under its Cargo-derived edition.
    pub source_file: EditionedFileId,
    /// Compile-recipe edition cross-checked before this authority was created.
    pub edition: RustEdition,
    /// Active Cargo relationship proven for this selected source.
    pub source_scope: RustSourceScope,
}

impl<'analysis> RustAuthority<'analysis> {
    /// Streams HIR-backed declarations in source traversal order without materializing a fact list.
    pub fn declarations(&self) -> impl Iterator<Item = RustDeclaration> + '_ {
        self.root
            .syntax()
            .descendants()
            .filter_map(|syntax| self.declaration(syntax))
    }

    /// Returns the exact identifier span selected by rust-analyzer syntax for one declaration.
    ///
    /// # Errors
    ///
    /// Returns a missing-semantic-fact terminal when the HIR-backed declaration
    /// has no source identifier (for example, an anonymous implementation), or
    /// a coordinate failure without falling back to text scanning.
    pub fn declaration_name(
        &self,
        declaration: &RustDeclaration,
    ) -> Result<ByteSpan, RustAuthorityError> {
        let name = declaration
            .syntax
            .descendants()
            .find_map(ast::Name::cast)
            .map(|name| name.syntax().clone())
            .or_else(|| {
                declaration
                    .syntax
                    .descendants()
                    .find_map(ast::NameRef::cast)
                    .map(|name| name.syntax().clone())
            })
            .ok_or(RustAuthorityError::MissingSemanticFact {
                fact: declaration.kind,
            })?;
        self.span(&name)
    }

    /// Streams method calls with the actual inferred receiver/call type and resolved function.
    pub fn method_calls(&self) -> impl Iterator<Item = RustMethodCall<'analysis>> + '_ {
        let mut calls = Vec::new();
        // Several expanded tokens can project to one written call. Keep the
        // first-seen order while making duplicate detection independent of
        // the number of calls already collected.
        let mut projected_calls = HashSet::new();
        for syntax in self.root.syntax().descendants() {
            let Some(syntax) = ast::MethodCallExpr::cast(syntax) else {
                continue;
            };
            calls.push(RustMethodCall {
                inferred: self.semantics.type_of_expr(&syntax.clone().into()),
                target: self.semantics.resolve_method_call(&syntax),
                syntax,
                projected_span: None,
            });
        }

        // A macro argument is parsed as a token tree in the source file. Descending
        // each token gives us the expanded syntax node that rust-analyzer inferred;
        // its original-range map still points at the written argument.
        for macro_call in self.macro_calls() {
            let Some(token_tree) = macro_call.token_tree() else {
                continue;
            };
            for token in token_tree
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
            {
                for descended in self.semantics.descend_into_macros_no_opaque(token, false) {
                    let Some(syntax) = descended
                        .value
                        .parent()
                        .and_then(|node| node.ancestors().find_map(ast::MethodCallExpr::cast))
                    else {
                        continue;
                    };
                    let Some(name) = syntax.name_ref() else {
                        continue;
                    };
                    let Ok(Some(projected_span)) = self.projected_span(name.syntax()) else {
                        continue;
                    };
                    if !projected_calls.insert(projected_span) {
                        continue;
                    }
                    calls.push(RustMethodCall {
                        inferred: self.semantics.type_of_expr(&syntax.clone().into()),
                        target: self.semantics.resolve_method_call(&syntax),
                        syntax,
                        projected_span: Some(projected_span),
                    });
                }
            }
        }
        calls.into_iter()
    }

    /// Streams path syntax so callers can retain rust-analyzer resolution and substitutions.
    pub fn paths(&self) -> impl Iterator<Item = ast::Path> + '_ {
        self.root.syntax().descendants().filter_map(ast::Path::cast)
    }

    /// Streams only the top path of every path chain, skipping qualifier
    /// children (`a::b` yields one `a::b`, never an extra `a`), so consumers
    /// emit exactly one reference fact per written path chain.
    pub fn top_level_paths(&self) -> impl Iterator<Item = ast::Path> + '_ {
        self.root.syntax().descendants().filter_map(|syntax| {
            let path = ast::Path::cast(syntax)?;
            let nested = path
                .syntax()
                .parent()
                .is_some_and(|parent| parent.kind() == ra_ap_syntax::SyntaxKind::PATH);
            (!nested).then_some(path)
        })
    }

    /// Streams macro invocations with their unexpanded call-site syntax intact.
    pub fn macro_calls(&self) -> impl Iterator<Item = ast::MacroCall> + '_ {
        self.root
            .syntax()
            .descendants()
            .filter_map(ast::MacroCall::cast)
    }

    /// Resolves a path and keeps rust-analyzer's generic substitution unrendered.
    #[must_use]
    pub fn resolve_path(
        &self,
        path: &ast::Path,
    ) -> Option<(
        PathResolution,
        Option<ra_ap_hir::GenericSubstitution<'analysis>>,
    )> {
        self.semantics.resolve_path_with_subst(path)
    }

    /// Resolves a macro invocation to its definition without pretending expanded tokens are source.
    #[must_use]
    pub fn resolve_macro(&self, call: &ast::MacroCall) -> Option<Macro> {
        self.semantics.resolve_macro_call(call)
    }

    /// Returns the inferred type of one expression without rendering it to lossy text.
    #[must_use]
    pub fn inferred_type(&self, expression: &ast::Expr) -> Option<TypeInfo<'analysis>> {
        self.semantics.type_of_expr(expression)
    }

    /// Streams written let initializers with their analyzer-proven result types.
    pub fn inferred_let_initializers(
        &self,
    ) -> impl Iterator<Item = RustInferredExpression<'analysis>> + '_ {
        self.root.syntax().descendants().filter_map(|syntax| {
            let statement = ast::LetStmt::cast(syntax)?;
            let expression = statement.initializer()?;
            Some(RustInferredExpression {
                expression: expression.clone(),
                inferred: self.inferred_type(&expression),
            })
        })
    }

    /// Returns resolvable written bindings from source-level `use` items.
    pub fn reexports(&self) -> Vec<RustReexport> {
        let mut result = Vec::new();
        for syntax in self.root.syntax().descendants() {
            let Some(item) = ast::Use::cast(syntax) else {
                continue;
            };
            // A private `use` is a local alias, not a re-export: it never
            // enters the crate's public surface, so admitting it here can
            // mint a `Reexport` entity that shadows the identity of the same
            // name's genuine public binding in a different scope (observed
            // on `generic-array@1.4.5`, whose crate root privately
            // `use`-imports two names a nested `pub mod` also re-exports
            // under `#[cfg(feature = "internals")]`).
            if item.visibility().is_none() {
                continue;
            }
            let Some(tree) = item.use_tree() else {
                continue;
            };
            collect_reexports(self, &item, &tree, &mut result);
        }
        result
    }

    /// Converts one syntax node's local range into an exact validated original-byte span.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError`] if rust-analyzer produced coordinates outside `source`.
    pub fn span(&self, syntax: &ra_ap_syntax::SyntaxNode) -> Result<ByteSpan, RustAuthorityError> {
        checked_span(syntax, self.source)
    }

    /// Borrows exact original bytes for a previously validated span.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError`] when `span` lies outside this authority's source buffer.
    pub fn source_at(&self, span: ByteSpan) -> Result<&'analysis [u8], RustAuthorityError> {
        bytes_at(self.source, span)
    }

    /// Visits raw Rustdoc fragments without allocating or normalizing author text.
    pub fn visit_documentation(
        &self,
        syntax: &ra_ap_syntax::SyntaxNode,
        mut receive: impl FnMut(&str),
    ) {
        for comment in ast::DocCommentIter::from_syntax_node(syntax) {
            if let Some((text, _)) = comment.doc_comment() {
                receive(text.trim_start());
            }
        }
    }

    /// Rust module path of one resolved method's defining source file when it
    /// lives in another project-local file (`src/service` for both
    /// `src/service.rs` and `src/service/mod.rs`). Free functions use the same
    /// module path as methods.
    #[must_use]
    pub fn cross_file_method_package_path(&self, function: Function) -> Option<String> {
        let source = self.semantics.source(function)?;
        self.cross_file_package_path_from_syntax(source.value.syntax())
    }

    /// Rust module path of one resolved named field when it lives in another
    /// project-local file. Named struct fields share the same `src/...` path
    /// rules as types and functions.
    #[must_use]
    pub fn cross_file_field_package_path(&self, field: Field) -> Option<String> {
        let source = self.semantics.source(field)?;
        self.cross_file_package_path_from_syntax(source.value.syntax())
    }

    /// Rust module path of one resolved const, static, or enum variant when it
    /// lives in another project-local file.
    #[must_use]
    pub fn cross_file_value_package_path(&self, definition: ModuleDef) -> Option<String> {
        match definition {
            ModuleDef::Const(const_) => {
                let source = self.semantics.source(const_)?;
                self.cross_file_package_path_from_syntax(source.value.syntax())
            }
            ModuleDef::Static(static_) => {
                let source = self.semantics.source(static_)?;
                self.cross_file_package_path_from_syntax(source.value.syntax())
            }
            ModuleDef::EnumVariant(variant) => {
                let source = self.semantics.source(variant)?;
                self.cross_file_package_path_from_syntax(source.value.syntax())
            }
            _ => None,
        }
    }

    /// Rust module path of one resolved type or module definition when it lives
    /// in another project-local file. Structs, unions, enums, traits, type
    /// aliases, and modules share the same `src/...` path rules as functions.
    #[must_use]
    pub fn cross_file_type_package_path(&self, definition: ModuleDef) -> Option<String> {
        match definition {
            ModuleDef::Adt(adt) => {
                let source = self.semantics.source(adt)?;
                self.cross_file_package_path_from_syntax(source.value.syntax())
            }
            ModuleDef::Trait(trait_) => {
                let source = self.semantics.source(trait_)?;
                self.cross_file_package_path_from_syntax(source.value.syntax())
            }
            ModuleDef::TypeAlias(alias) => {
                let source = self.semantics.source(alias)?;
                self.cross_file_package_path_from_syntax(source.value.syntax())
            }
            ModuleDef::Module(module) => module
                .as_source_file_id(self.database)
                .and_then(|file_id| self.cross_file_package_path_from_file(file_id)),
            ModuleDef::Function(_)
            | ModuleDef::EnumVariant(_)
            | ModuleDef::Const(_)
            | ModuleDef::Static(_)
            | ModuleDef::BuiltinType(_)
            | ModuleDef::Macro(_) => None,
        }
    }

    fn cross_file_package_path_from_syntax(
        &self,
        syntax: &ra_ap_syntax::SyntaxNode,
    ) -> Option<String> {
        let range = self.semantics.original_range(syntax);
        if range.file_id == self.source_file {
            return None;
        }
        self.cross_file_package_path_from_file(range.file_id)
    }

    fn cross_file_package_path_from_file(&self, file_id: EditionedFileId) -> Option<String> {
        if file_id == self.source_file {
            return None;
        }
        let db = self.database;
        let file_id = file_id.file_id(db);
        let source_root_id = db.file_source_root(file_id).source_root_id(db);
        let source_root = db.source_root(source_root_id).source_root(db);
        if source_root.is_library {
            return None;
        }
        let vfs_path = source_root.path_for_file(&file_id)?;
        let abs_path = vfs_path.as_path()?.as_str();
        let marker = "/src/";
        let pos = abs_path.rfind(marker)?;
        let mut path = abs_path[pos + 1..].to_string();
        if path.ends_with(".rs") {
            path.truncate(path.len() - 3);
        }
        if let Some(stripped) = path.strip_suffix("/mod") {
            if !stripped.is_empty() {
                path = stripped.to_string();
            }
        }
        if path.is_empty() || path.contains('\\') || path.contains(':') {
            return None;
        }
        Some(path)
    }

    /// Maps a resolved HIR definition to an original local coordinate or explicit foreign state.
    #[must_use]
    pub fn definition_origin<Definition: HasSource>(
        &self,
        definition: Definition,
        kind: SemanticKind,
    ) -> SourceOrigin {
        let Some(source) = self.semantics.source(definition) else {
            return SourceOrigin::Foreign(kind);
        };
        let range = self.semantics.original_range(source.value.syntax());
        if range.file_id != self.source_file {
            return SourceOrigin::Foreign(kind);
        }
        ByteSpan::from_text_range(range.range)
            .and_then(|span| bytes_at(self.source, span).ok().map(|_| span))
            .map_or(SourceOrigin::Foreign(kind), SourceOrigin::Local)
    }

    /// Visits syntax diagnostics with exact coordinates and without storing analyzer prose.
    pub fn visit_syntax_diagnostics(&self, mut receive: impl FnMut(ByteSpan)) {
        for error in self.source_file.parse(self.database).errors() {
            if let Some(span) = ByteSpan::from_text_range(error.range()) {
                receive(span);
            }
        }
    }

    /// Projects one syntax node onto this authority's original source bytes,
    /// mapping through macro-expansion provenance to the real-file token
    /// range it grew from. `Ok(None)` means the node projects into another
    /// file, so no byte span of this source can honestly represent it.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError`] when a same-file projection produced a
    /// range this source buffer cannot address.
    pub fn projected_span(
        &self,
        syntax: &ra_ap_syntax::SyntaxNode,
    ) -> Result<Option<ByteSpan>, RustAuthorityError> {
        let range = self.semantics.original_range(syntax);
        if range.file_id != self.source_file {
            return Ok(None);
        }
        let span =
            ByteSpan::from_text_range(range.range).ok_or(RustAuthorityError::InvalidSpan {
                span: ByteSpan { start: 1, end: 0 },
                source_bytes: self.source.len(),
            })?;
        bytes_at(self.source, span).map(|_| span).map(Some)
    }

    /// True when one syntax node lives inside a macro expansion rather than
    /// the parsed original source tree; its raw text ranges address the
    /// expansion buffer, never the caller source.
    #[must_use]
    pub fn is_macro_expansion(&self, syntax: &ra_ap_syntax::SyntaxNode) -> bool {
        self.semantics.hir_file_for(syntax).is_macro()
    }

    /// Streams every written field-access expression with the field
    /// rust-analyzer resolved it to, when it resolved one.
    pub fn field_accesses(&self) -> impl Iterator<Item = RustFieldAccess> + '_ {
        self.root.syntax().descendants().filter_map(|syntax| {
            let syntax = ast::FieldExpr::cast(syntax)?;
            let target = self.resolve_field_target(&syntax);
            Some(RustFieldAccess { syntax, target })
        })
    }

    /// Resolves one field access to its named HIR field. Tuple-index
    /// accesses resolve to no named declaration, so they stay unresolved.
    #[must_use]
    pub fn resolve_field_target(&self, access: &ast::FieldExpr) -> Option<Field> {
        self.semantics
            .resolve_field(access)
            .and_then(|resolved| resolved.left())
    }

    /// Enumerates every HIR-provable declaration of this crate's module tree
    /// — including declarations that exist only through macro expansion and
    /// tuple-struct fields the written syntax tree does not cast — paired
    /// with their projected item span and projected name span. Declarations
    /// whose HIR origin projects outside this source, or whose name has no
    /// provable same-source spelling, carry `Foreign` or absent coordinates
    /// and stay unemitted rather than being guessed.
    pub fn module_declarations(&self) -> Vec<ModuleDeclaration> {
        let Some(root) = self.semantics.hir_file_to_module_def(self.source_file) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut modules = vec![root];
        let mut cursor = 0;
        while cursor < modules.len() {
            let module = modules[cursor];
            cursor += 1;
            for definition in module.declarations(self.database) {
                match definition {
                    ModuleDef::Function(definition) => {
                        self.record_module_declaration(
                            &mut out,
                            RustDefinition::Function(definition),
                        );
                    }
                    ModuleDef::Adt(adt) => match adt {
                        Adt::Struct(definition) => {
                            self.record_module_declaration(
                                &mut out,
                                RustDefinition::Record(Adt::Struct(definition)),
                            );
                            self.record_fields(&mut out, definition.fields(self.database));
                        }
                        Adt::Union(definition) => {
                            self.record_module_declaration(
                                &mut out,
                                RustDefinition::Record(Adt::Union(definition)),
                            );
                            self.record_fields(&mut out, definition.fields(self.database));
                        }
                        Adt::Enum(definition) => {
                            self.record_module_declaration(
                                &mut out,
                                RustDefinition::Enum(Adt::Enum(definition)),
                            );
                            for variant in definition.variants(self.database) {
                                self.record_module_declaration(
                                    &mut out,
                                    RustDefinition::Variant(variant),
                                );
                            }
                        }
                    },
                    ModuleDef::Const(definition) => self
                        .record_module_declaration(&mut out, RustDefinition::Constant(definition)),
                    ModuleDef::Static(definition) => {
                        self.record_module_declaration(
                            &mut out,
                            RustDefinition::Static(definition),
                        );
                    }
                    ModuleDef::Trait(definition) => {
                        self.record_module_declaration(&mut out, RustDefinition::Trait(definition));
                    }
                    ModuleDef::TypeAlias(definition) => self
                        .record_module_declaration(&mut out, RustDefinition::TypeAlias(definition)),
                    // Modules are covered by the written syntax walk or live
                    // in another file; variants are enumerated with their
                    // enum; builtins and macros carry no lane declaration.
                    ModuleDef::Module(_)
                    | ModuleDef::EnumVariant(_)
                    | ModuleDef::BuiltinType(_)
                    | ModuleDef::Macro(_) => {}
                }
            }
            for implementation in module.impl_defs(self.database) {
                self.record_module_declaration(
                    &mut out,
                    RustDefinition::Implementation(implementation),
                );
                for item in implementation.items(self.database) {
                    let definition = match item {
                        AssocItem::Function(definition) => RustDefinition::Function(definition),
                        AssocItem::Const(definition) => RustDefinition::Constant(definition),
                        AssocItem::TypeAlias(definition) => RustDefinition::TypeAlias(definition),
                    };
                    self.record_module_declaration(&mut out, definition);
                }
            }
            modules.extend(module.children(self.database));
        }
        out
    }

    /// Records one walked declaration with its projected item and name
    /// coordinates, keeping declarations whose origin projects outside this
    /// source off the list.
    fn record_module_declaration(
        &self,
        out: &mut Vec<ModuleDeclaration>,
        definition: RustDefinition,
    ) {
        let kind = definition.kind();
        let Some((name, item_node)) = self.definition_source_nodes(&definition) else {
            return;
        };
        let item = match self.projected_span(&item_node) {
            Ok(Some(span)) => SourceOrigin::Local(span),
            Ok(None) | Err(_) => SourceOrigin::Foreign(kind),
        };
        if !item.is_local() {
            return;
        }
        let name = name.and_then(|name| {
            let projected = self.projected_span(&name).ok().flatten()?;
            let projected_bytes = self.source_at(projected).ok()?;
            // `original_range` may conservatively map an expansion token to
            // the complete invocation. Such a range proves an origin but not
            // the declaration's exact name. Admit the coordinate only when
            // its caller-source bytes equal the authority syntax spelling.
            let authority_spelling = name.text().to_string();
            (projected_bytes == authority_spelling.as_bytes()).then_some(projected)
        });
        out.push(ModuleDeclaration {
            definition,
            item,
            name,
            syntax: item_node,
        });
    }

    /// Records one named field declaration; tuple fields carry no name node
    /// and rely on their canonical positional name at the projection site.
    fn record_fields(&self, out: &mut Vec<ModuleDeclaration>, fields: Vec<Field>) {
        for field in fields {
            self.record_module_declaration(out, RustDefinition::Field(field));
        }
    }

    /// Borrows the projected name node and whole-item node of one walked
    /// declaration, when rust-analyzer retains its source.
    fn definition_source_nodes(
        &self,
        definition: &RustDefinition,
    ) -> Option<(Option<ra_ap_syntax::SyntaxNode>, ra_ap_syntax::SyntaxNode)> {
        let (name, item) = match definition {
            RustDefinition::Function(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            RustDefinition::Record(adt) | RustDefinition::Enum(adt) => match adt {
                Adt::Struct(definition) => {
                    let source = self.semantics.source(*definition)?;
                    (
                        source.value.name().map(|name| name.syntax().clone()),
                        source.value.syntax().clone(),
                    )
                }
                Adt::Union(definition) => {
                    let source = self.semantics.source(*definition)?;
                    (
                        source.value.name().map(|name| name.syntax().clone()),
                        source.value.syntax().clone(),
                    )
                }
                Adt::Enum(definition) => {
                    let source = self.semantics.source(*definition)?;
                    (
                        source.value.name().map(|name| name.syntax().clone()),
                        source.value.syntax().clone(),
                    )
                }
            },
            RustDefinition::Variant(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            RustDefinition::Field(definition) => {
                let source = self.semantics.source(*definition)?;
                let name = match &source.value {
                    FieldSource::Named(field) => field.name().map(|name| name.syntax().clone()),
                    FieldSource::Pos(_) => None,
                };
                (name, source.value.syntax().clone())
            }
            RustDefinition::Trait(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            RustDefinition::Implementation(definition) => {
                let source = self.semantics.source(*definition)?;
                let name = implementation_name(&source.value).map(|name| name.syntax().clone());
                (name, source.value.syntax().clone())
            }
            RustDefinition::TypeAlias(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            RustDefinition::Constant(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            RustDefinition::Static(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            // Macro definitions stay out of the declaration lane; walked
            // modules are covered by the written syntax walk or live in
            // another file.
            RustDefinition::Macro(_) | RustDefinition::Module(_) => return None,
        };
        Some((name, item))
    }

    /// Converts one source declaration into a borrowed HIR definition.
    fn declaration(&self, syntax: ra_ap_syntax::SyntaxNode) -> Option<RustDeclaration> {
        let definition = if let Some(item) = ast::RecordField::cast(syntax.clone()) {
            RustDefinition::Field(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Variant::cast(syntax.clone()) {
            RustDefinition::Variant(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Macro::cast(syntax.clone()) {
            RustDefinition::Macro(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Module::cast(syntax.clone()) {
            RustDefinition::Module(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Trait::cast(syntax.clone()) {
            RustDefinition::Trait(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Impl::cast(syntax.clone()) {
            RustDefinition::Implementation(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Fn::cast(syntax.clone()) {
            RustDefinition::Function(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Struct::cast(syntax.clone()) {
            RustDefinition::Record(self.semantics.to_def(&item)?.into())
        } else if let Some(item) = ast::Union::cast(syntax.clone()) {
            RustDefinition::Record(self.semantics.to_def(&item)?.into())
        } else if let Some(item) = ast::Enum::cast(syntax.clone()) {
            RustDefinition::Enum(self.semantics.to_def(&item)?.into())
        } else if let Some(item) = ast::TypeAlias::cast(syntax.clone()) {
            RustDefinition::TypeAlias(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Const::cast(syntax.clone()) {
            RustDefinition::Constant(self.semantics.to_def(&item)?)
        } else {
            let item = ast::Static::cast(syntax.clone())?;
            RustDefinition::Static(self.semantics.to_def(&item)?)
        };
        Some(RustDeclaration {
            kind: definition.kind(),
            definition,
            syntax,
        })
    }
}

/// Source-coordinate fact carried as bytes, never characters or UTF-16 columns.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ByteSpan {
    /// First included original source byte.
    pub start: u32,
    /// First excluded original source byte.
    pub end: u32,
}

impl ByteSpan {
    /// Converts rust-analyzer's compact text range without widening or changing coordinate units.
    fn from_text_range(range: ra_ap_syntax::TextRange) -> Option<Self> {
        let start = u32::from(range.start());
        let end = u32::from(range.end());
        (start <= end).then_some(Self { start, end })
    }
}

/// Rust declaration category proved by HIR rather than inferred from token spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticKind {
    /// A module declaration.
    Module,
    /// A function or associated method.
    Function,
    /// A named or tuple field.
    Field,
    /// A struct or union.
    Record,
    /// An enum.
    Enum,
    /// A trait.
    Trait,
    /// An inherent or trait implementation.
    Implementation,
    /// A type alias.
    TypeAlias,
    /// A constant.
    Constant,
    /// A static declaration.
    Static,
    /// A macro definition or invocation.
    Macro,
    /// A named enum variant.
    Variant,
    /// A lexical value binding.
    LocalBinding,
    /// A type or const generic parameter.
    GenericParameter,
    /// A compiler-known item with no source-owned declaration coordinate.
    Builtin,
}

/// Local or foreign status of a resolved HIR definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceOrigin {
    /// Definition maps to an exact span in the authority source buffer.
    Local(ByteSpan),
    /// Definition belongs to another source file, dependency, or non-source compiler item.
    Foreign(SemanticKind),
}

impl SourceOrigin {
    /// True when the definition resolved into this authority's exact source buffer.
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Local(_))
    }
}

/// One source declaration paired with its actual rust-analyzer HIR definition.
pub struct RustDeclaration {
    /// HIR-proven category.
    pub kind: SemanticKind,
    /// Exact typed HIR definition consumed by the IR lowerer.
    pub definition: RustDefinition,
    /// Original source syntax preserving documentation, attributes, and byte coordinates.
    pub syntax: ra_ap_syntax::SyntaxNode,
}

/// Rust-analyzer definitions retained without rendering generic or type information.
#[allow(
    clippy::large_enum_variant,
    reason = "The variant selects a monomorphized HIR handle and never reaches stored IR."
)]
pub enum RustDefinition {
    /// Named or tuple field declaration.
    Field(Field),
    /// Enum-case declaration.
    Variant(EnumVariant),
    /// Declarative or source-defined macro declaration.
    Macro(Macro),
    /// Source module declaration.
    Module(Module),
    /// Trait declaration.
    Trait(Trait),
    /// Inherent or trait implementation.
    Implementation(Impl),
    /// Free or associated function.
    Function(Function),
    /// Struct or union definition.
    Record(Adt),
    /// Enum definition.
    Enum(Adt),
    /// Type alias definition.
    TypeAlias(TypeAlias),
    /// Constant definition.
    Constant(Const),
    /// Static definition.
    Static(Static),
}

impl RustDefinition {
    /// Returns the closed semantic category selected by this HIR definition.
    #[must_use]
    pub const fn kind(&self) -> SemanticKind {
        match self {
            Self::Field(_) => SemanticKind::Field,
            Self::Variant(_) => SemanticKind::Variant,
            Self::Macro(_) => SemanticKind::Macro,
            Self::Module(_) => SemanticKind::Module,
            Self::Trait(_) => SemanticKind::Trait,
            Self::Implementation(_) => SemanticKind::Implementation,
            Self::Function(_) => SemanticKind::Function,
            Self::Record(_) => SemanticKind::Record,
            Self::Enum(_) => SemanticKind::Enum,
            Self::TypeAlias(_) => SemanticKind::TypeAlias,
            Self::Constant(_) => SemanticKind::Constant,
            Self::Static(_) => SemanticKind::Static,
        }
    }

    /// Borrows the HIR type owned by this definition without rendering or allocating it.
    #[must_use]
    pub fn semantic_type<'analysis>(
        &self,
        database: &'analysis ra_ap_ide_db::RootDatabase,
    ) -> Option<ra_ap_hir::Type<'analysis>> {
        match self {
            Self::Field(definition) => Some(definition.ty(database)),
            Self::Variant(definition) => Some(definition.constructor_ty(database)),
            Self::Macro(_) | Self::Module(_) | Self::Trait(_) => None,
            Self::Implementation(definition) => Some(definition.self_ty(database)),
            Self::Function(definition) => Some(definition.fn_ptr_type(database)),
            Self::Record(definition) | Self::Enum(definition) => Some(definition.ty(database)),
            Self::TypeAlias(definition) => Some(definition.ty(database)),
            Self::Constant(definition) => Some(definition.ty(database)),
            Self::Static(definition) => Some(definition.ty(database)),
        }
    }

    /// Borrows this definition's HIR generic parameters.
    ///
    /// Order follows rust-analyzer: lifetimes first, then type and const
    /// parameters. Callers that emit positionally must re-pair with the
    /// written parameter list rather than trusting this order.
    ///
    /// Every definition rust-analyzer models as a public [`ra_ap_hir::GenericDef`]
    /// is projected directly. Fields, enum variants, modules, and macros have no
    /// such handle and yield an empty vector rather than an approximated one.
    #[must_use]
    pub fn generic_params<'analysis>(
        &self,
        database: &'analysis ra_ap_ide_db::RootDatabase,
    ) -> Vec<ra_ap_hir::GenericParam> {
        let generic = match self {
            Self::Field(_) | Self::Variant(_) | Self::Macro(_) | Self::Module(_) => {
                return Vec::new();
            }
            Self::Trait(definition) => ra_ap_hir::GenericDef::from(*definition),
            Self::Implementation(definition) => ra_ap_hir::GenericDef::from(*definition),
            Self::Function(definition) => ra_ap_hir::GenericDef::from(*definition),
            Self::Record(definition) | Self::Enum(definition) => {
                ra_ap_hir::GenericDef::from(*definition)
            }
            Self::TypeAlias(definition) => ra_ap_hir::GenericDef::from(*definition),
            Self::Constant(definition) => ra_ap_hir::GenericDef::from(*definition),
            Self::Static(definition) => ra_ap_hir::GenericDef::from(*definition),
        };
        generic.params(database)
    }
}

/// One method call with exact AST call-site, inferred result type, and static dispatch target.
pub struct RustMethodCall<'analysis> {
    /// Original call syntax including generic arguments and macro provenance.
    pub syntax: ast::MethodCallExpr,
    /// Original and adjusted inferred result types, retained unrendered.
    pub inferred: Option<TypeInfo<'analysis>>,
    /// Concrete function selected by static method dispatch, when rust-analyzer can resolve one.
    pub target: Option<Function>,
    /// Original-source span for a call discovered through macro expansion.
    pub projected_span: Option<ByteSpan>,
}

/// One written field-access expression with its resolved named HIR field.
pub struct RustFieldAccess {
    /// Original access syntax (`base.field`).
    pub syntax: ast::FieldExpr,
    /// Named field selected by the analyzer, when the base type provably
    /// owns one; tuple-index accesses stay unresolved.
    pub target: Option<Field>,
}

/// One written expression and its unrendered inferred type, when inference succeeded.
pub struct RustInferredExpression<'analysis> {
    /// The original expression syntax.
    pub expression: ast::Expr,
    /// The analyzer's semantic result, absent for unresolved expressions.
    pub inferred: Option<TypeInfo<'analysis>>,
}

/// One written local spelling in a resolvable `use` binding.
pub struct RustReexport {
    /// The use item, retained for visibility and documentation projection.
    pub item: ast::Use,
    /// Exact written local name span.
    pub name: ra_ap_syntax::SyntaxNode,
    /// Whether rust-analyzer resolved the imported path.
    pub resolved: bool,
}

fn collect_reexports<'analysis>(
    authority: &RustAuthority<'analysis>,
    item: &ast::Use,
    tree: &ast::UseTree,
    result: &mut Vec<RustReexport>,
) {
    if tree.star_token().is_some() {
        return;
    }
    if let Some(list) = tree.use_tree_list() {
        for child in list.use_trees() {
            collect_reexports(authority, item, &child, result);
        }
        return;
    }
    let Some(path) = tree.path() else {
        return;
    };
    let name = tree
        .rename()
        .and_then(|rename| rename.name())
        .map(|name| name.syntax().clone())
        .or_else(|| {
            path.segments()
                .last()
                .and_then(|segment| segment.name_ref())
                .map(|name| name.syntax().clone())
        });
    let Some(name) = name else {
        return;
    };
    result.push(RustReexport {
        item: item.clone(),
        name,
        resolved: authority.resolve_path(&path).is_some(),
    });
}

/// One HIR-walked declaration with its projected original-source coordinates.
pub struct ModuleDeclaration {
    /// Exact typed HIR definition consumed by the IR lowerer.
    pub definition: RustDefinition,
    /// Projected whole-item origin: `Local` only when this source owns it.
    pub item: SourceOrigin,
    /// Projected name span, when the name has a provable same-source
    /// spelling. Tuple fields carry none and use their canonical positional
    /// name at the projection site.
    pub name: Option<ByteSpan>,
    /// The declaration's own syntax node, whatever file it was parsed in.
    pub syntax: ra_ap_syntax::SyntaxNode,
}

/// Borrows the written self-type leaf name of one implementation, the lane's
/// implementation naming rule.
fn implementation_name(implementation: &ast::Impl) -> Option<ast::NameRef> {
    let self_ty = implementation.self_ty()?;
    let ast::Type::PathType(path_type) = self_ty else {
        return None;
    };
    let segment = path_type.path()?.segments().last()?;
    segment.name_ref()
}

/// Failure to establish or query one rust-analyzer authority transaction.
#[derive(Debug, thiserror::Error)]
pub enum RustAuthorityError {
    /// The caller cancelled before the authority entered its next controllable phase.
    #[error("Rust semantic authority was cancelled")]
    Cancelled,
    /// The caller's monotonic work deadline elapsed before the next authority phase.
    #[error("Rust semantic authority exceeded its deadline")]
    DeadlineExceeded,
    /// Native toolchain discovery rejected the caller-selected compiler.
    #[error("Rust toolchain authority failed: {0}")]
    Toolchain(#[from] LoadError),
    /// The requested project root could not be canonicalized.
    #[error("cannot open Rust project root {path}: {source}")]
    ProjectRoot {
        /// Caller-selected project root.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The caller-selected crate root source could not be canonicalized.
    #[error("cannot open Rust crate root {path}: {source}")]
    ProjectSource {
        /// Caller-selected crate root source path.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The root is not a Cargo package manifest location.
    #[error("Rust project is missing Cargo manifest {path}")]
    MissingManifest {
        /// Expected manifest path.
        path: PathBuf,
    },
    /// The caller-selected crate root is not a regular source file.
    #[error("Rust crate root is not a regular file: {path}")]
    SourceNotFile {
        /// Caller-selected unusable crate root path.
        path: PathBuf,
    },
    /// A per-source request resolved outside the workspace's exact Cargo package root.
    #[error("selected Rust source path {path} is outside Cargo package root {root}")]
    SourceOutsidePackage {
        /// Exact Cargo package root loaded into this workspace.
        root: PathBuf,
        /// Canonical source path rejected by package containment.
        path: PathBuf,
    },
    /// The selected root source exceeds the caller-owned admission budget.
    #[error("Rust source is {actual} bytes, exceeding the {maximum:?}-byte authority budget")]
    SourceBudget {
        /// Observed root-source length before workspace loading.
        actual: u64,
        /// Caller-selected maximum source bytes.
        maximum: SourceByteLimit,
    },
    /// Cargo selected an edition that conflicts with the caller's sealed profile.
    #[error("Rust Cargo edition {observed:?} conflicts with requested profile {requested:?}")]
    EditionMismatch {
        /// Profile bound into the compile request.
        requested: RustEdition,
        /// Edition derived by rust-analyzer from Cargo metadata.
        observed: RustEdition,
    },
    /// rust-analyzer could not load the Cargo project graph.
    #[error("rust-analyzer could not load {root}: {source}")]
    Workspace {
        /// Caller-selected project root.
        root: PathBuf,
        /// Exact project-model failure.
        #[source]
        source: anyhow::Error,
    },
    /// A workspace request disagreed with the exact keyed source path set.
    #[error("Rust workspace source path set does not match its operation key")]
    SessionFrontierMismatch,
    /// A new virtual source could not be assigned to one local RA source root.
    #[error(
        "rust-analyzer source root for a new virtual source could not be identified unambiguously"
    )]
    SessionSourceRootAmbiguous,
    /// Rebuilding RA root membership would exceed the bounded path-copy budget.
    #[error("Rust workspace source-root update exceeds the {maximum}-file membership limit")]
    SessionSourceRootLimit {
        /// Largest number of source-root members copied by one operation.
        maximum: usize,
    },
    /// A workspace source path is not an admitted normalized package path.
    #[error("Rust workspace session rejected source path {path}")]
    SessionSourcePath {
        /// Rejected package-relative source path.
        path: PathBuf,
    },
    /// A workspace source set is empty or exceeds its fixed bound.
    #[error("Rust workspace session source count {actual} exceeds allowed range 1..={maximum}")]
    SessionSourceCardinality {
        /// Observed source path count.
        actual: usize,
        /// Largest admitted source path count.
        maximum: usize,
    },
    /// A caller attempted to use a workspace for another root or edition.
    #[error("Rust workspace session is bound to another package root or edition")]
    WorkspaceBindingMismatch,
    /// The declared crate root could not be read after project loading.
    #[error("cannot read Rust crate root {path}: {source}")]
    SourceRead {
        /// Exact requested source path.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The loader did not admit the requested crate root into its VFS.
    #[error("rust-analyzer did not load source path {path}")]
    SourceNotLoaded {
        /// Exact requested source path.
        path: PathBuf,
    },
    /// The selected file exists in the package VFS but is outside all active Cargo targets.
    #[error(
        "selected Rust source is cfg-inactive or detached from every active Cargo target: {path}"
    )]
    DetachedSource {
        /// Exact selected package source without active Cargo HIR ownership.
        path: PathBuf,
    },
    /// The compiler request bytes differ from the exact source text in the Cargo VFS.
    #[error(
        "Rust request source differs from Cargo VFS text (request {expected} bytes, VFS {observed} bytes)"
    )]
    SourceBinding {
        /// Byte count retained by the admitted compiler request source.
        expected: usize,
        /// Byte count loaded by rust-analyzer from its package VFS.
        observed: usize,
    },
    /// A rust-analyzer byte range exceeded the supplied source buffer.
    #[error("rust-analyzer emitted source range {span:?} outside {source_bytes} bytes")]
    InvalidSpan {
        /// Returned byte coordinate.
        span: ByteSpan,
        /// Exact loaded source length.
        source_bytes: usize,
    },
    /// A platform address cannot represent rust-analyzer's source coordinate.
    #[error("rust-analyzer coordinate {span:?} cannot fit this platform: {source}")]
    Coordinate {
        /// Returned byte coordinate.
        span: ByteSpan,
        /// Exact failed width conversion.
        #[source]
        source: std::num::TryFromIntError,
    },
    /// A required HIR fact was absent after successful Cargo graph loading.
    #[error("rust-analyzer did not produce required semantic fact {fact:?}")]
    MissingSemanticFact {
        /// Exact fact category required by the caller's lowering contract.
        fact: SemanticKind,
    },
    /// The shared canonical fact lane rejected a complete borrowed HIR declaration.
    #[error("Rust semantic admission rejected a declaration: {cause}")]
    Admission {
        /// Exact canonical admission terminal.
        #[source]
        cause: backend_semantic::vocabulary::LoweringUnsupported,
    },
    /// Inference returned an error type where a resolved semantic type was required.
    #[error("rust-analyzer produced an unresolved inferred type")]
    UnresolvedInferredType,
}

/// Converts one local syntax range into validated byte coordinates.
fn checked_span(
    syntax: &ra_ap_syntax::SyntaxNode,
    source: &[u8],
) -> Result<ByteSpan, RustAuthorityError> {
    let span =
        ByteSpan::from_text_range(syntax.text_range()).ok_or(RustAuthorityError::InvalidSpan {
            span: ByteSpan { start: 1, end: 0 },
            source_bytes: source.len(),
        })?;
    bytes_at(source, span).map(|_| span)
}

/// Borrows a source span after checked coordinate conversion and range validation.
fn bytes_at(source: &[u8], span: ByteSpan) -> Result<&[u8], RustAuthorityError> {
    let start = usize::try_from(span.start)
        .map_err(|source| RustAuthorityError::Coordinate { span, source })?;
    let end = usize::try_from(span.end)
        .map_err(|source| RustAuthorityError::Coordinate { span, source })?;
    source
        .get(start..end)
        .ok_or(RustAuthorityError::InvalidSpan {
            span,
            source_bytes: source.len(),
        })
}

/// Converts rust-analyzer's Cargo-derived edition into the sealed compiler profile.
const fn rust_edition(edition: ra_ap_syntax::Edition) -> RustEdition {
    match edition {
        ra_ap_syntax::Edition::Edition2015 => RustEdition::Rust2015,
        ra_ap_syntax::Edition::Edition2018 => RustEdition::Rust2018,
        ra_ap_syntax::Edition::Edition2021 => RustEdition::Rust2021,
        ra_ap_syntax::Edition::Edition2024 => RustEdition::Rust2024,
    }
}
