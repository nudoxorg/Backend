//! Owns the bounded rust-analyzer authority transaction for one Cargo source root.
//! Borrows HIR definitions, types, substitutions, and source maps directly into a caller closure.
//! Never renders, copies, or serializes semantic facts before the shared IR lowerer consumes them.

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fmt, fs,
    io::{self, Read},
    ops::Deref,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use backend_semantic::vocabulary::{RustEdition, Stage};
use ra_ap_base_db::{
    EditionedFileId, FileSet, SourceDatabase, SourceRoot, SourceRootId, all_crates,
};
use ra_ap_hir::{
    Adt, AssocItem, CfgExpr, CfgOptions, Const, EnumVariant, Field, FieldSource, Function,
    HasSource, Impl, Macro, Module, ModuleDef, PathResolution, Semantics, Static, Trait, TypeAlias,
    TypeInfo,
};
use ra_ap_hir_def::nameres::{ModuleOrigin, crate_def_map, diagnostics::DefDiagnosticKind};
use ra_ap_ide_db::{
    ChangeWithProcMacros, LibraryRoots, LocalRoots, RootDatabase, documentation::HasDocs,
};
use ra_ap_project_model::{CargoConfig, CargoFeatures, RustLibSource};
use ra_ap_syntax::{
    AstNode, AstToken, TextRange, TextSize,
    ast::{self, HasName, HasVisibility},
};
use ra_ap_vfs::{AbsPathBuf, FileExcluded, FileId, Vfs, VfsPath};

use crate::legacy::{LoadError, RustToolchain};

/// Largest sorted package source path set admitted by one Rust authority lane.
pub const MAX_RUST_WORKSPACE_SESSION_SOURCES: usize = 100_000;

/// Maximum RA source-root path entries copied while adding virtual files.
const MAX_RUST_WORKSPACE_ROOT_MEMBERSHIP_FILES: usize = 250_000;

/// Largest active DefMap ownership index retained for one loaded package graph.
const MAX_RUST_SOURCE_OWNERSHIP_MODULES: usize = MAX_RUST_WORKSPACE_ROOT_MEMBERSHIP_FILES;

/// Maximum number of `include_str!` inputs admitted from Rustdoc attributes in one operation.
const MAX_RUST_DOCUMENTATION_INPUTS: usize = 1024;

/// Maximum combined bytes read for Rustdoc `include_str!` inputs in one operation.
const MAX_RUST_DOCUMENTATION_INPUT_BYTES: usize = 64 * 1024 * 1024;

/// Maximum package-relative active HIR roots retained on a detached-source error.
const MAX_DETACHED_HIR_ROOT_SAMPLE: usize = 16;

/// Maximum retained stdout or stderr from one metadata subprocess.
const MAX_CARGO_METADATA_STREAM_BYTES: usize = 64 * 1024 * 1024;

/// Poll interval for cooperative Cargo metadata cancellation and deadline checks.
const CARGO_METADATA_POLL_INTERVAL: Duration = Duration::from_millis(20);

static CARGO_METADATA_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Finds literal source-relative include paths used by active Rustdoc attributes.
///
/// rust-analyzer remains responsible for evaluating the attributes and expanding
/// `include_str!`; this syntax pass only makes bounded package files visible to
/// its VFS first. Expressions such as `concat!`, `env!`, or generated paths fail
/// closed because they cannot be safely admitted before RA expansion.
fn documentation_include_paths(
    source: &ra_ap_syntax::SyntaxNode,
    cfg: &CfgOptions,
    source_path: &Path,
) -> Result<Vec<String>, RustAuthorityError> {
    let mut includes = Vec::new();
    for node in source.descendants() {
        let attributes = node
            .children()
            .filter_map(ast::Attr::cast)
            .collect::<Vec<_>>();
        if !attributes
            .iter()
            .filter_map(ast::Attr::meta)
            .any(|meta| meta_contains_documentation_expression(&meta))
        {
            continue;
        }

        let mut disabled = false;
        for meta in attributes.iter().filter_map(ast::Attr::meta) {
            if meta_disables_documented_item(&meta, cfg, source_path)? {
                disabled = true;
                break;
            }
        }
        if disabled {
            continue;
        }

        for meta in attributes.iter().filter_map(ast::Attr::meta) {
            collect_active_documentation_includes(&meta, cfg, source_path, &mut includes)?;
        }
    }
    Ok(includes)
}

fn meta_contains_documentation_expression(meta: &ast::Meta) -> bool {
    match meta {
        ast::Meta::KeyValueMeta(meta) => {
            meta.path().is_some_and(|path| path.to_string() == "doc")
                && meta.expr().is_some_and(|expression| {
                    !matches!(expression, ast::Expr::Literal(literal)
                        if literal.syntax().first_token().and_then(ast::String::cast).is_some())
                })
        }
        ast::Meta::CfgAttrMeta(meta) => meta
            .metas()
            .any(|nested| meta_contains_documentation_expression(&nested)),
        _ => false,
    }
}

fn meta_disables_documented_item(
    meta: &ast::Meta,
    cfg: &CfgOptions,
    source_path: &Path,
) -> Result<bool, RustAuthorityError> {
    match meta {
        ast::Meta::CfgMeta(meta) => {
            let Some(predicate) = meta.cfg_predicate() else {
                return Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                ));
            };
            match cfg.check(&CfgExpr::parse_from_ast(predicate)) {
                Some(false) => Ok(true),
                Some(true) => Ok(false),
                None => Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                )),
            }
        }
        ast::Meta::CfgAttrMeta(meta) => {
            let Some(predicate) = meta.cfg_predicate() else {
                return Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                ));
            };
            match cfg.check(&CfgExpr::parse_from_ast(predicate)) {
                Some(false) => Ok(false),
                Some(true) => {
                    for nested in meta.metas() {
                        if meta_disables_documented_item(&nested, cfg, source_path)? {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                }
                None if meta
                    .metas()
                    .any(|nested| meta_contains_documentation_expression(&nested)) =>
                {
                    Err(unsupported_documentation_expression(
                        source_path,
                        meta.syntax(),
                    ))
                }
                None => Ok(false),
            }
        }
        _ => Ok(false),
    }
}

fn collect_active_documentation_includes(
    meta: &ast::Meta,
    cfg: &CfgOptions,
    source_path: &Path,
    includes: &mut Vec<String>,
) -> Result<(), RustAuthorityError> {
    match meta {
        ast::Meta::KeyValueMeta(meta)
            if meta.path().is_some_and(|path| path.to_string() == "doc") =>
        {
            let Some(expression) = meta.expr() else {
                return Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                ));
            };
            if matches!(&expression, ast::Expr::Literal(literal)
                if literal.syntax().first_token().and_then(ast::String::cast).is_some())
            {
                return Ok(());
            }
            let ast::Expr::MacroExpr(expression) = expression else {
                return Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                ));
            };
            let Some(call) = expression.macro_call() else {
                return Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                ));
            };
            let macro_name = call
                .path()
                .and_then(|path| path.segments().last())
                .and_then(|segment| segment.name_ref())
                .map(|name| name.text().to_string());
            if macro_name.as_deref() != Some("include_str") {
                return Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                ));
            }
            let Some(tree) = call.token_tree() else {
                return Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                ));
            };
            let left_delimiter = tree.left_delimiter_token().map(|token| token.text_range());
            let right_delimiter = tree.right_delimiter_token().map(|token| token.text_range());
            let arguments = tree
                .token_trees_and_tokens()
                .filter_map(|element| element.into_token())
                .filter(|token| {
                    !matches!(
                        token.kind(),
                        ra_ap_syntax::SyntaxKind::WHITESPACE | ra_ap_syntax::SyntaxKind::COMMENT
                    ) && Some(token.text_range()) != left_delimiter
                        && Some(token.text_range()) != right_delimiter
                })
                .collect::<Vec<_>>();
            let [argument] = arguments.as_slice() else {
                return Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                ));
            };
            let Some(argument) = ast::String::cast(argument.clone()) else {
                return Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                ));
            };
            let include = argument
                .value()
                .map_err(|_| unsupported_documentation_expression(source_path, meta.syntax()))?;
            if include.is_empty() || Path::new(include.as_ref()).is_absolute() {
                return Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                ));
            }
            includes.push(include.into_owned());
            Ok(())
        }
        ast::Meta::CfgAttrMeta(meta) => {
            let Some(predicate) = meta.cfg_predicate() else {
                return Err(unsupported_documentation_expression(
                    source_path,
                    meta.syntax(),
                ));
            };
            match cfg.check(&CfgExpr::parse_from_ast(predicate)) {
                Some(true) => {
                    for nested in meta.metas() {
                        collect_active_documentation_includes(&nested, cfg, source_path, includes)?;
                    }
                    Ok(())
                }
                Some(false) => Ok(()),
                None if meta
                    .metas()
                    .any(|nested| meta_contains_documentation_expression(&nested)) =>
                {
                    Err(unsupported_documentation_expression(
                        source_path,
                        meta.syntax(),
                    ))
                }
                None => Ok(()),
            }
        }
        _ => Ok(()),
    }
}

fn unsupported_documentation_expression(
    path: &Path,
    syntax: &ra_ap_syntax::SyntaxNode,
) -> RustAuthorityError {
    RustAuthorityError::UnsupportedDocumentationExpression {
        path: path.to_path_buf(),
        expression: syntax.text().to_string(),
    }
}

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
    /// Exact active module membership for this database, built before it is shared.
    source_ownership_index: Option<RustSourceOwnershipIndex>,
}

#[derive(Clone, Copy)]
struct RustSourceOwnershipEntry {
    krate: ra_ap_hir::Crate,
    scope: RustSourceScope,
}

enum RustSourceOwners {
    Unique(RustSourceOwnershipEntry),
    Ambiguous(Vec<RustSourceOwnershipEntry>),
}

struct RustSourceOwnershipIndex {
    owners_by_file: HashMap<FileId, RustSourceOwners>,
    active_hir_roots: RustActiveHirRootInventory,
}

/// Bounded evidence about package crate roots visible to HIR when one source
/// cannot be assigned to an active target.
///
/// Paths are relative to the admitted package root. The count includes every
/// active package crate observed; the root list retains only the first bounded
/// sample in rust-analyzer's deterministic crate order.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RustActiveHirRootInventory {
    /// Exact number of active HIR crates whose roots are inside this package.
    pub package_crate_count: usize,
    /// Bounded package-relative root sample.
    pub package_relative_roots: Box<[PathBuf]>,
    /// Number of package crate entries omitted after filling the sample.
    pub omitted_package_crates: usize,
}

impl RustWorkspace {
    fn build_source_ownership_index(
        &self,
        control: RustAnalysisControl<'_>,
    ) -> Result<RustSourceOwnershipIndex, RustAuthorityError> {
        control.check()?;
        let crate_ids = all_crates(&self.database);
        if crate_ids.len() > MAX_RUST_SOURCE_OWNERSHIP_MODULES {
            return Err(RustAuthorityError::SourceOwnershipIndexLimit {
                maximum: MAX_RUST_SOURCE_OWNERSHIP_MODULES,
            });
        }
        let package_root = AbsPathBuf::assert_utf8(self.root.clone());
        let mut package_crate_count = 0_usize;
        let mut package_relative_roots = Vec::with_capacity(MAX_DETACHED_HIR_ROOT_SAMPLE);
        let mut package_crates = Vec::<(ra_ap_hir::Crate, FileId)>::new();
        package_crates
            .try_reserve(crate_ids.len().min(16))
            .map_err(|_| RustAuthorityError::SourceOwnershipIndexLimit {
                maximum: MAX_RUST_SOURCE_OWNERSHIP_MODULES,
            })?;
        for crate_id in crate_ids.iter().copied() {
            control.check()?;
            let krate = ra_ap_hir::Crate::from(crate_id);
            let root_file = krate.root_file(&self.database);
            let Some(root_path) = self.vfs.file_path(root_file).as_path() else {
                continue;
            };
            let Some(relative) = root_path.strip_prefix(package_root.as_path()) else {
                continue;
            };
            package_crate_count = package_crate_count.saturating_add(1);
            if package_crate_count > MAX_RUST_SOURCE_OWNERSHIP_MODULES {
                return Err(RustAuthorityError::SourceOwnershipIndexLimit {
                    maximum: MAX_RUST_SOURCE_OWNERSHIP_MODULES,
                });
            }
            if package_relative_roots.len() < MAX_DETACHED_HIR_ROOT_SAMPLE {
                package_relative_roots.push(PathBuf::from(relative.as_str()));
            }
            if package_crates.len() == package_crates.capacity() {
                package_crates.try_reserve(1).map_err(|_| {
                    RustAuthorityError::SourceOwnershipIndexLimit {
                        maximum: MAX_RUST_SOURCE_OWNERSHIP_MODULES,
                    }
                })?;
            }
            package_crates.push((krate, root_file));
        }
        let mut owners_by_file = HashMap::<FileId, RustSourceOwners>::new();
        owners_by_file
            .try_reserve(
                package_crates
                    .len()
                    .saturating_mul(4)
                    .min(MAX_RUST_SOURCE_OWNERSHIP_MODULES),
            )
            .map_err(|_| RustAuthorityError::SourceOwnershipIndexLimit {
                maximum: MAX_RUST_SOURCE_OWNERSHIP_MODULES,
            })?;
        let mut observed_modules = 0_usize;
        for (krate, root_file) in package_crates {
            control.check()?;
            let def_map = crate_def_map(&self.database, krate.base());
            for (_, module) in def_map.modules() {
                control.check()?;
                observed_modules = observed_modules.saturating_add(1);
                if observed_modules > MAX_RUST_SOURCE_OWNERSHIP_MODULES {
                    return Err(RustAuthorityError::SourceOwnershipIndexLimit {
                        maximum: MAX_RUST_SOURCE_OWNERSHIP_MODULES,
                    });
                }
                if !matches!(
                    module.origin,
                    ModuleOrigin::CrateRoot { .. } | ModuleOrigin::File { .. }
                ) {
                    continue;
                }
                let Some(definition) = module.origin.file_id() else {
                    continue;
                };
                let file_id = definition.file_id(&self.database);
                let Some(path) = self.vfs.file_path(file_id).as_path() else {
                    continue;
                };
                if !path.starts_with(package_root.as_path()) {
                    continue;
                }
                let owner = RustSourceOwnershipEntry {
                    krate,
                    scope: if file_id == root_file {
                        RustSourceScope::CargoTargetRoot
                    } else {
                        RustSourceScope::CargoModule
                    },
                };
                if !owners_by_file.contains_key(&file_id) {
                    owners_by_file.try_reserve(1).map_err(|_| {
                        RustAuthorityError::SourceOwnershipIndexLimit {
                            maximum: MAX_RUST_SOURCE_OWNERSHIP_MODULES,
                        }
                    })?;
                }
                use std::collections::hash_map::Entry;
                match owners_by_file.entry(file_id) {
                    Entry::Vacant(slot) => {
                        slot.insert(RustSourceOwners::Unique(owner));
                    }
                    Entry::Occupied(mut slot) => {
                        let previous = std::mem::replace(
                            slot.get_mut(),
                            RustSourceOwners::Ambiguous(Vec::new()),
                        );
                        let owners = match previous {
                            RustSourceOwners::Unique(previous) => {
                                let mut owners = Vec::new();
                                owners.try_reserve_exact(2).map_err(|_| {
                                    RustAuthorityError::SourceOwnershipIndexLimit {
                                        maximum: MAX_RUST_SOURCE_OWNERSHIP_MODULES,
                                    }
                                })?;
                                owners.push(previous);
                                owners.push(owner);
                                owners
                            }
                            RustSourceOwners::Ambiguous(mut owners) => {
                                owners.try_reserve(1).map_err(|_| {
                                    RustAuthorityError::SourceOwnershipIndexLimit {
                                        maximum: MAX_RUST_SOURCE_OWNERSHIP_MODULES,
                                    }
                                })?;
                                owners.push(owner);
                                owners
                            }
                        };
                        *slot.get_mut() = RustSourceOwners::Ambiguous(owners);
                    }
                }
            }
        }
        let omitted_package_crates =
            package_crate_count.saturating_sub(package_relative_roots.len());
        control.check()?;
        Ok(RustSourceOwnershipIndex {
            owners_by_file,
            active_hir_roots: RustActiveHirRootInventory {
                package_crate_count,
                package_relative_roots: package_relative_roots.into_boxed_slice(),
                omitted_package_crates,
            },
        })
    }

    fn prepare_source_ownership_index(
        &mut self,
        control: RustAnalysisControl<'_>,
    ) -> Result<(), RustAuthorityError> {
        self.source_ownership_index = Some(self.build_source_ownership_index(control)?);
        Ok(())
    }

    fn source_owner_entry(
        &self,
        file_id: FileId,
        path: &Path,
        control: RustAnalysisControl<'_>,
    ) -> Result<RustSourceOwnershipEntry, RustAuthorityError> {
        control.check()?;
        let index = self
            .source_ownership_index
            .as_ref()
            .ok_or(RustAuthorityError::SourceOwnershipIndexUnavailable)?;
        let Some(owners) = index.owners_by_file.get(&file_id) else {
            return Err(RustAuthorityError::DetachedSource {
                path: path.to_path_buf(),
                active_hir_roots: index.active_hir_roots.clone(),
            });
        };
        match owners {
            RustSourceOwners::Unique(owner) => Ok(*owner),
            RustSourceOwners::Ambiguous(owners) => {
                let package_root = AbsPathBuf::assert_utf8(self.root.clone());
                let mut owner_root_sample = Vec::with_capacity(MAX_DETACHED_HIR_ROOT_SAMPLE);
                for owner in owners {
                    control.check()?;
                    if owner_root_sample.len() >= MAX_DETACHED_HIR_ROOT_SAMPLE {
                        break;
                    }
                    let root_file = owner.krate.root_file(&self.database);
                    let Some(root_path) = self.vfs.file_path(root_file).as_path() else {
                        continue;
                    };
                    if let Some(relative) = root_path.strip_prefix(package_root.as_path()) {
                        owner_root_sample.push(PathBuf::from(relative.as_str()));
                    }
                }
                let omitted_owner_definitions =
                    owners.len().saturating_sub(owner_root_sample.len());
                Err(RustAuthorityError::AmbiguousSourceOwner {
                    path: path.to_path_buf(),
                    definition_count: owners.len(),
                    owner_root_sample: owner_root_sample.into_boxed_slice(),
                    omitted_owner_definitions,
                })
            }
        }
    }
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

/// Borrow-scoped observer for the exact selected editor buffers installed in
/// rust-analyzer's database for one workspace operation.
///
/// This is deliberately narrower than a compiler read observer: it does not
/// report disk-loaded siblings, negative path lookups, directory listings,
/// project-model reads, environment values, or child-process activity.
pub trait RustWorkspaceEditorBufferObserver {
    /// Receives one selected path and the exact UTF-8 bytes now visible through
    /// `SourceDatabase::file_text` after the overlay has been applied.
    fn observe_editor_buffer(&mut self, relative_path: &Path, contents: &[u8]);
}

/// Borrow-scoped observer for selected buffers plus concrete compiler event
/// surfaces: loaded RA VFS file contents, Rustdoc filesystem attempts and
/// successful input reads, and rejected `mod` candidates from RA's
/// name-resolution `DefMap` diagnostics.
///
/// This is diagnostic evidence only. The VFS scan does not see failed VFS
/// loader probes, and unresolved-module DefMap diagnostics do not cover arbitrary
/// filesystem calls, Cargo/project-model reads, environment values, child
/// processes, sysroot discovery, or generated outputs. It cannot authorize
/// workspace reuse.
pub trait RustWorkspaceReadFrontierObserver {
    /// Receives one selected editor buffer after RA has applied the overlay.
    fn observe_editor_buffer(&mut self, relative_path: &Path, contents: &[u8]);

    /// Receives the exact source text held by RA for one loaded VFS file.
    /// Return `true` only after the event has been accepted by the sink.
    fn observe_ra_vfs_file(&mut self, absolute_path: &str, contents: &[u8]) -> bool;

    /// Receives one exact byte buffer successfully read by the Rustdoc
    /// `include_str!` preloader before it is admitted into RA's VFS.
    fn observe_rustdoc_input(&mut self, absolute_path: &str, contents: &[u8]) -> bool;

    /// Receives one attempt made by the Rustdoc include preloader at its actual
    /// filesystem call site. `resolved_path` is populated only after
    /// successful canonicalization. Return `true` only after accepting it;
    /// observers that do not implement this callback reject it by default.
    fn observe_authority_filesystem_attempt(
        &mut self,
        requested_path: &str,
        operation: RustWorkspaceFilesystemOperation,
        outcome: RustWorkspaceFilesystemOutcome,
        resolved_path: Option<&str>,
    ) -> bool {
        false
    }

    /// Receives the crate root, declaration file, and candidate path string
    /// RA reports after module resolution rejected all candidates.
    /// Return `true` only after the event has been accepted by the sink.
    fn observe_unresolved_module_candidate(
        &mut self,
        crate_root_file: &str,
        declaring_file: &str,
        candidate: &str,
    ) -> bool;
}

/// Filesystem operation performed while resolving an active Rustdoc include.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RustWorkspaceFilesystemOperation {
    /// Resolve symlinks and missing components for the include target.
    Canonicalize,
    /// Inspect the canonical include target.
    Metadata,
    /// Open the canonical include target.
    Open,
    /// Read the bounded bytes from an opened include target.
    Read,
}

/// Outcome of one Rustdoc include filesystem operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RustWorkspaceFilesystemOutcome {
    /// The operation completed successfully, except metadata which carries its
    /// own typed result below.
    Succeeded,
    /// Metadata was read, including whether the path identifies a regular file.
    Metadata { length: u64, is_regular_file: bool },
    /// The bounded read succeeded with this exact byte count.
    Read { bytes_read: u64 },
    /// The operation failed with the operating system error category.
    Failed(std::io::ErrorKind),
}

/// Required read classes that remain outside the current Rust frontend observers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RustWorkspaceReadFrontierGap {
    /// The RA loader's failed requests and complete directory results are unknown.
    RaLoaderRequests,
    /// Successful module resolution and every individual resolver attempt are unknown.
    ModuleResolverAttempts,
    /// Filesystem activity outside the Rustdoc preloader remains unobserved.
    OtherAuthorityFilesystem,
    /// Complete directory enumeration results and errors are unknown.
    DirectoryEnumeration,
    /// Cargo and rust-analyzer project-model reads are unknown.
    CargoProjectModel,
    /// Present and absent environment values are unknown.
    Environment,
    /// Child-process identities, arguments, outputs, and terminal events are unknown.
    ProcessTree,
    /// Rustup, toolchain, sysroot, and standard-library reads are unknown.
    ToolchainSysroot,
    /// Build-script and generated output reads are unknown.
    GeneratedOutputs,
    /// Registry, path dependency, and other external source reads are unknown.
    ExternalDependencies,
    /// Read bytes are not yet proven members of the immutable remote assignment closure.
    AssignmentClosure,
    /// Observation was truncated at a fixed event or byte limit.
    ObservationTruncated,
    /// An observed path could not be represented in the observer's path format.
    UnsupportedPath,
    /// The observer rejected or failed to accept one or more visited events.
    EventNotAcknowledged,
}

/// Compact typed set of blockers in a Rust read-frontier seal report.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RustWorkspaceReadFrontierGaps(u16);

impl RustWorkspaceReadFrontierGaps {
    const fn insert(&mut self, gap: RustWorkspaceReadFrontierGap) {
        self.0 |= 1 << gap as u8;
    }

    /// Returns whether a required class prevents the frontier from sealing.
    #[must_use]
    pub const fn contains(self, gap: RustWorkspaceReadFrontierGap) -> bool {
        self.0 & (1 << gap as u8) != 0
    }

    const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Opaque token created only after every required read class has evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RustWorkspaceReadFrontierComplete {
    _sealed: (),
}

/// Typed result of checking whether one frontend observation can seal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RustWorkspaceReadFrontierSealReport {
    /// The listed classes still block a complete read frontier.
    Incomplete {
        /// Required classes with unknown or unacknowledged evidence.
        gaps: RustWorkspaceReadFrontierGaps,
    },
    /// Opaque proof token, constructible only by the private seal checker.
    Complete(RustWorkspaceReadFrontierComplete),
}

impl RustWorkspaceReadFrontierSealReport {
    /// Returns whether the complete-read token was produced.
    #[must_use]
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete(_))
    }

    /// Returns whether the report names this incomplete read class.
    #[must_use]
    pub const fn contains_gap(self, gap: RustWorkspaceReadFrontierGap) -> bool {
        match self {
            Self::Incomplete { gaps } => gaps.contains(gap),
            Self::Complete(_) => false,
        }
    }
}

/// Independent source-side totals from one opt-in RA read-frontier scan.
///
/// `visited` counts are advanced from the RA iterators/diagnostic payloads;
/// `delivered` counts advance only when the observer acknowledges an event.
/// A mismatch detects an omitted or rejected callback but does not establish
/// global compiler-read completeness.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RustWorkspaceReadFrontierSummary {
    /// RA VFS entries with a representable filesystem path encountered.
    pub vfs_files_visited: u64,
    /// RA VFS callbacks acknowledged by the observer.
    pub vfs_events_delivered: u64,
    /// Unique crate/declaration/candidate identities in RA name-resolution diagnostics.
    pub module_candidates_visited: u64,
    /// Module-candidate callbacks acknowledged by the observer.
    pub module_candidate_events_delivered: u64,
    /// Name-resolution diagnostics examined, including non-module and empty results.
    pub module_diagnostics_visited: u64,
    /// Successful Rustdoc include-file reads encountered by the preloader.
    pub rustdoc_inputs_visited: u64,
    /// Rustdoc include-file callbacks acknowledged by the observer.
    pub rustdoc_input_events_delivered: u64,
    /// Filesystem attempts at the Rustdoc include boundary.
    pub filesystem_attempts_visited: u64,
    /// Filesystem-attempt callbacks acknowledged by the observer.
    pub filesystem_attempt_events_delivered: u64,
    /// All Rustdoc filesystem producer events, including exact successful bytes.
    pub authority_filesystem_events_visited: u64,
    /// All Rustdoc filesystem callbacks acknowledged by the observer.
    pub authority_filesystem_events_delivered: u64,
    /// VFS, Rustdoc, or diagnostic paths that could not be represented as UTF-8.
    pub unsupported_paths: u64,
    /// The scan stopped at a fixed event/byte budget.
    pub truncated: bool,
}

impl RustWorkspaceReadFrontierSummary {
    /// Checks the observed event totals and the fixed set of still-required
    /// compiler channels. This frontend currently always returns `Incomplete`
    /// because Cargo/project-model, environment, process, toolchain, loader,
    /// and assignment-closure observations are not available here.
    #[must_use]
    pub fn seal_report(&self) -> RustWorkspaceReadFrontierSealReport {
        let mut gaps = RustWorkspaceReadFrontierGaps::default();
        if self.truncated {
            gaps.insert(RustWorkspaceReadFrontierGap::ObservationTruncated);
        }
        if self.unsupported_paths > 0 {
            gaps.insert(RustWorkspaceReadFrontierGap::UnsupportedPath);
        }
        if self.vfs_files_visited != self.vfs_events_delivered
            || self.module_candidates_visited != self.module_candidate_events_delivered
            || self.rustdoc_inputs_visited != self.rustdoc_input_events_delivered
            || self.filesystem_attempts_visited != self.filesystem_attempt_events_delivered
            || self.authority_filesystem_events_visited
                != self.authority_filesystem_events_delivered
        {
            gaps.insert(RustWorkspaceReadFrontierGap::EventNotAcknowledged);
        }

        // These are required channel gaps, not an estimate inferred from the
        // observed subset. No caller can turn the diagnostic observers above
        // into Complete while any of these classes remains unsupported.
        for gap in [
            RustWorkspaceReadFrontierGap::RaLoaderRequests,
            RustWorkspaceReadFrontierGap::ModuleResolverAttempts,
            RustWorkspaceReadFrontierGap::OtherAuthorityFilesystem,
            RustWorkspaceReadFrontierGap::DirectoryEnumeration,
            RustWorkspaceReadFrontierGap::CargoProjectModel,
            RustWorkspaceReadFrontierGap::Environment,
            RustWorkspaceReadFrontierGap::ProcessTree,
            RustWorkspaceReadFrontierGap::ToolchainSysroot,
            RustWorkspaceReadFrontierGap::GeneratedOutputs,
            RustWorkspaceReadFrontierGap::ExternalDependencies,
            RustWorkspaceReadFrontierGap::AssignmentClosure,
        ] {
            gaps.insert(gap);
        }

        if gaps.is_empty() {
            RustWorkspaceReadFrontierSealReport::Complete(RustWorkspaceReadFrontierComplete {
                _sealed: (),
            })
        } else {
            RustWorkspaceReadFrontierSealReport::Incomplete { gaps }
        }
    }
}

const MAX_RUST_READ_FRONTIER_EVENTS: u64 = 250_000;
const MAX_RUST_READ_FRONTIER_DIAGNOSTICS: u64 = 250_000;
const MAX_RUST_READ_FRONTIER_BYTES: u64 = 512 * 1024 * 1024;

enum RustWorkspaceSessionObserver<'a> {
    Editor(&'a mut dyn RustWorkspaceEditorBufferObserver),
    ReadFrontier(&'a mut dyn RustWorkspaceReadFrontierObserver),
}

impl RustWorkspaceSessionObserver<'_> {
    fn observe_editor_buffer(&mut self, relative_path: &Path, contents: &[u8]) {
        match self {
            Self::Editor(observer) => {
                observer.observe_editor_buffer(relative_path, contents);
            }
            Self::ReadFrontier(observer) => {
                observer.observe_editor_buffer(relative_path, contents);
            }
        }
    }

    fn read_frontier(&mut self) -> Option<&mut dyn RustWorkspaceReadFrontierObserver> {
        match self {
            Self::Editor(_) => None,
            Self::ReadFrontier(frontier) => Some(&mut **frontier),
        }
    }

    fn observe_rustdoc_input(
        &mut self,
        summary: &mut RustWorkspaceReadFrontierSummary,
        path: &Path,
        contents: &[u8],
    ) {
        let Self::ReadFrontier(observer) = self else {
            return;
        };
        summary.rustdoc_inputs_visited = summary.rustdoc_inputs_visited.saturating_add(1);
        if !advance_authority_filesystem_event(summary) {
            return;
        }
        let Some(absolute_path) = path.to_str() else {
            summary.unsupported_paths = summary.unsupported_paths.saturating_add(1);
            return;
        };
        if observer.observe_rustdoc_input(absolute_path, contents) {
            summary.rustdoc_input_events_delivered =
                summary.rustdoc_input_events_delivered.saturating_add(1);
            summary.authority_filesystem_events_delivered = summary
                .authority_filesystem_events_delivered
                .saturating_add(1);
        }
    }

    fn observe_authority_filesystem_attempt(
        &mut self,
        summary: &mut RustWorkspaceReadFrontierSummary,
        requested_path: &Path,
        operation: RustWorkspaceFilesystemOperation,
        outcome: RustWorkspaceFilesystemOutcome,
        resolved_path: Option<&Path>,
    ) {
        let Self::ReadFrontier(observer) = self else {
            return;
        };
        summary.filesystem_attempts_visited = summary.filesystem_attempts_visited.saturating_add(1);
        if !advance_authority_filesystem_event(summary) {
            return;
        }
        let Some(requested_path) = requested_path.to_str() else {
            summary.unsupported_paths = summary.unsupported_paths.saturating_add(1);
            return;
        };
        let resolved_path = match resolved_path {
            Some(path) => match path.to_str() {
                Some(path) => Some(path),
                None => {
                    summary.unsupported_paths = summary.unsupported_paths.saturating_add(1);
                    return;
                }
            },
            None => None,
        };
        if observer.observe_authority_filesystem_attempt(
            requested_path,
            operation,
            outcome,
            resolved_path,
        ) {
            summary.filesystem_attempt_events_delivered = summary
                .filesystem_attempt_events_delivered
                .saturating_add(1);
            summary.authority_filesystem_events_delivered = summary
                .authority_filesystem_events_delivered
                .saturating_add(1);
        }
    }
}

fn advance_authority_filesystem_event(summary: &mut RustWorkspaceReadFrontierSummary) -> bool {
    summary.authority_filesystem_events_visited = summary
        .authority_filesystem_events_visited
        .saturating_add(1);
    if summary.authority_filesystem_events_visited > MAX_RUST_READ_FRONTIER_EVENTS {
        summary.truncated = true;
        false
    } else {
        true
    }
}

/// Exact compiler-selected source buffer whose documentation attributes may read files.
#[derive(Clone, Copy)]
struct DocumentationSource<'source> {
    path: &'source Path,
    source: &'source [u8],
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
    metadata_policy: RustCargoMetadataPolicy,
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
        metadata_policy: RustCargoMetadataPolicy,
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
            metadata_policy,
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
    /// Source paths newly inserted into a local SourceRoot, including VFS-visible unrooted files.
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
        self.begin_inner(key, files, control, None)
            .map(|(lease, _)| lease)
    }

    /// Opens a fresh workspace and reports exact selected editor buffers to a
    /// caller-owned observer while the transaction remains single-owner.
    ///
    /// The callback sees only the selected overlay paths after RA has applied
    /// them. It is diagnostic evidence, not a complete filesystem frontier,
    /// and cannot authorize workspace reuse.
    pub fn begin_with_editor_buffer_observer<'cache>(
        &'cache mut self,
        key: RustWorkspaceSessionKey,
        files: &[RustWorkspaceFile<'_>],
        control: RustAnalysisControl<'_>,
        observer: &mut dyn RustWorkspaceEditorBufferObserver,
    ) -> Result<RustWorkspaceSessionLease<'cache>, RustAuthorityError> {
        self.begin_inner(
            key,
            files,
            control,
            Some(RustWorkspaceSessionObserver::Editor(observer)),
        )
        .map(|(lease, _)| lease)
    }

    /// Opens a fresh workspace and synchronously reports selected buffers,
    /// loaded RA VFS files, and unresolved module candidates from RA's DefMap.
    ///
    /// The returned source-side totals let a diagnostic consumer detect
    /// omitted or rejected callbacks. They do not certify complete compiler
    /// reads and cannot authorize workspace reuse.
    pub fn begin_with_read_frontier_observer<'cache>(
        &'cache mut self,
        key: RustWorkspaceSessionKey,
        files: &[RustWorkspaceFile<'_>],
        control: RustAnalysisControl<'_>,
        frontier_observer: &mut dyn RustWorkspaceReadFrontierObserver,
    ) -> Result<
        (
            RustWorkspaceSessionLease<'cache>,
            RustWorkspaceReadFrontierSummary,
        ),
        RustAuthorityError,
    > {
        self.begin_inner(
            key,
            files,
            control,
            Some(RustWorkspaceSessionObserver::ReadFrontier(
                frontier_observer,
            )),
        )
    }

    fn begin_inner<'cache>(
        &'cache mut self,
        key: RustWorkspaceSessionKey,
        files: &[RustWorkspaceFile<'_>],
        control: RustAnalysisControl<'_>,
        mut observer: Option<RustWorkspaceSessionObserver<'_>>,
    ) -> Result<
        (
            RustWorkspaceSessionLease<'cache>,
            RustWorkspaceReadFrontierSummary,
        ),
        RustAuthorityError,
    > {
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
        let workspace = RustWorkspace::open_with_features_and_metadata_policy_unindexed(
            &key.root,
            &key.toolchain,
            key.edition,
            features,
            key.metadata_policy,
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
        let mut read_frontier_summary = RustWorkspaceReadFrontierSummary::default();
        let result = workspace.apply_selected_sources(
            files,
            &key,
            control,
            observer.as_mut(),
            &mut read_frontier_summary,
        );
        self.stats.source_update_nanos = self
            .stats
            .source_update_nanos
            .saturating_add(elapsed_nanos(update_started));
        let (updated, unchanged, added, root_entries_rebuilt, roots_touched) =
            result.map_err(|error| {
                self.stats.failed_transactions = self.stats.failed_transactions.saturating_add(1);
                error
            })?;
        if let Some(observer) = observer.as_mut() {
            if let Some(frontier) = observer.read_frontier() {
                workspace.observe_read_frontier(frontier, control, &mut read_frontier_summary);
            }
        }
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
        Ok((
            RustWorkspaceSessionLease {
                lane: self,
                session: Some(RustWorkspaceSession { workspace }),
                committed: false,
            },
            read_frontier_summary,
        ))
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
        let mut workspace = RustWorkspace::open_with_features(
            &self.root,
            &self.toolchain,
            self.edition,
            features,
            control,
        )?;
        let mut read_frontier_summary = RustWorkspaceReadFrontierSummary::default();
        workspace.preload_documentation_inputs(
            &[DocumentationSource {
                path: &self.source_path,
                source: &source,
            }],
            control,
            None,
            &mut read_frontier_summary,
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
        Self::open_with_features_and_metadata_policy(
            root,
            toolchain,
            edition,
            features,
            RustCargoMetadataPolicy::Offline,
            control,
        )
    }

    /// Loads the Cargo graph under an explicit registry metadata policy.
    ///
    /// Online permits Cargo metadata to resolve missing registry state; Offline forbids network
    /// access and fails closed when only `--no-deps` metadata is available.
    pub fn open_with_features_and_metadata_policy(
        root: impl AsRef<Path>,
        toolchain: &RustToolchain,
        edition: RustEdition,
        features: RustFeatureControl<'_>,
        metadata_policy: RustCargoMetadataPolicy,
        control: RustAnalysisControl<'_>,
    ) -> Result<Self, RustAuthorityError> {
        let mut workspace = Self::open_with_features_and_metadata_policy_unindexed(
            root,
            toolchain,
            edition,
            features,
            metadata_policy,
            control,
        )?;
        workspace.prepare_source_ownership_index(control)?;
        Ok(workspace)
    }

    fn open_with_features_and_metadata_policy_unindexed(
        root: impl AsRef<Path>,
        toolchain: &RustToolchain,
        edition: RustEdition,
        features: RustFeatureControl<'_>,
        metadata_policy: RustCargoMetadataPolicy,
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
        let metadata_gate = cargo_metadata_preflight(
            &root,
            cargo,
            cargo_home,
            toolchain,
            features,
            metadata_policy,
            control,
        )?;
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
        .chain(metadata_gate.extra_env.iter().cloned())
        .collect();
        let config = CargoConfig {
            sysroot: Some(RustLibSource::Path(AbsPathBuf::assert_utf8(
                toolchain.sysroot.clone(),
            ))),
            no_deps: false,
            // The preflight resolved this same manifest, feature set, and network policy.
            // `--locked` also keeps the analyzer's second Cargo invocation from mutating the
            // user's lockfile between proof and graph construction.
            metadata_extra_args: {
                let mut args = match metadata_policy {
                    RustCargoMetadataPolicy::Online => {
                        vec!["--config".to_owned(), "net.offline=false".to_owned()]
                    }
                    RustCargoMetadataPolicy::Offline => vec!["--offline".to_owned()],
                };
                if metadata_gate.lockfile_exists {
                    args.push("--locked".to_owned());
                }
                args.extend(metadata_gate.metadata_extra_args.iter().cloned());
                args
            },
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
        let metadata_failed = Arc::new(AtomicBool::new(false));
        let metadata_failed_callback = Arc::clone(&metadata_failed);
        let (database, vfs, _proc_macros) =
            ra_ap_load_cargo::load_workspace_at(&root, &config, &load, &|message| {
                if message.starts_with("cargo metadata: failed") {
                    metadata_failed_callback.store(true, Ordering::Release);
                }
            })
            .map_err(|source| RustAuthorityError::Workspace {
                root: root.clone(),
                source,
            })?;
        if metadata_failed.load(Ordering::Acquire) {
            return Err(RustAuthorityError::CargoMetadataIncomplete {
                root: root.clone(),
                policy: metadata_policy,
                cause: CargoMetadataIncompleteCause::AnalyzerUsedNoDependenciesFallback,
            });
        }
        control.check()?;
        Ok(Self {
            root,
            edition,
            database,
            vfs,
            selected_source_paths: HashMap::new(),
            source_ownership_index: None,
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
        // VFS presence alone does not establish active Cargo ownership. The
        // retained DefMap index was built from this exact loaded graph before
        // the workspace was shared, after any selected source overlays.
        let semantics = Semantics::new(&self.database);
        let owner = self.source_owner_entry(file_id, &source_path, control)?;
        let source_scope = owner.scope;
        let owner = owner.krate;
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

    /// Admits active Rustdoc `include_str!` inputs into the exact local source root that owns
    /// each selected source. Rust-analyzer's built-in macro expansion reads only its VFS, so the
    /// compiler must load non-Rust package files before asking `HasDocs` to expand attributes.
    fn preload_documentation_inputs(
        &mut self,
        sources: &[DocumentationSource<'_>],
        control: RustAnalysisControl<'_>,
        mut observer: Option<&mut RustWorkspaceSessionObserver<'_>>,
        read_frontier_summary: &mut RustWorkspaceReadFrontierSummary,
    ) -> Result<(), RustAuthorityError> {
        let local_roots = LocalRoots::get(&self.database)
            .roots(&self.database)
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        let mut inputs = HashMap::<PathBuf, (SourceRootId, String)>::new();
        let maximum_file_bytes = u64::from(*control.maximum_source_bytes);
        let mut total_bytes = 0_usize;

        for selected in sources {
            control.check()?;
            if !selected.path.starts_with(&self.root) {
                return Err(RustAuthorityError::SourceOutsidePackage {
                    root: self.root.clone(),
                    path: selected.path.to_path_buf(),
                });
            }
            let Some(path_text) = selected.path.to_str() else {
                return Err(RustAuthorityError::SessionSourcePath {
                    path: selected.path.to_path_buf(),
                });
            };
            let selected_vfs_path =
                VfsPath::from(AbsPathBuf::assert_utf8(PathBuf::from(path_text)));
            let Some((selected_file_id, FileExcluded::No)) = self.vfs.file_id(&selected_vfs_path)
            else {
                return Err(RustAuthorityError::SourceNotLoaded {
                    path: selected.path.to_path_buf(),
                });
            };
            let selected_root = self
                .database
                .file_source_root(selected_file_id)
                .source_root_id(&self.database);
            if !local_roots.contains(&selected_root) {
                return Err(RustAuthorityError::SessionSourceRootAmbiguous);
            }
            let observed = SourceDatabase::file_text(&self.database, selected_file_id);
            let observed_text = observed.text(&self.database);
            if observed_text.as_bytes() != selected.source {
                return Err(RustAuthorityError::SourceBinding {
                    expected: selected.source.len(),
                    observed: observed_text.len(),
                });
            }

            let owner = match self.source_owner_entry(selected_file_id, selected.path, control) {
                Ok(owner) => owner.krate,
                // The selected compiler frontier can contain cfg-inactive Rust
                // files. They still receive exact VFS/source binding above, but
                // only active Cargo-owned files have a crate cfg context from
                // which Rustdoc include attributes can be expanded. Skipping
                // this preload does not grant source ownership: analyze_source
                // continues to reject a detached file.
                Err(RustAuthorityError::DetachedSource { .. }) => continue,
                Err(error) => return Err(error),
            };
            let semantics = Semantics::new(&self.database);
            let edition = owner.edition(&self.database);
            let source_file = EditionedFileId::new(&self.database, selected_file_id, edition);
            let parsed = semantics.parse(source_file);
            let include_paths = documentation_include_paths(
                parsed.syntax(),
                owner.cfg(&self.database),
                selected.path,
            )?;

            for include_path in include_paths {
                control.check()?;
                let requested = selected
                    .path
                    .parent()
                    .unwrap_or(&self.root)
                    .join(include_path);
                let canonical = match fs::canonicalize(&requested) {
                    Ok(canonical) => {
                        if let Some(observer) = observer.as_deref_mut() {
                            observer.observe_authority_filesystem_attempt(
                                read_frontier_summary,
                                &requested,
                                RustWorkspaceFilesystemOperation::Canonicalize,
                                RustWorkspaceFilesystemOutcome::Succeeded,
                                Some(&canonical),
                            );
                        }
                        canonical
                    }
                    Err(source) => {
                        if let Some(observer) = observer.as_deref_mut() {
                            observer.observe_authority_filesystem_attempt(
                                read_frontier_summary,
                                &requested,
                                RustWorkspaceFilesystemOperation::Canonicalize,
                                RustWorkspaceFilesystemOutcome::Failed(source.kind()),
                                None,
                            );
                        }
                        return Err(if source.kind() == std::io::ErrorKind::NotFound {
                            RustAuthorityError::DocumentationInputMissing {
                                path: requested.clone(),
                            }
                        } else {
                            RustAuthorityError::DocumentationInputRead {
                                path: requested.clone(),
                                source,
                            }
                        });
                    }
                };
                if !canonical.starts_with(&self.root) {
                    return Err(RustAuthorityError::SourceOutsidePackage {
                        root: self.root.clone(),
                        path: canonical,
                    });
                }
                let metadata = match fs::metadata(&canonical) {
                    Ok(metadata) => {
                        if let Some(observer) = observer.as_deref_mut() {
                            observer.observe_authority_filesystem_attempt(
                                read_frontier_summary,
                                &canonical,
                                RustWorkspaceFilesystemOperation::Metadata,
                                RustWorkspaceFilesystemOutcome::Metadata {
                                    length: metadata.len(),
                                    is_regular_file: metadata.is_file(),
                                },
                                None,
                            );
                        }
                        metadata
                    }
                    Err(source) => {
                        if let Some(observer) = observer.as_deref_mut() {
                            observer.observe_authority_filesystem_attempt(
                                read_frontier_summary,
                                &canonical,
                                RustWorkspaceFilesystemOperation::Metadata,
                                RustWorkspaceFilesystemOutcome::Failed(source.kind()),
                                None,
                            );
                        }
                        return Err(RustAuthorityError::DocumentationInputRead {
                            path: canonical.clone(),
                            source,
                        });
                    }
                };
                if !metadata.is_file() {
                    return Err(RustAuthorityError::DocumentationInputRead {
                        path: canonical,
                        source: std::io::Error::from(std::io::ErrorKind::InvalidInput),
                    });
                }
                if metadata.len() > maximum_file_bytes {
                    return Err(RustAuthorityError::DocumentationInputBudget {
                        path: canonical,
                        actual: metadata.len(),
                        maximum: control.maximum_source_bytes,
                    });
                }
                let mut bytes =
                    Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(usize::MAX));
                let file = match fs::File::open(&canonical) {
                    Ok(file) => {
                        if let Some(observer) = observer.as_deref_mut() {
                            observer.observe_authority_filesystem_attempt(
                                read_frontier_summary,
                                &canonical,
                                RustWorkspaceFilesystemOperation::Open,
                                RustWorkspaceFilesystemOutcome::Succeeded,
                                None,
                            );
                        }
                        file
                    }
                    Err(source) => {
                        if let Some(observer) = observer.as_deref_mut() {
                            observer.observe_authority_filesystem_attempt(
                                read_frontier_summary,
                                &canonical,
                                RustWorkspaceFilesystemOperation::Open,
                                RustWorkspaceFilesystemOutcome::Failed(source.kind()),
                                None,
                            );
                        }
                        return Err(RustAuthorityError::DocumentationInputRead {
                            path: canonical.clone(),
                            source,
                        });
                    }
                };
                let bytes_read = match file
                    .take(maximum_file_bytes.saturating_add(1))
                    .read_to_end(&mut bytes)
                {
                    Ok(bytes_read) => {
                        if let Some(observer) = observer.as_deref_mut() {
                            observer.observe_authority_filesystem_attempt(
                                read_frontier_summary,
                                &canonical,
                                RustWorkspaceFilesystemOperation::Read,
                                RustWorkspaceFilesystemOutcome::Read {
                                    bytes_read: u64::try_from(bytes_read).unwrap_or(u64::MAX),
                                },
                                None,
                            );
                        }
                        bytes_read
                    }
                    Err(source) => {
                        if let Some(observer) = observer.as_deref_mut() {
                            observer.observe_authority_filesystem_attempt(
                                read_frontier_summary,
                                &canonical,
                                RustWorkspaceFilesystemOperation::Read,
                                RustWorkspaceFilesystemOutcome::Failed(source.kind()),
                                None,
                            );
                        }
                        return Err(RustAuthorityError::DocumentationInputRead {
                            path: canonical.clone(),
                            source,
                        });
                    }
                };
                debug_assert_eq!(bytes_read, bytes.len());
                if let Some(observer) = observer.as_deref_mut() {
                    observer.observe_rustdoc_input(read_frontier_summary, &canonical, &bytes);
                }
                if bytes.len() as u64 > maximum_file_bytes {
                    return Err(RustAuthorityError::DocumentationInputBudget {
                        path: canonical,
                        actual: bytes.len() as u64,
                        maximum: control.maximum_source_bytes,
                    });
                }
                let text = String::from_utf8(bytes).map_err(|_| {
                    RustAuthorityError::DocumentationInputUtf8 {
                        path: canonical.clone(),
                    }
                })?;
                total_bytes = total_bytes.checked_add(text.len()).ok_or(
                    RustAuthorityError::DocumentationInputLimit {
                        actual: usize::MAX,
                        maximum: MAX_RUST_DOCUMENTATION_INPUT_BYTES,
                    },
                )?;
                if total_bytes > MAX_RUST_DOCUMENTATION_INPUT_BYTES {
                    return Err(RustAuthorityError::DocumentationInputLimit {
                        actual: total_bytes,
                        maximum: MAX_RUST_DOCUMENTATION_INPUT_BYTES,
                    });
                }
                if inputs.len() >= MAX_RUST_DOCUMENTATION_INPUTS && !inputs.contains_key(&canonical)
                {
                    return Err(RustAuthorityError::DocumentationInputLimit {
                        actual: inputs.len().saturating_add(1),
                        maximum: MAX_RUST_DOCUMENTATION_INPUTS,
                    });
                }
                match inputs.entry(canonical) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert((selected_root, text));
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if entry.get().0 != selected_root || entry.get().1 != text {
                            return Err(RustAuthorityError::SessionSourceRootAmbiguous);
                        }
                    }
                }
            }
        }

        if inputs.is_empty() {
            return Ok(());
        }

        let local_root_ids = LocalRoots::get(&self.database)
            .roots(&self.database)
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let mut membership = HashMap::<FileId, SourceRootId>::new();
        for root_id in &local_root_ids {
            let root = self
                .database
                .source_root(*root_id)
                .source_root(&self.database);
            for file_id in root.iter() {
                if membership.insert(file_id, *root_id).is_some() {
                    return Err(RustAuthorityError::SessionSourceRootAmbiguous);
                }
            }
        }

        let mut change = ChangeWithProcMacros::default();
        let mut added = Vec::<(SourceRootId, FileId, VfsPath)>::new();
        for (path, (expected_root, text)) in inputs {
            control.check()?;
            let Some(path_text) = path.to_str() else {
                return Err(RustAuthorityError::SessionSourcePath { path });
            };
            let vfs_path = VfsPath::from(AbsPathBuf::assert_utf8(PathBuf::from(path_text)));
            let (file_id, excluded) = if let Some(existing) = self.vfs.file_id(&vfs_path) {
                existing
            } else {
                self.vfs
                    .set_file_contents(vfs_path.clone(), Some(text.as_bytes().to_vec()));
                self.vfs.file_id(&vfs_path).ok_or_else(|| {
                    RustAuthorityError::DocumentationInputRead {
                        path: path.clone(),
                        source: std::io::Error::from(std::io::ErrorKind::NotFound),
                    }
                })?
            };
            if let Some(observed_root) = membership.get(&file_id).copied() {
                if observed_root != expected_root {
                    return Err(RustAuthorityError::SessionSourceRootAmbiguous);
                }
                if excluded == FileExcluded::Yes
                    || SourceDatabase::file_text(&self.database, file_id)
                        .text(&self.database)
                        .as_ref()
                        != text
                {
                    change.change_file(file_id, Some(text));
                }
            } else {
                if !local_root_ids.contains(&expected_root) {
                    return Err(RustAuthorityError::SessionSourceRootAmbiguous);
                }
                change.change_file(file_id, Some(text));
                added.push((expected_root, file_id, vfs_path));
                membership.insert(file_id, expected_root);
            }
        }

        if !added.is_empty() {
            let library_roots = LibraryRoots::get(&self.database).roots(&self.database);
            let all_roots = local_root_ids
                .iter()
                .chain(library_roots.iter())
                .copied()
                .collect::<HashSet<_>>();
            let Some(max_root_id) = all_roots.iter().map(|id| id.0).max() else {
                return Err(RustAuthorityError::SessionSourceRootAmbiguous);
            };
            if all_roots.len() != max_root_id as usize + 1 {
                return Err(RustAuthorityError::SessionSourceRootAmbiguous);
            }
            let mut roots = Vec::with_capacity(max_root_id as usize + 1);
            for raw_id in 0..=max_root_id {
                control.check()?;
                let root_id = SourceRootId(raw_id);
                let old_root = self
                    .database
                    .source_root(root_id)
                    .source_root(&self.database);
                let mut file_set = FileSet::default();
                for file_id in old_root.iter() {
                    let path = old_root
                        .path_for_file(&file_id)
                        .ok_or(RustAuthorityError::SessionSourceRootAmbiguous)?;
                    file_set.insert(file_id, path.clone());
                }
                for (added_root, file_id, path) in &added {
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
        self.database.apply_change(change);
        Ok(())
    }

    fn apply_selected_sources(
        &mut self,
        files: &[RustWorkspaceFile<'_>],
        key: &RustWorkspaceSessionKey,
        control: RustAnalysisControl<'_>,
        mut observer: Option<&mut RustWorkspaceSessionObserver<'_>>,
        read_frontier_summary: &mut RustWorkspaceReadFrontierSummary,
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
        let mut selected_file_ids = None;
        if observer.is_some() {
            let mut file_ids = Vec::new();
            if file_ids.try_reserve_exact(files.len()).is_ok() {
                selected_file_ids = Some(file_ids);
            }
        }
        let mut selected_source_roots = HashSet::new();
        let mut local_roots_by_directory = None;
        let mut rehomed_file_ids = HashSet::new();
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
                    let indexed_source_root = self
                        .database
                        .file_source_root(file_id)
                        .source_root_id(&self.database);
                    let indexed_path = self
                        .database
                        .source_root(indexed_source_root)
                        .source_root(&self.database)
                        .path_for_file(&file_id)
                        .cloned();
                    if indexed_path.is_some() && !local_roots.contains(&indexed_source_root) {
                        return Err(RustAuthorityError::SessionSourceRootAmbiguous);
                    }
                    if indexed_path.as_ref() == Some(&vfs_path) {
                        selected_source_roots.insert(indexed_source_root);
                        (file_id, false)
                    } else {
                        // VFS identity alone does not prove that the FileId is
                        // indexed at this exact lexical path in an active local
                        // SourceRoot. It may be visible but unrooted after a
                        // prior overlay, or the root FileSet may retain a
                        // different path for the same VFS identity. Re-home it
                        // before rebuilding DefMaps; otherwise an unconditional
                        // module can remain invisible to HIR or bind through a
                        // different path.
                        if local_roots_by_directory.is_none() {
                            local_roots_by_directory =
                                Some(self.local_source_roots_by_directory(&local_roots, control)?);
                        }
                        let destination_source_root = Self::source_root_for_new_file(
                            &self.root,
                            &requested_path,
                            local_roots_by_directory
                                .as_mut()
                                .expect("local source-root index was initialized"),
                        )?;
                        selected_source_roots.insert(destination_source_root);
                        added_ids.push((destination_source_root, file_id, vfs_path.clone()));
                        if let Some(parent) = requested_path.parent() {
                            local_roots_by_directory
                                .as_mut()
                                .expect("local source-root index was initialized")
                                .entry(parent.to_path_buf())
                                .or_default()
                                .insert(destination_source_root);
                        }
                        if indexed_path.is_some() {
                            // The old and destination roots are both touched:
                            // the FileId must leave its prior FileSet even
                            // when the lexical path now selects another root.
                            selected_source_roots.insert(indexed_source_root);
                            rehomed_file_ids.insert(file_id);
                        }
                        added = added.saturating_add(1);
                        (file_id, true)
                    }
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
            if let Some(selected_file_ids) = selected_file_ids.as_mut() {
                selected_file_ids.push(file_id);
            }
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
            // Rebuild the complete root inventory, including a rehomed
            // FileId's old root when it differs from its selected destination.
            // `selected_source_roots` is a touched-root count, not a filter on
            // this loop: stale membership must be removed from every old root.
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
                    if rehomed_file_ids.contains(&file_id) {
                        continue;
                    }
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
        // Selected editor buffers can change the active DefMap. Build the
        // complete ownership cache now, before this workspace is shared with
        // source analysis or documentation loading.
        self.prepare_source_ownership_index(control)?;
        let selected_source_paths = files
            .iter()
            .map(|file| self.root.join(file.relative_path))
            .collect::<Vec<_>>();
        let documentation_sources = files
            .iter()
            .zip(&selected_source_paths)
            .map(|(file, path)| DocumentationSource {
                // Read Rustdoc attributes from the exact selected VFS path.
                // Canonicalizing a symlink here can alias two selected buffers
                // to one physical path and bind one buffer to the other.
                path,
                source: file.source.as_bytes(),
            })
            .collect::<Vec<_>>();
        self.preload_documentation_inputs(
            &documentation_sources,
            control,
            observer.as_mut().map(|observer| &mut **observer),
            read_frontier_summary,
        )?;
        control.check()?;
        self.selected_source_paths = files
            .iter()
            .zip(selected_source_paths)
            .map(|(file, path)| (file.relative_path.to_path_buf(), path))
            .collect();
        if let (Some(observer), Some(selected_file_ids)) =
            (observer.as_mut(), selected_file_ids.as_ref())
        {
            for (file, file_id) in files.iter().zip(selected_file_ids) {
                let observed = SourceDatabase::file_text(&self.database, *file_id);
                let observed_text = observed.text(&self.database);
                observer.observe_editor_buffer(file.relative_path, observed_text.as_bytes());
            }
        }
        Ok((
            updated,
            unchanged,
            added,
            root_entries_rebuilt,
            selected_source_roots.len(),
        ))
    }

    fn observe_read_frontier(
        &self,
        observer: &mut dyn RustWorkspaceReadFrontierObserver,
        control: RustAnalysisControl<'_>,
        summary: &mut RustWorkspaceReadFrontierSummary,
    ) {
        ra_ap_hir_ty::next_solver::interner::attach_db(&self.database, || {
            let mut observed_bytes = 0_u64;
            let mut vfs_entries_visited = 0_u64;

            // This is a positive-file snapshot of RA's actual VFS, not an OS
            // loader interception. Every path-bearing entry is sent synchronously
            // without allocating an event row or retaining source bytes.
            for (file_id, vfs_path) in self.vfs.iter() {
                if control.check().is_err() || vfs_entries_visited >= MAX_RUST_READ_FRONTIER_EVENTS
                {
                    summary.truncated = true;
                    break;
                }
                vfs_entries_visited = vfs_entries_visited.saturating_add(1);
                let Some(path) = vfs_path.as_path() else {
                    summary.unsupported_paths = summary.unsupported_paths.saturating_add(1);
                    continue;
                };
                let observed = SourceDatabase::file_text(&self.database, file_id);
                let contents = observed.text(&self.database);
                let byte_charge = u64::try_from(contents.len()).unwrap_or(u64::MAX);
                if observed_bytes
                    .checked_add(byte_charge)
                    .is_none_or(|total| total > MAX_RUST_READ_FRONTIER_BYTES)
                {
                    summary.truncated = true;
                    break;
                }
                observed_bytes += byte_charge;
                summary.vfs_files_visited = summary.vfs_files_visited.saturating_add(1);
                if observer.observe_ra_vfs_file(path.as_str(), contents.as_bytes()) {
                    summary.vfs_events_delivered = summary.vfs_events_delivered.saturating_add(1);
                }
            }

            // DefMap contains diagnostics emitted by real name resolution.
            // Read only this narrow data rather than calling Module::diagnostics,
            // which also runs unrelated type and MIR diagnostics.
            let mut seen_candidates = HashSet::<[u8; 32]>::new();
            let mut crate_maps_visited = 0_u64;
            for crate_id in all_crates(&self.database).iter().copied() {
                if summary.truncated || control.check().is_err() {
                    summary.truncated = true;
                    break;
                }
                if crate_maps_visited >= MAX_RUST_READ_FRONTIER_EVENTS {
                    summary.truncated = true;
                    break;
                }
                crate_maps_visited = crate_maps_visited.saturating_add(1);
                let krate = ra_ap_hir::Crate::from(crate_id);
                let crate_root_file_id = krate.root_file(&self.database);
                let Some(crate_root_file) = self.vfs.file_path(crate_root_file_id).as_path() else {
                    summary.unsupported_paths = summary.unsupported_paths.saturating_add(1);
                    continue;
                };
                let crate_root_file = crate_root_file.as_str();
                let def_map = crate_def_map(&self.database, crate_id);
                for diagnostic in def_map.diagnostics() {
                    if summary.truncated || control.check().is_err() {
                        summary.truncated = true;
                        break;
                    }
                    if !advance_bounded_counter(
                        &mut summary.module_diagnostics_visited,
                        MAX_RUST_READ_FRONTIER_DIAGNOSTICS,
                    ) {
                        summary.truncated = true;
                        break;
                    }
                    let DefDiagnosticKind::UnresolvedModule { ast, candidates } = &diagnostic.kind
                    else {
                        continue;
                    };
                    let file_id = ast
                        .file_id
                        .original_file_respecting_includes(&self.database)
                        .file_id(&self.database);
                    let Some(declaring_file) = self.vfs.file_path(file_id).as_path() else {
                        summary.unsupported_paths = summary.unsupported_paths.saturating_add(1);
                        continue;
                    };
                    for candidate in candidates.iter() {
                        if summary.module_candidates_visited >= MAX_RUST_READ_FRONTIER_EVENTS {
                            summary.truncated = true;
                            break;
                        }
                        let byte_charge = u64::try_from(
                            crate_root_file
                                .len()
                                .saturating_add(declaring_file.as_str().len())
                                .saturating_add(candidate.len()),
                        )
                        .unwrap_or(u64::MAX);
                        if observed_bytes
                            .checked_add(byte_charge)
                            .is_none_or(|total| total > MAX_RUST_READ_FRONTIER_BYTES)
                        {
                            summary.truncated = true;
                            break;
                        }
                        let mut candidate_hasher = blake3::Hasher::new();
                        candidate_hasher.update(b"backend.ra.unresolved-module-candidate.v1\0");
                        candidate_hasher.update(
                            &u64::try_from(crate_root_file.len())
                                .unwrap_or(u64::MAX)
                                .to_be_bytes(),
                        );
                        candidate_hasher.update(crate_root_file.as_bytes());
                        candidate_hasher.update(
                            &u64::try_from(declaring_file.as_str().len())
                                .unwrap_or(u64::MAX)
                                .to_be_bytes(),
                        );
                        candidate_hasher.update(declaring_file.as_str().as_bytes());
                        candidate_hasher.update(
                            &u64::try_from(candidate.len())
                                .unwrap_or(u64::MAX)
                                .to_be_bytes(),
                        );
                        candidate_hasher.update(candidate.as_bytes());
                        let candidate_identity = *candidate_hasher.finalize().as_bytes();
                        if seen_candidates.contains(&candidate_identity) {
                            continue;
                        }
                        if seen_candidates.try_reserve(1).is_err() {
                            summary.truncated = true;
                            break;
                        }
                        seen_candidates.insert(candidate_identity);
                        observed_bytes += byte_charge;
                        summary.module_candidates_visited =
                            summary.module_candidates_visited.saturating_add(1);
                        if observer.observe_unresolved_module_candidate(
                            crate_root_file,
                            declaring_file.as_str(),
                            candidate,
                        ) {
                            summary.module_candidate_events_delivered =
                                summary.module_candidate_events_delivered.saturating_add(1);
                        }
                    }
                    if summary.truncated {
                        break;
                    }
                }
                if summary.truncated {
                    break;
                }
            }
            ()
        })
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

/// Registry network policy used while resolving Cargo metadata for one authority graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RustCargoMetadataPolicy {
    /// Permit Cargo metadata to fetch missing index and resolution state.
    Online,
    /// Forbid network access and refuse if only no-dependency metadata can be produced.
    Offline,
}

/// Why full Cargo metadata could not prove a complete dependency and feature graph.
#[derive(Debug, thiserror::Error)]
pub enum CargoMetadataIncompleteCause {
    /// The isolated Cargo metadata subprocess failed or emitted invalid data.
    #[error("Cargo metadata preflight failed: {0}")]
    Preflight(#[from] CargoMetadataPreflightError),
    /// Rust-analyzer reported that it selected its no-dependencies fallback.
    #[error("rust-analyzer selected its no-dependencies metadata fallback")]
    AnalyzerUsedNoDependenciesFallback,
}

/// Concrete terminal from bounded Cargo metadata resolution.
#[derive(Debug, thiserror::Error)]
pub enum CargoMetadataPreflightError {
    /// Cargo could not be started for an exact phase.
    #[error("could not start Cargo {phase}: {source}")]
    Spawn {
        /// Metadata phase.
        phase: &'static str,
        /// Operating-system process creation failure.
        #[source]
        source: io::Error,
    },
    /// Cargo failed and returned its bounded diagnostic stream.
    #[error("Cargo {phase} exited with {status}: {stderr}")]
    CommandFailed {
        /// Metadata phase.
        phase: &'static str,
        /// Exit status of the exact Cargo process.
        status: String,
        /// Bounded stderr returned by Cargo.
        stderr: String,
    },
    /// Cargo output exceeded a fixed retained-stream bound.
    #[error("Cargo {phase} output exceeded the {maximum}-byte {stream} limit")]
    OutputLimit {
        /// Metadata phase.
        phase: &'static str,
        /// Output stream that exceeded its bound.
        stream: &'static str,
        /// Maximum retained stream bytes.
        maximum: usize,
    },
    /// Cargo returned invalid JSON for its metadata response.
    #[error("Cargo {phase} returned invalid metadata JSON: {source}")]
    InvalidJson {
        /// Metadata phase.
        phase: &'static str,
        /// Exact JSON parser failure.
        #[source]
        source: serde_json::Error,
    },
    /// The full Cargo invocation returned no dependency resolution object.
    #[error("Cargo full metadata did not contain a resolved dependency graph")]
    MissingResolutionGraph,
    /// Cargo could not identify the workspace root from no-deps metadata.
    #[error("Cargo no-deps metadata omitted its workspace root")]
    MissingWorkspaceRoot,
    /// This selected Cargo is too old to safely redirect a missing lockfile.
    #[error("Cargo {version} cannot resolve a project without creating {lockfile}")]
    NoSafeLockfilePath {
        /// Exact Cargo version output.
        version: String,
        /// Workspace lockfile that must remain absent.
        lockfile: PathBuf,
    },
    /// Cargo reported an unusable version while a temporary lockfile was needed.
    #[error("cannot determine whether selected Cargo supports isolated lockfiles: {output}")]
    UnknownCargoVersion {
        /// Exact Cargo version output.
        output: String,
    },
    /// A temporary isolated lockfile location could not be created.
    #[error("cannot create isolated Cargo lockfile: {source}")]
    TemporaryLockfile {
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
}

/// Resolve full Cargo metadata before rust-analyzer may construct HIR roots.
///
/// A successful cargo metadata --no-deps response omits dependency resolution
/// and active feature facts, so it cannot authorize semantic work. This gate
/// requires the full JSON response to contain a concrete resolve.nodes graph.
/// It uses the selected Cargo, feature set, network policy, and an isolated
/// temporary lockfile when the project does not already have one.
struct CargoMetadataGate {
    lockfile_exists: bool,
    metadata_extra_args: Vec<String>,
    extra_env: Vec<(String, Option<String>)>,
    _isolated_lockfile: Option<IsolatedCargoLockfile>,
}

fn cargo_metadata_preflight(
    root: &Path,
    cargo: &Path,
    cargo_home: &Path,
    toolchain: &RustToolchain,
    features: RustFeatureControl<'_>,
    policy: RustCargoMetadataPolicy,
    control: RustAnalysisControl<'_>,
) -> Result<CargoMetadataGate, RustAuthorityError> {
    let manifest = root.join("Cargo.toml");
    let mut no_deps_command = cargo_metadata_command(
        cargo, cargo_home, toolchain, root, &manifest, features, policy,
    )?;
    no_deps_command.arg("--no-deps");
    let no_deps_output = run_cargo_metadata_process(no_deps_command, control, "metadata --no-deps")
        .map_err(|failure| metadata_process_failure(root, policy, failure))?;
    let no_deps_json = parse_metadata_json("metadata --no-deps", &no_deps_output.stdout)
        .map_err(|cause| metadata_incomplete(root, policy, cause))?;
    let workspace_root = no_deps_json
        .get("workspace_root")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            metadata_incomplete(
                root,
                policy,
                CargoMetadataPreflightError::MissingWorkspaceRoot,
            )
        })?;
    let workspace_root = if workspace_root.is_absolute() {
        workspace_root
    } else {
        root.join(workspace_root)
    };
    let lockfile = workspace_root.join("Cargo.lock");
    let lockfile_exists = lockfile.is_file();
    let mut command = cargo_metadata_command(
        cargo, cargo_home, toolchain, root, &manifest, features, policy,
    )?;

    let mut metadata_extra_args = Vec::new();
    let mut extra_env = Vec::new();
    let isolated_lockfile = if lockfile_exists {
        command.arg("--locked");
        None
    } else {
        let version_output = run_cargo_metadata_process(
            {
                let mut command = cargo_base_command(cargo, cargo_home, toolchain, root)?;
                command.arg("--version");
                command
            },
            control,
            "--version",
        )
        .map_err(|failure| metadata_process_failure(root, policy, failure))?;
        let version_text = String::from_utf8_lossy(&version_output.stdout)
            .trim()
            .to_owned();
        let Some((major, minor, _patch)) = parse_cargo_version(&version_text) else {
            return Err(metadata_incomplete(
                root,
                policy,
                CargoMetadataPreflightError::UnknownCargoVersion {
                    output: version_text,
                },
            ));
        };
        let isolated = IsolatedCargoLockfile::create().map_err(|source| {
            metadata_incomplete(
                root,
                policy,
                CargoMetadataPreflightError::TemporaryLockfile { source },
            )
        })?;
        if major > 1 || (major == 1 && minor >= 95) {
            command
                .env("CARGO_RESOLVER_LOCKFILE_PATH", isolated.path.as_os_str())
                .env("__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS", "nightly")
                .arg("-Zunstable-options");
            metadata_extra_args.push("-Zunstable-options".to_owned());
            extra_env.push((
                "CARGO_RESOLVER_LOCKFILE_PATH".to_owned(),
                Some(isolated.path.to_string_lossy().into_owned()),
            ));
            extra_env.push((
                "__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS".to_owned(),
                Some("nightly".to_owned()),
            ));
        } else if major == 1 && minor >= 82 {
            command
                .arg("--lockfile-path")
                .arg(&isolated.path)
                .arg("-Zunstable-options")
                .env("__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS", "nightly");
            metadata_extra_args.extend([
                "--lockfile-path".to_owned(),
                isolated.path.to_string_lossy().into_owned(),
                "-Zunstable-options".to_owned(),
            ]);
            extra_env.push((
                "__CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS".to_owned(),
                Some("nightly".to_owned()),
            ));
        } else {
            return Err(metadata_incomplete(
                root,
                policy,
                CargoMetadataPreflightError::NoSafeLockfilePath {
                    version: version_text,
                    lockfile,
                },
            ));
        }
        Some(isolated)
    };

    let output = run_cargo_metadata_process(command, control, "metadata full")
        .map_err(|failure| metadata_process_failure(root, policy, failure))?;
    let metadata = parse_metadata_json("metadata full", &output.stdout)
        .map_err(|cause| metadata_incomplete(root, policy, cause))?;
    validate_resolution_graph(&metadata)
        .map_err(|cause| metadata_incomplete(root, policy, cause))?;
    control.check()?;
    Ok(CargoMetadataGate {
        lockfile_exists,
        metadata_extra_args,
        extra_env,
        _isolated_lockfile: isolated_lockfile,
    })
}

fn cargo_base_command(
    cargo: &Path,
    cargo_home: &Path,
    toolchain: &RustToolchain,
    root: &Path,
) -> Result<Command, RustAuthorityError> {
    let path = toolchain.authority_path()?;
    let mut command = Command::new(cargo);
    command
        .current_dir(root)
        .env_clear()
        .env("CARGO", cargo)
        .env("CARGO_HOME", cargo_home)
        .env("HOME", cargo_home)
        .env("RUSTC", &toolchain.tool)
        .env("PATH", path)
        .env("CARGO_TERM_COLOR", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(rustup_home) = &toolchain.rustup_home {
        command.env("RUSTUP_HOME", rustup_home);
    }
    if let Some(rustup_toolchain) = &toolchain.rustup_toolchain {
        command.env("RUSTUP_TOOLCHAIN", rustup_toolchain);
    }
    Ok(command)
}

fn cargo_metadata_command(
    cargo: &Path,
    cargo_home: &Path,
    toolchain: &RustToolchain,
    root: &Path,
    manifest: &Path,
    features: RustFeatureControl<'_>,
    policy: RustCargoMetadataPolicy,
) -> Result<Command, RustAuthorityError> {
    let mut command = cargo_base_command(cargo, cargo_home, toolchain, root)?;
    command
        .arg("metadata")
        .arg("--manifest-path")
        .arg(manifest)
        .arg("--format-version")
        .arg("1");
    match features.cargo_features() {
        CargoFeatures::All => {
            command.arg("--all-features");
        }
        CargoFeatures::Selected {
            features,
            no_default_features,
        } => {
            if no_default_features {
                command.arg("--no-default-features");
            }
            if !features.is_empty() {
                command.arg("--features").arg(features.join(","));
            }
        }
    }
    match policy {
        RustCargoMetadataPolicy::Online => {
            command
                .arg("--config")
                .arg("net.offline=false")
                .env("CARGO_NET_OFFLINE", "false");
        }
        RustCargoMetadataPolicy::Offline => {
            command.arg("--offline").env("CARGO_NET_OFFLINE", "true");
        }
    }
    Ok(command)
}

fn parse_metadata_json(
    phase: &'static str,
    stdout: &[u8],
) -> Result<serde_json::Value, CargoMetadataPreflightError> {
    serde_json::from_slice(stdout)
        .map_err(|source| CargoMetadataPreflightError::InvalidJson { phase, source })
}

fn validate_resolution_graph(
    metadata: &serde_json::Value,
) -> Result<(), CargoMetadataPreflightError> {
    let Some(resolve) = metadata.get("resolve").filter(|value| !value.is_null()) else {
        return Err(CargoMetadataPreflightError::MissingResolutionGraph);
    };
    if resolve
        .get("nodes")
        .and_then(serde_json::Value::as_array)
        .is_none()
    {
        return Err(CargoMetadataPreflightError::MissingResolutionGraph);
    }
    Ok(())
}

fn parse_cargo_version(output: &str) -> Option<(u64, u64, u64)> {
    let version = output.split_whitespace().find(|part| {
        part.chars()
            .next()
            .is_some_and(|character| character.is_ascii_digit())
    })?;
    let mut parts = version.split(|character| character == '.' || character == '-');
    Some((
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ))
}

fn metadata_incomplete(
    root: &Path,
    policy: RustCargoMetadataPolicy,
    cause: CargoMetadataPreflightError,
) -> RustAuthorityError {
    RustAuthorityError::CargoMetadataIncomplete {
        root: root.to_path_buf(),
        policy,
        cause: CargoMetadataIncompleteCause::Preflight(cause),
    }
}

enum CargoMetadataProcessFailure {
    Cancelled,
    Deadline,
    Incomplete(CargoMetadataPreflightError),
}

fn metadata_process_failure(
    root: &Path,
    policy: RustCargoMetadataPolicy,
    failure: CargoMetadataProcessFailure,
) -> RustAuthorityError {
    match failure {
        CargoMetadataProcessFailure::Cancelled => RustAuthorityError::Cancelled,
        CargoMetadataProcessFailure::Deadline => RustAuthorityError::DeadlineExceeded,
        CargoMetadataProcessFailure::Incomplete(cause) => metadata_incomplete(root, policy, cause),
    }
}

struct CargoMetadataOutput {
    stdout: Vec<u8>,
}

fn run_cargo_metadata_process(
    mut command: Command,
    control: RustAnalysisControl<'_>,
    phase: &'static str,
) -> Result<CargoMetadataOutput, CargoMetadataProcessFailure> {
    control.check().map_err(|error| match error {
        RustAuthorityError::Cancelled => CargoMetadataProcessFailure::Cancelled,
        _ => CargoMetadataProcessFailure::Deadline,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().map_err(|source| {
        CargoMetadataProcessFailure::Incomplete(CargoMetadataPreflightError::Spawn {
            phase,
            source,
        })
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        CargoMetadataProcessFailure::Incomplete(CargoMetadataPreflightError::Spawn {
            phase,
            source: io::Error::other("Cargo stdout pipe was unavailable"),
        })
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        CargoMetadataProcessFailure::Incomplete(CargoMetadataPreflightError::Spawn {
            phase,
            source: io::Error::other("Cargo stderr pipe was unavailable"),
        })
    })?;
    let stdout_worker = spawn_cargo_reader(stdout);
    let stderr_worker = spawn_cargo_reader(stderr);
    let status = loop {
        if control.cancelled.load(Ordering::Acquire) {
            terminate_cargo(&mut child);
            let _ = join_cargo_reader(stdout_worker);
            let _ = join_cargo_reader(stderr_worker);
            return Err(CargoMetadataProcessFailure::Cancelled);
        }
        if Instant::now() >= control.deadline {
            terminate_cargo(&mut child);
            let _ = join_cargo_reader(stdout_worker);
            let _ = join_cargo_reader(stderr_worker);
            return Err(CargoMetadataProcessFailure::Deadline);
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(CARGO_METADATA_POLL_INTERVAL),
            Err(source) => {
                terminate_cargo(&mut child);
                let _ = join_cargo_reader(stdout_worker);
                let _ = join_cargo_reader(stderr_worker);
                return Err(CargoMetadataProcessFailure::Incomplete(
                    CargoMetadataPreflightError::Spawn { phase, source },
                ));
            }
        }
    };
    let stdout = join_cargo_reader(stdout_worker).map_err(|source| {
        CargoMetadataProcessFailure::Incomplete(CargoMetadataPreflightError::Spawn {
            phase,
            source,
        })
    })?;
    let stderr = join_cargo_reader(stderr_worker).map_err(|source| {
        CargoMetadataProcessFailure::Incomplete(CargoMetadataPreflightError::Spawn {
            phase,
            source,
        })
    })?;
    if stdout.1 {
        return Err(CargoMetadataProcessFailure::Incomplete(
            CargoMetadataPreflightError::OutputLimit {
                phase,
                stream: "stdout",
                maximum: MAX_CARGO_METADATA_STREAM_BYTES,
            },
        ));
    }
    if stderr.1 {
        return Err(CargoMetadataProcessFailure::Incomplete(
            CargoMetadataPreflightError::OutputLimit {
                phase,
                stream: "stderr",
                maximum: MAX_CARGO_METADATA_STREAM_BYTES,
            },
        ));
    }
    if !status.success() {
        return Err(CargoMetadataProcessFailure::Incomplete(
            CargoMetadataPreflightError::CommandFailed {
                phase,
                status: status.to_string(),
                stderr: String::from_utf8_lossy(&stderr.0).into_owned(),
            },
        ));
    }
    Ok(CargoMetadataOutput { stdout: stdout.0 })
}

fn spawn_cargo_reader(
    mut reader: impl Read + Send + 'static,
) -> JoinHandle<io::Result<(Vec<u8>, bool)>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut exceeded = false;
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            let read = reader.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            let remaining = MAX_CARGO_METADATA_STREAM_BYTES.saturating_sub(bytes.len());
            let retained = read.min(remaining);
            bytes.extend_from_slice(&chunk[..retained]);
            exceeded |= retained < read;
        }
        Ok((bytes, exceeded))
    })
}

fn join_cargo_reader(
    worker: JoinHandle<io::Result<(Vec<u8>, bool)>>,
) -> io::Result<(Vec<u8>, bool)> {
    worker
        .join()
        .map_err(|_| io::Error::other("Cargo output reader panicked"))?
}

fn terminate_cargo(child: &mut Child) {
    #[cfg(unix)]
    {
        let process_group = format!("-{}", child.id());
        let _ = Command::new("/bin/kill")
            .args(["-KILL", "--", process_group.as_str()])
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

struct IsolatedCargoLockfile {
    directory: PathBuf,
    path: PathBuf,
}

impl IsolatedCargoLockfile {
    fn create() -> io::Result<Self> {
        for _ in 0..32 {
            let sequence = CARGO_METADATA_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let directory = std::env::temp_dir().join(format!(
                "backend-cargo-metadata-{}-{sequence}",
                std::process::id()
            ));
            match fs::create_dir(&directory) {
                Ok(()) => {
                    return Ok(Self {
                        path: directory.join("Cargo.lock"),
                        directory,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate unique temporary Cargo metadata directory",
        ))
    }
}

impl Drop for IsolatedCargoLockfile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
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

    /// Visits rust-analyzer's resolved Rustdoc text, mapping ordinary lines back to this exact
    /// source buffer and marking macro-expanded lines as owned text.
    pub fn visit_declaration_documentation(
        &self,
        definition: &RustDefinition,
        mut receive: impl FnMut(&str, Option<ByteSpan>) -> Result<(), RustAuthorityError>,
    ) -> Result<(), RustAuthorityError> {
        let Some(docs) = definition.docs_with_rangemap(self.database) else {
            return Ok(());
        };
        let text = docs.docs();
        if text.len() > MAX_RUST_DOCUMENTATION_INPUT_BYTES {
            return Err(RustAuthorityError::DocumentationInputLimit {
                actual: text.len(),
                maximum: MAX_RUST_DOCUMENTATION_INPUT_BYTES,
            });
        }
        let mut offset = 0_usize;
        for line in text.split_terminator('\n') {
            let start = TextSize::of(&text[..offset]);
            let end = start + TextSize::of(line);
            let range = TextRange::new(start, end);
            let mapped_span = docs.find_ast_range(range).and_then(|(mapped, _)| {
                (mapped.file_id == self.source_file).then_some(mapped.value)
            });
            let span = if let Some(range) = mapped_span {
                let span =
                    ByteSpan::from_text_range(range).ok_or(RustAuthorityError::InvalidSpan {
                        span: ByteSpan { start: 1, end: 0 },
                        source_bytes: self.source.len(),
                    })?;
                bytes_at(self.source, span)?;
                Some(span)
            } else {
                None
            };
            receive(line, span)?;
            offset = offset.saturating_add(line.len()).saturating_add(1);
        }
        Ok(())
    }

    /// Visits raw source comments for syntax-only items such as `use` re-exports, which have no
    /// source-owned HIR definition in this authority's declaration lane.
    pub fn visit_syntax_documentation(
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
    fn docs_with_rangemap<'db>(
        &self,
        database: &'db RootDatabase,
    ) -> Option<Cow<'db, ra_ap_hir::Docs>> {
        match self {
            Self::Field(definition) => definition.docs_with_rangemap(database),
            Self::Variant(definition) => definition.docs_with_rangemap(database),
            Self::Macro(definition) => definition.docs_with_rangemap(database),
            Self::Module(definition) => definition.docs_with_rangemap(database),
            Self::Trait(definition) => definition.docs_with_rangemap(database),
            Self::Implementation(definition) => definition.docs_with_rangemap(database),
            Self::Function(definition) => definition.docs_with_rangemap(database),
            Self::Record(definition) | Self::Enum(definition) => {
                definition.docs_with_rangemap(database)
            }
            Self::TypeAlias(definition) => definition.docs_with_rangemap(database),
            Self::Constant(definition) => definition.docs_with_rangemap(database),
            Self::Static(definition) => definition.docs_with_rangemap(database),
        }
    }

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
    /// A selected Rustdoc include could not be resolved to a package file.
    #[error("Rustdoc include file is missing: {path}")]
    DocumentationInputMissing {
        /// Exact source-relative include path after anchoring to its Rust source.
        path: PathBuf,
    },
    /// A Rustdoc include could not be inspected or read as a regular file.
    #[error("cannot read Rustdoc include file {path}: {source}")]
    DocumentationInputRead {
        /// Exact canonical or requested include path.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// One Rustdoc include exceeds the selected source byte budget.
    #[error(
        "Rustdoc include {path} is {actual} bytes, exceeding the {maximum:?}-byte authority budget"
    )]
    DocumentationInputBudget {
        /// Exact canonical include path.
        path: PathBuf,
        /// Observed include length before the bounded read.
        actual: u64,
        /// Caller-selected maximum source bytes.
        maximum: SourceByteLimit,
    },
    /// A Rustdoc include is not valid UTF-8, as required by include_str!.
    #[error("Rustdoc include file is not valid UTF-8: {path}")]
    DocumentationInputUtf8 {
        /// Exact canonical include path.
        path: PathBuf,
    },
    /// The include count or combined byte budget was exceeded.
    #[error("Rustdoc include inputs exceed bounded limit {maximum} (observed {actual})")]
    DocumentationInputLimit {
        /// Observed number of paths or combined bytes.
        actual: usize,
        /// Maximum number of paths or combined bytes admitted.
        maximum: usize,
    },
    /// A documentation attribute expression cannot be admitted before RA expansion.
    #[error("unsupported Rustdoc expression in {path}: {expression}")]
    UnsupportedDocumentationExpression {
        /// Source file containing the attribute.
        path: PathBuf,
        /// Exact attribute or configuration syntax rejected by the bounded preloader.
        expression: String,
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
    /// Full Cargo dependency/feature resolution failed and rust-analyzer only had its `--no-deps`
    /// fallback, which is not sufficient proof for semantic authority.
    #[error("Cargo metadata resolution is incomplete for {root} under {policy:?} policy")]
    CargoMetadataIncomplete {
        /// Caller-selected project root.
        root: PathBuf,
        /// Exact policy that constrained Cargo metadata resolution.
        policy: RustCargoMetadataPolicy,
        /// Concrete Cargo or analyzer fallback cause.
        #[source]
        cause: CargoMetadataIncompleteCause,
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
    /// The complete active DefMap scan exceeded its bounded crate or module limit.
    #[error("Rust source ownership scan exceeded the {maximum}-entry limit")]
    SourceOwnershipIndexLimit {
        /// Maximum active crate or module entries examined.
        maximum: usize,
    },
    /// A source request reached a workspace without its completed ownership index.
    #[error("Rust active module ownership index is unavailable")]
    SourceOwnershipIndexUnavailable,
    /// The same physical source is defined in multiple active Cargo crate contexts.
    #[error(
        "selected Rust source has {definition_count} active module definitions: {path}; owner roots: {owner_root_sample:?} ({omitted_owner_definitions} definitions omitted)"
    )]
    AmbiguousSourceOwner {
        /// Exact selected package source with multiple active owners.
        path: PathBuf,
        /// Exact number of active DefMap module definitions for this file.
        definition_count: usize,
        /// Bounded package-relative sample of the owning Cargo crate roots.
        owner_root_sample: Box<[PathBuf]>,
        /// Number of owning module definitions omitted after filling the sample.
        omitted_owner_definitions: usize,
    },
    /// The selected file exists in the package VFS but is outside all active Cargo targets.
    #[error(
        "selected Rust source is cfg-inactive or detached from every active Cargo target: {path}; active package HIR roots: {active_hir_roots:?}"
    )]
    DetachedSource {
        /// Exact selected package source without active Cargo HIR ownership.
        path: PathBuf,
        /// Bounded exact HIR root evidence from the loaded package graph.
        active_hir_roots: RustActiveHirRootInventory,
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

fn advance_bounded_counter(counter: &mut u64, maximum: u64) -> bool {
    if *counter >= maximum {
        return false;
    }
    *counter += 1;
    true
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

#[cfg(test)]
mod read_frontier_budget_tests {
    use super::advance_bounded_counter;

    #[test]
    fn empty_and_duplicate_diagnostics_still_consume_the_examined_budget() {
        let diagnostic_candidate_lists = [Vec::<&str>::new(), vec!["same.rs"], vec!["same.rs"]];
        let mut diagnostics_visited = 0;
        let maximum = 2;
        let mut candidates = std::collections::HashSet::new();
        let mut truncated = false;

        for diagnostic in &diagnostic_candidate_lists {
            if !advance_bounded_counter(&mut diagnostics_visited, maximum) {
                truncated = true;
                break;
            }
            for candidate in diagnostic {
                candidates.insert(*candidate);
            }
        }

        assert_eq!(diagnostics_visited, maximum);
        assert_eq!(candidates.len(), 1);
        assert!(
            truncated,
            "empty and duplicate results must not bypass the cap"
        );
        assert!(!advance_bounded_counter(&mut diagnostics_visited, maximum));
    }

    #[test]
    fn offline_no_deps_metadata_fallback_is_refused() {
        use super::{
            CargoMetadataIncompleteCause, CargoMetadataPreflightError, RustAnalysisControl,
            RustAuthorityError, RustCargoMetadataPolicy, RustFeatureControl, RustToolchain,
            RustWorkspace, SourceByteLimit,
        };
        use backend_semantic::vocabulary::RustEdition;
        use std::sync::atomic::AtomicBool;
        use std::time::{Instant, SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "backend-rust-metadata-fallback-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("src")).expect("fixture source directory");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"readiness-metadata-fallback\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[workspace]\n[dependencies]\nbackend-readiness-absent-metadata-fixture = \"=0.0.1\"\n",
        )
        .expect("fixture manifest");
        std::fs::write(root.join("src/lib.rs"), "pub fn fixture() {}\n")
            .expect("fixture library source");
        let tool = std::env::var_os("RUSTC")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("rustc"));
        let toolchain = RustToolchain::discover(tool).expect("selected Rust toolchain");
        let cancelled = AtomicBool::new(false);
        let result = RustWorkspace::open_with_features_and_metadata_policy(
            &root,
            &toolchain,
            RustEdition::Rust2021,
            RustFeatureControl {
                all_features: true,
                no_default_features: false,
                features: &[],
            },
            RustCargoMetadataPolicy::Offline,
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(1024 * 1024),
                deadline: Instant::now() + std::time::Duration::from_secs(60),
            },
        );
        assert!(
            !root.join("Cargo.lock").exists(),
            "metadata resolution must not write a lockfile into the project"
        );
        assert!(matches!(
            &result,
            Err(RustAuthorityError::CargoMetadataIncomplete {
                policy: RustCargoMetadataPolicy::Offline,
                cause: CargoMetadataIncompleteCause::Preflight(
                    CargoMetadataPreflightError::CommandFailed {
                        phase: "metadata full",
                        ..
                    }
                ),
                ..
            })
        ));
        let _ = std::fs::remove_dir_all(&root);
        assert!(matches!(
            result,
            Err(RustAuthorityError::CargoMetadataIncomplete {
                policy: RustCargoMetadataPolicy::Offline,
                ..
            })
        ));
    }

    #[test]
    fn all_feature_metadata_owns_declared_example_and_gated_module_without_lock_mutation() {
        use super::{
            RustAnalysisControl, RustCargoMetadataPolicy, RustFeatureControl, RustToolchain,
            RustWorkspace, SourceByteLimit,
        };
        use backend_semantic::vocabulary::RustEdition;
        use std::sync::atomic::AtomicBool;
        use std::time::{Instant, SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "backend-rust-all-feature-metadata-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("src")).expect("fixture source directory");
        std::fs::create_dir_all(root.join("examples")).expect("fixture example directory");
        std::fs::write(
            root.join("Cargo.toml"),
            concat!(
                "[package]\n",
                "name = \"readiness_feature_metadata\"\n",
                "version = \"0.1.0\"\n",
                "edition = \"2021\"\n",
                "[workspace]\n",
                "[features]\n",
                "default = []\n",
                "display = []\n",
                "[[example]]\n",
                "name = \"feature_probe\"\n",
                "path = \"examples/feature_probe.rs\"\n",
                "required-features = [\"display\"]\n",
            ),
        )
        .expect("fixture manifest");
        let library = b"#[cfg(feature = \"display\")]\npub mod display;\n";
        let gated_module = b"pub fn enabled() {}\n";
        let example = b"fn main() { readiness_feature_metadata::display::enabled(); }\n";
        let build_script = b"fn main() {}\n";
        std::fs::write(root.join("src/lib.rs"), library).expect("fixture library");
        std::fs::write(root.join("src/display.rs"), gated_module).expect("fixture module");
        std::fs::write(root.join("examples/feature_probe.rs"), example).expect("fixture example");
        std::fs::write(root.join("build.rs"), build_script).expect("fixture build script");
        let tool = std::env::var_os("RUSTC")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("rustc"));
        let toolchain = RustToolchain::discover(tool).expect("selected Rust toolchain");
        let cancelled = AtomicBool::new(false);
        let control = || RustAnalysisControl {
            cancelled: &cancelled,
            maximum_source_bytes: SourceByteLimit::from(1024 * 1024),
            deadline: Instant::now() + std::time::Duration::from_secs(60),
        };
        let workspace = RustWorkspace::open_with_features_and_metadata_policy(
            &root,
            &toolchain,
            RustEdition::Rust2021,
            RustFeatureControl {
                all_features: true,
                no_default_features: false,
                features: &[],
            },
            RustCargoMetadataPolicy::Offline,
            control(),
        )
        .expect("complete all-features Cargo graph");
        workspace
            .analyze_source(root.join("src/display.rs"), gated_module, control(), |_| {
                Ok(())
            })
            .expect("cfg-selected module has HIR ownership");
        workspace
            .analyze_source(
                root.join("examples/feature_probe.rs"),
                example,
                control(),
                |_| Ok(()),
            )
            .expect("declared example target has HIR ownership");
        workspace
            .analyze_source(root.join("build.rs"), build_script, control(), |_| Ok(()))
            .expect("declared build-script target has HIR ownership");
        assert!(
            !root.join("Cargo.lock").exists(),
            "metadata and analyzer loading must not write a lockfile into the project"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn source_ownership_indexes_the_captured_backend_present_assemble_file() {
        use super::{
            RustAnalysisControl, RustAuthorityError, RustCargoMetadataPolicy, RustFeatureControl,
            RustSourceScope, RustToolchain, RustWorkspace, SourceByteLimit,
        };
        use backend_semantic::vocabulary::RustEdition;
        use std::sync::atomic::AtomicBool;
        use std::time::Instant;

        let repository_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repository root");
        let package_root = std::env::var_os("BACKEND_PRESENT_HARNESS_ROOT")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| repository_root.join("crates/present"));
        let source_path = package_root.join("assemble.rs");
        let source = std::fs::read(&source_path).expect("backend-present assemble source");
        assert_eq!(
            source.as_slice(),
            include_bytes!("../../../../crates/present/assemble.rs"),
            "the captured Harness source must match the real backend-present module"
        );
        let library = std::fs::read_to_string(package_root.join("lib.rs"))
            .expect("backend-present library root");
        assert!(
            library.lines().any(|line| line.trim() == "mod assemble;"),
            "the package root must actively declare assemble without a cfg gate"
        );
        let tool = std::env::var_os("RUSTC")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("rustc"));
        let toolchain = RustToolchain::discover(tool).expect("selected Rust toolchain");
        let cancelled = AtomicBool::new(false);
        let control = || RustAnalysisControl {
            cancelled: &cancelled,
            maximum_source_bytes: SourceByteLimit::from(1024 * 1024),
            deadline: Instant::now() + std::time::Duration::from_secs(180),
        };
        let workspace = RustWorkspace::open_with_features_and_metadata_policy(
            &package_root,
            &toolchain,
            RustEdition::Rust2024,
            RustFeatureControl::default(),
            RustCargoMetadataPolicy::Offline,
            control(),
        )
        .expect("load backend-present through Cargo metadata and rust-analyzer");
        let result = workspace.analyze_source(&source_path, &source, control(), |authority| {
            assert_eq!(authority.source_scope, RustSourceScope::CargoModule);
            Ok(())
        });
        assert!(
            result.is_ok(),
            "active lib.rs module lost its Cargo owner: {result:?}"
        );
        assert!(matches!(
            workspace.analyze_source(&source_path, b"different bytes\n", control(), |_| Ok(())),
            Err(RustAuthorityError::SourceBinding { .. })
        ));
    }

    #[test]
    fn source_ownership_reports_shared_and_complete_absence_from_one_retained_index() {
        use super::{
            RustAnalysisControl, RustAuthorityError, RustCargoMetadataPolicy, RustFeatureControl,
            RustToolchain, RustWorkspace, SourceByteLimit,
        };
        use backend_semantic::vocabulary::RustEdition;
        use std::sync::atomic::AtomicBool;
        use std::time::{Instant, SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after the epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "backend-rust-source-ownership-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("fixture package directory");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"readiness-source-ownership\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n[lib]\npath = \"lib.rs\"\n[[bin]]\nname = \"readiness-source-ownership-bin\"\npath = \"main.rs\"\n",
        )
        .expect("fixture manifest");
        let library = b"#[path = \"shared.rs\"] mod shared;\n";
        let binary = b"#[path = \"shared.rs\"] mod shared;\nfn main() {}\n";
        let shared = b"pub fn shared() {}\n";
        let inactive = b"pub fn inactive() {}\n";
        std::fs::write(root.join("lib.rs"), library).expect("library target");
        std::fs::write(root.join("main.rs"), binary).expect("binary target");
        std::fs::write(root.join("shared.rs"), shared).expect("shared module");
        std::fs::write(root.join("inactive.rs"), inactive).expect("inactive source");
        let tool = std::env::var_os("RUSTC")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("rustc"));
        let toolchain = RustToolchain::discover(tool).expect("selected Rust toolchain");
        let cancelled = AtomicBool::new(false);
        let control = || RustAnalysisControl {
            cancelled: &cancelled,
            maximum_source_bytes: SourceByteLimit::from(1024 * 1024),
            deadline: Instant::now() + std::time::Duration::from_secs(120),
        };
        let workspace = RustWorkspace::open_with_features_and_metadata_policy(
            &root,
            &toolchain,
            RustEdition::Rust2024,
            RustFeatureControl::default(),
            RustCargoMetadataPolicy::Offline,
            control(),
        )
        .expect("load both Cargo targets");
        let retained_index = std::ptr::from_ref(
            workspace
                .source_ownership_index
                .as_ref()
                .expect("index prepared before workspace is shared"),
        );
        assert!(matches!(
            workspace.analyze_source(root.join("shared.rs"), shared, control(), |_| Ok(())),
            Err(RustAuthorityError::AmbiguousSourceOwner {
                definition_count: 2,
                ..
            })
        ));
        assert!(matches!(
            workspace.analyze_source(root.join("inactive.rs"), inactive, control(), |_| Ok(())),
            Err(RustAuthorityError::DetachedSource { .. })
        ));
        assert!(std::ptr::eq(
            retained_index,
            std::ptr::from_ref(
                workspace
                    .source_ownership_index
                    .as_ref()
                    .expect("same retained index after per-source queries")
            )
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn cargo_metadata_preflight_kills_its_process_group_on_cancellation() {
        use super::{
            CargoMetadataProcessFailure, RustAnalysisControl, SourceByteLimit,
            run_cargo_metadata_process,
        };
        use std::time::{Duration, Instant};
        use std::{
            process::{Command, Stdio},
            sync::atomic::AtomicBool,
            thread,
        };

        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let signal = std::sync::Arc::clone(&cancelled);
        let cancelling_worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(80));
            signal.store(true, std::sync::atomic::Ordering::Release);
        });
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "sleep 10"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let result = run_cargo_metadata_process(
            command,
            RustAnalysisControl {
                cancelled: &cancelled,
                maximum_source_bytes: SourceByteLimit::from(1024),
                deadline: Instant::now() + Duration::from_secs(5),
            },
            "cancellation regression",
        );
        cancelling_worker.join().expect("cancellation worker");
        assert!(matches!(
            result,
            Err(CargoMetadataProcessFailure::Cancelled)
        ));
    }
}
