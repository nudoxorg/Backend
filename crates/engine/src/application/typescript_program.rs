//! Compiler-API-resolved TypeScript program closure for native TSZ checking.
//!
//! The selected TypeScript package constructs the real program and resolves each
//! module request. The Rust side admits every source and filesystem observation
//! through the package's resolver capability before those facts can enter TSZ.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::Instant,
};

use backend_frontend_typescript::{
    TszCheckerOptions, TszEnvironmentFingerprint, TszFileInput, TszLibraryInput,
    TszProjectFileFormat, TszProjectFileFormatInput, TszProjectModuleRequestKind,
    TszProjectModuleResolution, TszProjectModuleResolutionTarget, TszProjectOptions,
    TszProjectSemanticOptions, checker_options_from_compiler_api_json,
};
use backend_version::{ContentId, SourceFactDomain};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::typescript_host::{
    TypeScriptDirectoryEntryKind, TypeScriptProjectHostError, TypeScriptProjectInputs,
    TypeScriptResolverCapability, TypeScriptResolverWitness, ValidatedTypeScriptHost,
};
use super::{ToolchainProbeLimits, toolchain_probe::run_typescript_program_bridge};
use crate::application::compiler::PackageSource;

mod direct;

const MAX_BRIDGE_STDOUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_BRIDGE_FILES: usize = 16_384;
const MAX_BRIDGE_REQUESTS: usize = 131_072;
const MAX_BRIDGE_ACCESS_OPERATIONS: usize = 131_072;
const MAX_BRIDGE_DIRECTORY_QUERIES: usize = 16_384;
const MAX_BRIDGE_DIRECTORY_ENUMERATIONS: usize = 65_536;
const MAX_BRIDGE_DIRECTORY_ENTRIES: usize = 65_536;
const MAX_BRIDGE_MATCHER_RESULT_WORK: usize = 196_608;
const MAX_BRIDGE_MATCHER_PATTERN_ITEMS: usize = 131_072;
const MAX_BRIDGE_MATCHER_PATTERN_BYTES: usize = 512 * 1024;
const MAX_BRIDGE_DIRECTORY_QUERY_CACHE_BYTES: usize = 1024 * 1024;
pub(super) const MAX_CLOSURE_REPLAY_WORK_UNITS: u64 = 2_000_000;
const MAX_PROJECT_WORK_UNITS: u64 = 16_000_000_000;
const LIB_VIRTUAL_PREFIX: &str = "@compiler/lib.";

/// Fully admitted source/lib/options set produced by the selected compiler API.
pub(crate) struct NativeTypeScriptInputs {
    pub(crate) sources: Vec<TszFileInput>,
    pub(crate) libraries: Vec<std::sync::Arc<backend_frontend_typescript::TszLibFile>>,
    pub(crate) options: TszProjectOptions,
    pub(crate) closure_witness: TypeScriptProgramClosureWitness,
    /// Membership of every captured package source in the exact configured program.
    pub(crate) package_membership: BTreeMap<Box<str>, TypeScriptPackageSourceMembership>,
    pub(crate) work_units: u64,
    /// What one file of this program may spend before it alone becomes a coverage gap.
    pub(crate) file_work_units: u64,
    pub(crate) solution: bool,
}

impl NativeTypeScriptInputs {
    /// Solution containers have no semantic members, but their native config
    /// diagnostics still need an exact retained project owner.
    pub(crate) fn requires_retained_authority(&self) -> bool {
        !self.solution || !self.options.program_diagnostics.is_empty()
    }
}

/// A captured source can retain provenance without being a Compiler API program member.
/// Exclusion never admits that source to TSZ or supplies semantic authority for it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TypeScriptPackageSourceMembership {
    Selected { program_path: Box<str> },
    ExcludedFromConfiguredProgram,
}

impl TypeScriptPackageSourceMembership {
    pub(crate) fn selected_program_path(&self) -> Option<&str> {
        match self {
            Self::Selected { program_path } => Some(program_path),
            Self::ExcludedFromConfiguredProgram => None,
        }
    }
}

/// Owns both the admitted host reads and the exact Compiler API directory-query transcript.
#[derive(Debug)]
pub(crate) struct TypeScriptProgramClosureWitness {
    resolver: TypeScriptResolverWitness,
    compiler_accesses: Box<[ProgramAccess]>,
    directory_views: Box<[ProgramDirectoryView]>,
    metrics: ProgramClosureMetrics,
    access_digest: [u8; 32],
}

impl TypeScriptProgramClosureWitness {
    fn retained_bytes(&self) -> usize {
        self.resolver
            .retained_bytes()
            .saturating_add(retained_slice_bytes(
                &self.compiler_accesses,
                ProgramAccess::retained_bytes,
            ))
            .saturating_add(retained_slice_bytes(
                &self.directory_views,
                ProgramDirectoryView::retained_bytes,
            ))
    }

    pub(crate) fn validate_current(
        &self,
        witness: &super::typescript_host::TypeScriptProjectWitness,
    ) -> Result<(), TypeScriptProjectHostError> {
        self.validate_with_host(&witness.validated()?)
    }

    fn validate_with_host(
        &self,
        host: &ValidatedTypeScriptHost<'_>,
    ) -> Result<(), TypeScriptProjectHostError> {
        self.validate_with_host_controlled(host, &mut || Ok(()))
    }

    fn validate_with_host_controlled(
        &self,
        host: &ValidatedTypeScriptHost<'_>,
        control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
    ) -> Result<(), TypeScriptProjectHostError> {
        control()?;
        if compiler_access_digest(&self.compiler_accesses, &self.directory_views, self.metrics)?
            != self.access_digest
        {
            return Err(bridge_error(
                "retained Compiler API directory transcript changed after construction",
            ));
        }
        control()?;
        self.resolver.validate_with_host_controlled(host, control)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramReport {
    schema: u32,
    version: String,
    config_path: String,
    compiler_options: serde_json::Value,
    #[serde(default)]
    project_references: Vec<serde_json::Value>,
    root_names: Vec<String>,
    files: Vec<ProgramFile>,
    resolutions: Vec<ProgramResolution>,
    #[serde(default)]
    accesses: Vec<ProgramAccess>,
    #[serde(default)]
    directory_views: Vec<ProgramDirectoryView>,
    #[serde(default)]
    access_operations: usize,
    #[serde(default)]
    directory_queries: usize,
    #[serde(default)]
    directory_enumerations: usize,
    #[serde(default)]
    directory_visited_entries: usize,
    #[serde(default)]
    directory_verification_entries: usize,
    #[serde(default)]
    matcher_result_entries: usize,
    #[serde(default)]
    matcher_pattern_items: usize,
    #[serde(default)]
    matcher_pattern_bytes: usize,
    #[serde(default)]
    directory_query_cache_bytes: usize,
    #[serde(default)]
    unsupported_options: Vec<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    failure_kind: Option<String>,
    #[serde(default)]
    missing_compiler_api_export: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramFile {
    path: String,
    sha256: String,
    default_library: bool,
    implied_node_format: TszProjectFileFormat,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramResolution {
    importer: String,
    specifier: String,
    kind: String,
    mode: Option<String>,
    target_path: Option<String>,
    package_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ProgramAccess {
    kind: ProgramAccessKind,
    path: String,
    #[serde(default)]
    exists: Option<bool>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    realpath: Option<String>,
    #[serde(default)]
    extensions: Option<Vec<String>>,
    #[serde(default)]
    excludes: Option<Vec<String>>,
    #[serde(default)]
    includes: Option<Vec<String>>,
    #[serde(default)]
    recursive: Option<bool>,
    #[serde(default)]
    depth: Option<usize>,
    #[serde(default)]
    current_directory: Option<String>,
    #[serde(default)]
    use_case_sensitive_file_names: Option<bool>,
    #[serde(default)]
    directory_view_paths: Vec<String>,
    #[serde(default)]
    directory_realpaths: Vec<ProgramPathResolution>,
    #[serde(default)]
    matcher_arguments: Vec<ProgramMatcherArgument>,
    /// SHA-256 of the complete selected-TypeScript `matchFiles` result over the exact captured
    /// directory snapshots. The bridge also compares the full returned arrays before emitting
    /// this receipt; Rust recomputes it from `entries` so the equality witness stays bound to the
    /// retained resolver closure without duplicating every path in the report.
    #[serde(default)]
    matcher_replay_sha256: Option<Box<str>>,
    #[serde(default)]
    entries: Option<Vec<String>>,
}

// Conservative heap accounting, independent of the resolver's deduplicated
// paths and the bridge's separate serialized-report/count bounds. Inline
// elements are charged by their owning collection, including spare capacity.
const RETAINED_ALLOCATION_OVERHEAD: usize = 32;

fn retained_allocation_bytes(bytes: usize) -> usize {
    bytes.saturating_add(if bytes == 0 {
        0
    } else {
        RETAINED_ALLOCATION_OVERHEAD
    })
}

fn retained_string_bytes(value: &String) -> usize {
    retained_allocation_bytes(value.capacity())
}

fn retained_vec_bytes<T>(values: &Vec<T>, nested: impl Fn(&T) -> usize) -> usize {
    values.iter().fold(
        retained_allocation_bytes(values.capacity().saturating_mul(std::mem::size_of::<T>())),
        |bytes, value| bytes.saturating_add(nested(value)),
    )
}

fn retained_slice_bytes<T>(values: &[T], nested: impl Fn(&T) -> usize) -> usize {
    values.iter().fold(
        retained_allocation_bytes(values.len().saturating_mul(std::mem::size_of::<T>())),
        |bytes, value| bytes.saturating_add(nested(value)),
    )
}

fn retained_json_bytes(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) => 0,
        // Also covers a serde_json build with arbitrary-precision number storage.
        serde_json::Value::Number(number) => retained_allocation_bytes(number.to_string().len()),
        serde_json::Value::String(text) => retained_string_bytes(text),
        serde_json::Value::Array(values) => retained_vec_bytes(values, retained_json_bytes),
        serde_json::Value::Object(values) => values.iter().fold(0usize, |bytes, (key, value)| {
            // serde_json hides its map allocation layout. A conservative charge
            // per entry covers tree/hash nodes, inline keys/values and slack.
            bytes
                .saturating_add(1024)
                .saturating_add(retained_string_bytes(key))
                .saturating_add(retained_json_bytes(value))
        }),
    }
}

impl ProgramAccess {
    fn retained_bytes(&self) -> usize {
        let mut bytes = retained_string_bytes(&self.path);
        for text in [&self.sha256, &self.realpath, &self.current_directory]
            .into_iter()
            .flatten()
        {
            bytes = bytes.saturating_add(retained_string_bytes(text));
        }
        for values in [
            &self.extensions,
            &self.excludes,
            &self.includes,
            &self.entries,
        ]
        .into_iter()
        .flatten()
        {
            bytes = bytes.saturating_add(retained_vec_bytes(values, retained_string_bytes));
        }
        bytes = bytes.saturating_add(retained_vec_bytes(
            &self.directory_view_paths,
            retained_string_bytes,
        ));
        bytes = bytes.saturating_add(retained_vec_bytes(
            &self.directory_realpaths,
            |resolution| {
                retained_string_bytes(&resolution.path)
                    .saturating_add(retained_string_bytes(&resolution.target))
            },
        ));
        bytes = bytes.saturating_add(retained_vec_bytes(&self.matcher_arguments, |argument| {
            match argument {
                ProgramMatcherArgument::Value(value) => retained_json_bytes(value),
                ProgramMatcherArgument::Undefined | ProgramMatcherArgument::Null => 0,
            }
        }));
        bytes.saturating_add(
            self.matcher_replay_sha256
                .as_ref()
                .map_or(0, |text| retained_allocation_bytes(text.len())),
        )
    }
}

/// Closed Compiler API filesystem operations retained in the authority ledger.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
enum ProgramAccessKind {
    File,
    Exists,
    Directory,
    Realpath,
    ReadDirectory,
    GetDirectories,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ProgramDirectoryView {
    path: String,
    files: Vec<String>,
    directories: Vec<String>,
    read_succeeded: bool,
}

impl ProgramDirectoryView {
    fn retained_bytes(&self) -> usize {
        retained_string_bytes(&self.path)
            .saturating_add(retained_vec_bytes(&self.files, retained_string_bytes))
            .saturating_add(retained_vec_bytes(&self.directories, retained_string_bytes))
    }
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
struct ProgramPathResolution {
    path: String,
    target: String,
    succeeded: bool,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum ProgramMatcherArgument {
    Undefined,
    Null,
    Value(serde_json::Value),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ProgramClosureMetrics {
    access_operations: usize,
    directory_queries: usize,
    directory_enumerations: usize,
    directory_visited_entries: usize,
    directory_verification_entries: usize,
    matcher_result_entries: usize,
    matcher_pattern_items: usize,
    matcher_pattern_bytes: usize,
    directory_query_cache_bytes: usize,
}

impl ProgramReport {
    fn closure_metrics(&self) -> ProgramClosureMetrics {
        ProgramClosureMetrics {
            access_operations: self.access_operations,
            directory_queries: self.directory_queries,
            directory_enumerations: self.directory_enumerations,
            directory_visited_entries: self.directory_visited_entries,
            directory_verification_entries: self.directory_verification_entries,
            matcher_result_entries: self.matcher_result_entries,
            matcher_pattern_items: self.matcher_pattern_items,
            matcher_pattern_bytes: self.matcher_pattern_bytes,
            directory_query_cache_bytes: self.directory_query_cache_bytes,
        }
    }
}

/// One script executed by the exact admitted TypeScript compiler API.
const COMPILER_API_PROGRAM_SCRIPT: &str = r#"
'use strict';
const path = require('path');
const crypto = require('crypto');
const fs = require('fs');
const compilerApiPath = process.argv[1];
const configPath = process.argv[2];
const workspaceRoot = process.argv[3];
const inferredRootNames = JSON.parse(process.argv[4] || "null");
const admitProjectForest = process.argv[5] === 'forest';
const remainingForestLimits = JSON.parse(process.argv[6] || '{}');
const admittedLimit = (name, maximum) => {
  const value = remainingForestLimits[name] === undefined ? maximum : remainingForestLimits[name];
  if (!Number.isSafeInteger(value) || value < 0 || value > maximum) throw new Error('invalid trusted forest allowance');
  return value;
};
const observations = new Map();
const directoryViews = new Map();
const directoryEntryCache = new Map();
const directoryQueryCache = new Map();
const MAX_PROGRAM_FILES = admittedLimit('files', 16384);
const MAX_PROGRAM_REQUESTS = admittedLimit('requests', 131072);
const MAX_ACCESS_OPERATIONS = admittedLimit('access_operations', 131072);
const MAX_DIRECTORY_QUERIES = admittedLimit('directory_queries', 16384);
const MAX_DIRECTORY_ENUMERATIONS = admittedLimit('directory_enumerations', 65536);
const MAX_DIRECTORY_ENTRIES = admittedLimit('directory_entries', 65536);
const MAX_ENTRIES_PER_DIRECTORY = 16384;
const MAX_MATCHER_RESULT_ENTRIES = 65536;
const MAX_MATCHER_RESULT_WORK = admittedLimit('matcher_result_entries', 196608);
const MAX_MATCHER_PATTERN_ITEMS = admittedLimit('matcher_pattern_items', 131072);
const MAX_MATCHER_PATTERN_BYTES = admittedLimit('matcher_pattern_bytes', 512 * 1024);
const MAX_DIRECTORY_QUERY_CACHE_BYTES = admittedLimit('directory_query_cache_bytes', 1024 * 1024);
let accessOperations = 0;
let directoryQueries = 0;
let directoryEnumerations = 0;
let directoryVisitedEntries = 0;
let directoryVerificationEntries = 0;
let matcherResultEntries = 0;
let matcherPatternItems = 0;
let matcherPatternBytes = 0;
let directoryQueryCacheBytes = 0;
let activeDirectoryQuery = null;
const closureFault = (kind, message) => {
  const error = new Error(message);
  error.closureFailureKind = kind;
  return error;
};
const matcherEnvironment = sys => {
  if (typeof sys.getCurrentDirectory !== 'function') {
    throw closureFault('mismatch', 'TypeScript system omitted its exact current-directory function');
  }
  const currentDirectory = sys.getCurrentDirectory();
  const caseSensitiveValue = typeof sys.useCaseSensitiveFileNames === 'function'
    ? sys.useCaseSensitiveFileNames() : sys.useCaseSensitiveFileNames;
  if (typeof currentDirectory !== 'string' || typeof caseSensitiveValue !== 'boolean') {
    throw closureFault('mismatch', 'TypeScript system exposed unsupported matcher environment values');
  }
  return {currentDirectory:path.resolve(currentDirectory), caseSensitive:caseSensitiveValue};
};
// Paths outside the admitted roots (the workspace and the TypeScript module
// root) are not part of the program universe. Probing them cannot be witnessed
// by the host, so the bridge answers "absent" without touching the disk and
// records nothing (for example Vite's rooted `import logo from '/vite.svg'`
// makes the compiler probe the filesystem root). The ancestor `node_modules`
// and `node_modules/@types` negative candidates stay observed because the host
// witnesses them by parent identity. This mirrors the host's
// path_is_admitted / external_negative_module_candidate; MAX_PROJECT_ANCESTORS
// is pinned to the host constant by a unit test.
const MAX_PROJECT_ANCESTORS = 32;
const admittedRoots = [path.resolve(workspaceRoot), path.resolve(compilerApiPath, '..', '..', '..')];
const insideRoot = (candidate, root) =>
  candidate === root || candidate.startsWith(root.endsWith(path.sep) ? root : root + path.sep);
const negativeModuleCandidates = new Set();
{
  let ancestor = path.dirname(path.resolve(workspaceRoot));
  for (let count = 0; count < MAX_PROJECT_ANCESTORS; count++) {
    negativeModuleCandidates.add(path.join(ancestor, 'node_modules'));
    negativeModuleCandidates.add(path.join(ancestor, 'node_modules', '@types'));
    const parent = path.dirname(ancestor);
    if (parent === ancestor) break;
    ancestor = parent;
  }
}
const outsideProgramUniverse = resolved =>
  !admittedRoots.some(root => insideRoot(resolved, root)) && !negativeModuleCandidates.has(resolved);
const chargeAccess = () => {
  if (accessOperations >= MAX_ACCESS_OPERATIONS) {
    throw closureFault('limit', 'TypeScript compiler filesystem access count exceeded its bound');
  }
  accessOperations++;
};
const makeDirectoryView = directory => {
  const key = path.resolve(directory || '.');
  let view = activeDirectoryQuery.views.get(key);
  if (!view) {
    view = {path:key, files:new Set(), directories:new Set(), symlinks:new Map(), readSucceeded:true};
    activeDirectoryQuery.views.set(key, view);
  }
  return view;
};
const recordDirectoryStat = (absolutePath, stat) => {
  if (!activeDirectoryQuery) return;
  const normalized = path.resolve(absolutePath);
  const pair = activeDirectoryQuery.symlinks.get(normalized);
  if (!pair) return;
  const {view, name} = pair;
  const kind = !stat ? null : stat.isFile() ? 'file' : stat.isDirectory() ? 'directory' : 'other';
  activeDirectoryQuery.symlinkStats.set(normalized, kind);
  if (kind !== null) directorySymlinkStats.set(normalized, kind);
  if (kind === 'file') view.files.add(name);
  else if (kind === 'directory') view.directories.add(name);
};
const cachedStat = kind => kind === null ? undefined : ({
  isFile:() => kind === 'file',
  isDirectory:() => kind === 'directory'
});
const originalStatSync = fs.statSync;
fs.statSync = function(file, ...args) {
  const normalized = path.resolve(file);
  if (activeDirectoryQuery && activeDirectoryQuery.symlinks.has(normalized)
      && activeDirectoryQuery.symlinkStats.has(normalized)) {
    const cached = cachedStat(activeDirectoryQuery.symlinkStats.get(normalized));
    if (cached) return cached;
  }
  if (activeDirectoryQuery && directorySymlinkStats.has(normalized)) {
    const kind = directorySymlinkStats.get(normalized);
    const cached = cachedStat(kind);
    if (cached) {
      activeDirectoryQuery.symlinkStats.set(normalized, kind);
      recordDirectoryStat(normalized, cached);
      return cached;
    }
  }
  try {
    const stat = originalStatSync.call(this, file, ...args);
    recordDirectoryStat(file, stat);
    return stat;
  } catch (error) {
    recordDirectoryStat(file, null);
    throw error;
  }
};
const wrapRealpath = original => function(file, ...args) {
  const lexical = path.resolve(file);
  if (activeDirectoryQuery && directoryRealpathCache.has(lexical)) {
    const cached = directoryRealpathCache.get(lexical);
    activeDirectoryQuery.realpaths.set(lexical, cached);
    if (!cached.succeeded) {
      const error = new Error('no such file or directory');
      error.code = 'ENOENT';
      error.path = lexical;
      error.syscall = 'realpath';
      throw error;
    }
    return cached.target;
  }
  try {
    const value = original.call(this, file, ...args);
    if (activeDirectoryQuery) {
      const captured = {target:path.resolve(value), succeeded:true};
      activeDirectoryQuery.realpaths.set(lexical, captured);
      directoryRealpathCache.set(lexical, captured);
    }
    return value;
  } catch (error) {
    if (activeDirectoryQuery) {
      const captured = {target:lexical, succeeded:false};
      activeDirectoryQuery.realpaths.set(lexical, captured);
      directoryRealpathCache.set(lexical, captured);
    }
    throw error;
  }
};
const directoryRealpathCache = new Map();
const directorySymlinkStats = new Map();
const originalRealpathSync = fs.realpathSync;
const originalNativeRealpathSync = fs.realpathSync.native;
fs.realpathSync = wrapRealpath(originalRealpathSync);
if (typeof originalNativeRealpathSync === 'function') {
  fs.realpathSync.native = wrapRealpath(originalNativeRealpathSync);
}
const originalOpendirSync = fs.opendirSync;
const originalReaddirSync = fs.readdirSync;
const recordDirent = (view, directory, entry) => {
  const name = entry.name;
  if (entry.isFile()) view.files.add(name);
  else if (entry.isDirectory()) view.directories.add(name);
  else if (entry.isSymbolicLink()) {
    activeDirectoryQuery.symlinks.set(path.resolve(directory, name), {view, name});
  }
};
fs.readdirSync = function(directory, options) {
  if (!activeDirectoryQuery) return originalReaddirSync.call(this, directory, options);
  const view = makeDirectoryView(directory);
  if (!options || options.withFileTypes !== true || typeof originalOpendirSync !== 'function') {
    view.readSucceeded = false;
    activeDirectoryQuery.fault = closureFault('mismatch', 'TypeScript directory enumeration could not be captured exactly');
    return originalReaddirSync.call(this, directory, options);
  }
  const cached = directoryEntryCache.get(view.path);
  if (cached) {
    if (directoryVisitedEntries + cached.length > MAX_DIRECTORY_ENTRIES) {
      activeDirectoryQuery.fault = closureFault('limit', 'TypeScript visited-directory-entry work exceeded its bound');
      return [];
    }
    directoryVisitedEntries += cached.length;
    for (const entry of cached) recordDirent(view, view.path, entry);
    return cached.slice();
  }
  if (directoryEnumerations >= MAX_DIRECTORY_ENUMERATIONS) {
    activeDirectoryQuery.fault = closureFault('limit', 'TypeScript directory enumeration count exceeded its bound');
    return [];
  }
  directoryEnumerations++;
  let handle;
    const entries = [];
  try {
    handle = originalOpendirSync.call(this, directory);
    while (true) {
      if (entries.length >= MAX_ENTRIES_PER_DIRECTORY) {
        activeDirectoryQuery.fault = closureFault('limit', 'one TypeScript directory reached its entry bound before completeness was proven');
        break;
      }
      if (directoryVisitedEntries >= MAX_DIRECTORY_ENTRIES) {
        activeDirectoryQuery.fault = closureFault('limit', 'TypeScript visited-directory-entry work reached its bound before completeness was proven');
        break;
      }
      const entry = handle.readSync();
      if (!entry) break;
      directoryVisitedEntries++;
      entries.push(entry);
      recordDirent(view, view.path, entry);
    }
    if (!activeDirectoryQuery.fault) directoryEntryCache.set(view.path, entries.slice());
    return entries;
  } catch (error) {
    view.readSucceeded = false;
    throw error;
  } finally {
    if (handle) {
      try { handle.closeSync(); } catch {}
    }
  }
};
const key = (kind, p, extra) => JSON.stringify([kind, path.resolve(p), extra || null]);
const digest = value => crypto.createHash('sha256').update(Buffer.isBuffer(value) ? value : Buffer.from(value)).digest('hex');
const resolvedEntries = (value, root) => Array.isArray(value) ? value.map(item =>
  path.isAbsolute(item) ? path.resolve(item) : path.resolve(root, item)).sort() : [];
const samePathList = (left, right) => left.length === right.length && left.every((value, index) => value === right[index]);
const encodeMatcherArgument = value => value === undefined ? {kind:'undefined'}
  : value === null ? {kind:'null'} : {kind:'value', value};
const queryCacheRetainedBytes = (key, entries, viewPaths, realpaths) => {
  let bytes = Buffer.byteLength(key, 'utf8') + 128;
  for (const value of entries) bytes += Buffer.byteLength(value, 'utf8') + 8;
  for (const value of viewPaths) bytes += Buffer.byteLength(value, 'utf8') + 8;
  for (const value of realpaths) {
    bytes += Buffer.byteLength(value.path, 'utf8') + Buffer.byteLength(value.target, 'utf8') + 24;
  }
  return bytes;
};
const digestPathList = entries => {
  const hash = crypto.createHash('sha256');
  hash.update('compiler.typescript.matchFiles.result.v1|');
  for (const entry of entries) {
    const bytes = Buffer.from(entry, 'utf8');
    const length = Buffer.alloc(8);
    length.writeBigUInt64LE(BigInt(bytes.length));
    hash.update(length);
    hash.update(bytes);
  }
  return hash.digest('hex');
};
const add = value => { const k = JSON.stringify(value); observations.set(k, value); };
const result = value => { process.stdout.write(JSON.stringify(value)); };
try {
  const ts = require(compilerApiPath);
  const compilerApiCapabilityFault = name => {
    const error = closureFault('capability', 'selected TypeScript runtime does not expose required Compiler API export ' + name);
    error.missingCompilerApiExport = name;
    return error;
  };
  if (typeof ts.getModeForUsageLocation !== 'function') throw compilerApiCapabilityFault('getModeForUsageLocation');
  for (const name of ['getEmitScriptTarget', 'getEmitModuleKind', 'getEmitModuleResolutionKind', 'getEmitModuleDetectionKind', 'getESModuleInterop', 'getAllowSyntheticDefaultImports']) {
    if (typeof ts[name] !== 'function') throw compilerApiCapabilityFault(name);
  }
  if (typeof ts.matchFiles !== 'function') throw compilerApiCapabilityFault('matchFiles');
  const finishDirectoryQuery = (context, queryPath, value, kind, args, environment) => {
    for (const view of context.views.values()) {
      const files = Array.from(view.files).sort();
      const directories = Array.from(view.directories).sort();
      const encoded = {path:view.path, files, directories, read_succeeded:view.readSucceeded};
      const previous = directoryViews.get(view.path);
      if (previous && JSON.stringify(previous) !== JSON.stringify(encoded)) {
        throw closureFault('mismatch', 'a TypeScript directory enumeration changed during program construction');
      }
      directoryViews.set(view.path, encoded);
    }
    if (context.fault) throw context.fault;
    const queryViews = new Map(Array.from(context.views, ([viewPath, view]) => [viewPath, {
      files:Array.from(view.files).sort(), directories:Array.from(view.directories).sort()
    }]));
    const viewPaths = Array.from(queryViews.keys()).sort();
    if (!Array.isArray(value) || value.length > MAX_MATCHER_RESULT_ENTRIES) {
      throw closureFault('limit', 'TypeScript directory-query result exceeded its entry bound');
    }
    const resultWork = value.length * 3;
    if (matcherResultEntries + resultWork > MAX_MATCHER_RESULT_WORK) {
      throw closureFault('limit', 'TypeScript directory-query result verification exceeded its work bound');
    }
    matcherResultEntries += resultWork;
    let replayEntries;
    if (kind === 'readDirectory') {
      let verifiedEntries = 0;
      const verified = ts.matchFiles(queryPath, args[1], args[2], args[3], environment.caseSensitive,
        environment.currentDirectory, args[4], directory => {
          const view = queryViews.get(path.resolve(directory));
          if (!view) throw closureFault('mismatch', 'TypeScript matchFiles traversed an uncaptured directory');
          verifiedEntries += view.files.length + view.directories.length;
          if (directoryVerificationEntries + verifiedEntries > MAX_DIRECTORY_ENTRIES) {
            throw closureFault('limit', 'TypeScript directory replay work exceeded its bound');
          }
          return view;
        }, absolutePath => {
          const canonical = context.realpaths.get(path.resolve(absolutePath));
          if (!canonical) throw closureFault('mismatch', 'TypeScript matchFiles used an uncaptured realpath');
          return canonical.target;
        });
      directoryVerificationEntries += verifiedEntries;
      const expected = resolvedEntries(value, path.resolve(queryPath));
      const replayed = resolvedEntries(verified, path.resolve(queryPath));
      if (!samePathList(expected, replayed)) {
        throw closureFault('mismatch', 'TypeScript readDirectory result differs from exact compiler-semantic replay');
      }
      replayEntries = replayed;
    } else {
      const rootView = queryViews.get(path.resolve(queryPath));
      if (!rootView) throw closureFault('mismatch', 'TypeScript getDirectories omitted its root enumeration');
      const expected = resolvedEntries(value, path.resolve(queryPath));
      const replayed = rootView.directories.map(name => path.resolve(queryPath, name)).sort();
      if (!samePathList(expected, replayed)) {
        throw closureFault('mismatch', 'TypeScript getDirectories result differs from exact compiler-semantic replay');
      }
      replayEntries = replayed;
    }
    const realpaths = Array.from(context.realpaths.entries(), ([realpath, observed]) => ({
      path:realpath, target:observed.target, succeeded:observed.succeeded
    })).sort((left, right) => left.path < right.path ? -1 : left.path > right.path ? 1 : 0);
    return {viewPaths, realpaths, matcherReplaySha256:digestPathList(replayEntries)};
  };
  const wrapSystem = sys => {
    for (const name of ['readFile', 'fileExists', 'directoryExists', 'realpath', 'readDirectory', 'getDirectories']) {
      if (typeof sys[name] !== 'function') continue;
      const original = sys[name].bind(sys);
      sys[name] = (...args) => {
        chargeAccess();
        const p = args[0];
        const resolvedPath = path.resolve(p);
        if ((name === 'readFile' || name === 'fileExists' || name === 'directoryExists' || name === 'realpath')
            && outsideProgramUniverse(resolvedPath)) {
          return name === 'realpath' ? p : name === 'readFile' ? undefined : false;
        }
        let value;
        let directoryViewPaths = [];
        let directoryRealpaths = [];
        let matcherReplaySha256 = null;
        let directoryMatcherEnvironment = null;
        if (name === 'readDirectory' || name === 'getDirectories') {
          if (name === 'readDirectory' && args.length > 5) {
            throw closureFault('mismatch', 'TypeScript readDirectory supplied unsupported trailing arguments');
          }
          if (name === 'getDirectories' && args.length > 1) {
            throw closureFault('mismatch', 'TypeScript getDirectories supplied unsupported trailing arguments');
          }
          if (name === 'readDirectory' && args[4] !== undefined && args[4] !== null
              && (!Number.isSafeInteger(args[4]) || args[4] < 0)) {
            throw closureFault('mismatch', 'TypeScript readDirectory supplied a non-canonical depth value');
          }
          directoryMatcherEnvironment = matcherEnvironment(sys);
          if (directoryQueries >= MAX_DIRECTORY_QUERIES) {
            throw closureFault('limit', 'TypeScript directory-query count exceeded its bound');
          }
          directoryQueries++;
          const matcherItems = args.slice(1, 4);
          let queryPatternItems = 0;
          let queryPatternBytes = 0;
          for (const patterns of matcherItems) {
            if (patterns === undefined || patterns === null) continue;
            if (!Array.isArray(patterns)) {
              throw closureFault('mismatch', 'TypeScript directory matcher supplied a non-array pattern set');
            }
            if (queryPatternItems + patterns.length > MAX_MATCHER_PATTERN_ITEMS) {
              throw closureFault('limit', 'one TypeScript directory matcher exceeded its pattern-item bound');
            }
            queryPatternItems += patterns.length;
            for (const pattern of patterns) {
              if (typeof pattern !== 'string') {
                throw closureFault('mismatch', 'TypeScript directory matcher supplied a non-string pattern');
              }
              queryPatternBytes += Buffer.byteLength(pattern, 'utf8');
              if (queryPatternBytes > MAX_MATCHER_PATTERN_BYTES) {
                throw closureFault('limit', 'one TypeScript directory matcher exceeded its pattern-byte bound');
              }
            }
          }
          if (matcherPatternItems + queryPatternItems > MAX_MATCHER_PATTERN_ITEMS
              || matcherPatternBytes + queryPatternBytes > MAX_MATCHER_PATTERN_BYTES) {
            throw closureFault('limit', 'TypeScript directory matcher pattern work exceeded its project bound');
          }
          matcherPatternItems += queryPatternItems;
          matcherPatternBytes += queryPatternBytes;
          const cacheArg = arg => arg === undefined ? ['omitted'] : ['value', arg];
          const queryCacheKey = JSON.stringify([
            name, resolvedPath, directoryMatcherEnvironment.currentDirectory,
            directoryMatcherEnvironment.caseSensitive,
            ...args.slice(1).map(cacheArg)
          ]);
          const cached = directoryQueryCache.get(queryCacheKey);
          if (cached) {
            const cachedResultWork = cached.entries.length * 2;
            if (cached.entries.length > MAX_MATCHER_RESULT_ENTRIES
                || matcherResultEntries + cachedResultWork > MAX_MATCHER_RESULT_WORK) {
              throw closureFault('limit', 'cached TypeScript directory-query result exceeded its work bound');
            }
            matcherResultEntries += cachedResultWork;
            value = cached.entries.slice();
            directoryViewPaths = cached.viewPaths.slice();
            directoryRealpaths = cached.realpaths.map(item => ({...item}));
            matcherReplaySha256 = cached.matcherReplaySha256;
          } else {
            const context = {views:new Map(), realpaths:new Map(), symlinks:new Map(), symlinkStats:new Map(), fault:null};
            const previous = activeDirectoryQuery;
            activeDirectoryQuery = context;
            try {
              value = original(...args);
            } finally {
              activeDirectoryQuery = previous;
            }
            const captured = finishDirectoryQuery(context, p, value, name, args, directoryMatcherEnvironment);
            directoryViewPaths = captured.viewPaths;
            directoryRealpaths = captured.realpaths;
            matcherReplaySha256 = captured.matcherReplaySha256;
            const retainedBytes = queryCacheRetainedBytes(
              queryCacheKey, value, directoryViewPaths, directoryRealpaths);
            if (directoryQueryCacheBytes + retainedBytes > MAX_DIRECTORY_QUERY_CACHE_BYTES) {
              throw closureFault('limit', 'TypeScript exact directory-query cache exceeded its retained-byte bound');
            }
            directoryQueryCacheBytes += retainedBytes;
            directoryQueryCache.set(queryCacheKey, {
              entries:Array.isArray(value) ? value.slice() : [],
              viewPaths:directoryViewPaths.slice(),
              realpaths:directoryRealpaths.map(item => ({...item})),
              matcherReplaySha256
            });
          }
        } else {
          value = original(...args);
        }
        if (name === 'readFile') {
          add({kind:'file', path:resolvedPath, exists:value !== undefined, sha256:value === undefined ? null : digest(value)});
        } else if (name === 'fileExists') {
          add({kind:'exists', path:resolvedPath, exists:!!value});
        } else if (name === 'directoryExists') {
          add({kind:'directory', path:resolvedPath, exists:!!value});
        } else if (name === 'realpath') {
          add({kind:'realpath', path:resolvedPath, realpath:value === undefined ? null : path.resolve(value)});
        } else if (name === 'readDirectory') {
          add({kind:'readDirectory', path:resolvedPath, extensions:args[1] === undefined ? null : args[1], excludes:args[2] === undefined ? null : args[2], includes:args[3] === undefined ? null : args[3],
            recursive:args[4] === undefined, depth:args[4] === undefined ? null : args[4], current_directory:directoryMatcherEnvironment.currentDirectory,
            use_case_sensitive_file_names:directoryMatcherEnvironment.caseSensitive, directory_view_paths:directoryViewPaths, directory_realpaths:directoryRealpaths,
            matcher_arguments:args.slice(1).map(encodeMatcherArgument),
            matcher_replay_sha256:matcherReplaySha256,
            entries:resolvedEntries(value, resolvedPath)});
        } else if (name === 'getDirectories') {
          add({kind:'getDirectories', path:resolvedPath, directory_view_paths:directoryViewPaths, directory_realpaths:directoryRealpaths,
            matcher_arguments:args.slice(1).map(encodeMatcherArgument),
            matcher_replay_sha256:matcherReplaySha256,
            entries:resolvedEntries(value, resolvedPath)});
        }
        return value;
      };
    }
    return sys;
  };
  wrapSystem(ts.sys);
  const configDiagnostics = [];
  const parseHost = Object.assign({}, ts.sys, {
    onUnRecoverableConfigFileDiagnostic: diagnostic => configDiagnostics.push(diagnostic)
  });
  const parsed = configPath
    ? ts.getParsedCommandLineOfConfigFile(configPath, {}, parseHost)
    : {options:ts.getDefaultCompilerOptions(), fileNames:inferredRootNames, errors:[], projectReferences:[]};
  if (!parsed) throw new Error('getParsedCommandLineOfConfigFile returned no project');
  if (configDiagnostics.length || (parsed.errors || []).length) {
    // Name the first compiler diagnostics (code and text) so a refusal is actionable.
    const described = configDiagnostics.concat(parsed.errors || []).slice(0, 2)
      .map(item => 'TS' + item.code + ': ' + ts.flattenDiagnosticMessageText(item.messageText, ' ')).join('; ');
    throw new Error('selected config has Compiler API parse diagnostics: ' + described);
  }
  if (!admitProjectForest && parsed.projectReferences && parsed.projectReferences.length) {
    result({schema:1, version:ts.version, config_path:configPath ? path.resolve(configPath) : "", compiler_options:{compilerOptions:{}},
      root_names:parsed.fileNames.map(name => path.resolve(name)),
      project_references:parsed.projectReferences.map(r => r.path), files:[], resolutions:[], accesses:Array.from(observations.values()),
      directory_views:Array.from(directoryViews.values()), access_operations:accessOperations, directory_queries:directoryQueries,
      directory_enumerations:directoryEnumerations, directory_visited_entries:directoryVisitedEntries, directory_verification_entries:directoryVerificationEntries,
      matcher_result_entries:matcherResultEntries,
      matcher_pattern_items:matcherPatternItems, matcher_pattern_bytes:matcherPatternBytes,
      directory_query_cache_bytes:directoryQueryCacheBytes,
      unsupported_options:[], error:'project references are not yet admitted as one TSZ program', failure_kind:null});
  } else {
    // With no tsconfig, the package's captured source frontier is the complete
    // project input. TypeScript defaults `allowJs` to false and silently omits
    // inferred JavaScript roots unless the host opts them in. Clone the defaults
    // so this narrow inference does not mutate explicit project options or
    // accidentally enable JavaScript checking.
    const options = {...parsed.options};
    const inferredJavaScriptRoots = !configPath && Array.isArray(inferredRootNames)
      && inferredRootNames.some(name => /\.(?:js|jsx|mjs|cjs)$/i.test(name));
    if (inferredJavaScriptRoots) options.allowJs = true;
    const host = ts.createCompilerHost(options, true);
    for (const name of ['readFile', 'fileExists', 'directoryExists', 'realpath', 'readDirectory', 'getDirectories']) {
      if (typeof host[name] === 'function' && typeof ts.sys[name] === 'function') host[name] = ts.sys[name].bind(ts.sys);
    }
    const program = ts.createProgram({rootNames:parsed.fileNames, options, host,
      projectReferences:admitProjectForest ? parsed.projectReferences : undefined});
    const programFiles = program.getSourceFiles();
    if (programFiles.length > MAX_PROGRAM_FILES) throw closureFault('limit','forest source count');
    const byPath = new Map(programFiles.map(file => [path.resolve(file.fileName), file]));
    const sourceFiles = programFiles.map(file => ({
      path:path.resolve(file.fileName),
      sha256:digest(file.text),
      default_library:program.isSourceFileDefaultLibrary(file),
      implied_node_format:file.impliedNodeFormat === undefined ? 'unknown'
        : file.impliedNodeFormat === ts.ModuleKind.CommonJS ? 'common_js'
        : file.impliedNodeFormat === ts.ModuleKind.ESNext ? 'es_module'
        : (() => { throw closureFault('mismatch', 'unsupported SourceFile.impliedNodeFormat'); })()
    }));
    const resolutions = [];
    const resolutionKeys = new Map();
    const modeFor = (file, literal) => {
      // The owning compiler interprets defaults, import type attributes,
      // require syntax, and dynamic imports for this exact AST usage.
      const mode = ts.getModeForUsageLocation(file, literal, options);
      if (mode === undefined) return null;
      if (mode === ts.ModuleKind.CommonJS) return 'require';
      if (mode === ts.ModuleKind.ESNext) return 'import';
      throw new Error('selected TypeScript runtime returned an unsupported exact request resolution mode');
    };
    const addRequest = (file, literal, kind) => {
      if (!literal || typeof literal.text !== 'string') return;
      const resolution = program.getResolvedModuleFromModuleSpecifier(literal, file);
      const resolved = resolution && resolution.resolvedModule;
      const mode = modeFor(file, literal);
      const value = {importer:path.resolve(file.fileName), specifier:literal.text, kind, mode,
        target_path:resolved ? path.resolve(resolved.resolvedFileName) : null,
        package_id:resolved && resolved.packageId ? [resolved.packageId.name, resolved.packageId.version, resolved.packageId.subModuleName || ''].join('/') : null};
      const requestKey = JSON.stringify([value.importer, value.specifier, value.kind, value.mode]);
      const encoded = JSON.stringify(value);
      if (resolutionKeys.has(requestKey) && resolutionKeys.get(requestKey) !== encoded) throw new Error('conflicting compiler resolutions for one request key');
      if (!resolutionKeys.has(requestKey)) {
        if (resolutions.length >= MAX_PROGRAM_REQUESTS) throw closureFault('limit','forest request count');
        resolutionKeys.set(requestKey, encoded); resolutions.push(value);
      }
    };
    const visit = (file, node) => {
      if (ts.isImportDeclaration(node) && ts.isStringLiteralLike(node.moduleSpecifier)) addRequest(file,node.moduleSpecifier,'EsmImport');
      else if (ts.isExportDeclaration(node) && node.moduleSpecifier && ts.isStringLiteralLike(node.moduleSpecifier)) addRequest(file,node.moduleSpecifier,'EsmReExport');
      else if (ts.isImportTypeNode(node) && ts.isLiteralTypeNode(node.argument) && ts.isStringLiteralLike(node.argument.literal)) addRequest(file,node.argument.literal,'EsmImport');
      else if (ts.isImportEqualsDeclaration(node) && ts.isExternalModuleReference(node.moduleReference) && node.moduleReference.expression && ts.isStringLiteralLike(node.moduleReference.expression)) addRequest(file,node.moduleReference.expression,'CjsRequire');
      else if (ts.isCallExpression(node) && node.arguments.length && ts.isStringLiteralLike(node.arguments[0])) {
        if (node.expression.kind === ts.SyntaxKind.ImportKeyword) addRequest(file,node.arguments[0],'DynamicImport');
        else if (ts.isIdentifier(node.expression) && node.expression.text === 'require') addRequest(file,node.arguments[0],'CjsRequire');
      }
      ts.forEachChild(node, child => visit(file, child));
    };
    for (const file of programFiles) visit(file,file);
    const reverseName = (table, value, fallback) => {
      if (value === undefined || value === null) return fallback;
      const name = table[value];
      if (typeof name !== 'string') throw new Error('compiler option enum has no canonical name');
      return name;
    };
    const computedEnumName = (table, value) => {
      if (typeof value !== 'number' || typeof table[value] !== 'string') throw new Error('selected TypeScript computed option returned an unsupported enum value');
      return table[value];
    };
    const computedFlag = name => {
      const value = ts[name](options);
      if (typeof value !== 'boolean') throw new Error('selected TypeScript computed option returned an unsupported boolean value');
      return value;
    };
    const scriptTarget = ts.getEmitScriptTarget(options);
    let targetName = computedEnumName(ts.ScriptTarget, scriptTarget);
    // Numeric reverse enums expose the SDK's compatibility alias `Latest`.
    // Preserve the named ESNext semantic identity accepted by the native DTO.
    if (scriptTarget === ts.ScriptTarget.ESNext) targetName = 'ESNext';
    const resolutionValue = ts.getEmitModuleResolutionKind(options);
    const moduleValue = ts.getEmitModuleKind(options);
    const strictFlag = key => options[key] === undefined ? !!options.strict : !!options[key];
    const normalized = {
      target:targetName,
      module:computedEnumName(ts.ModuleKind,moduleValue),
      moduleResolution:computedEnumName(ts.ModuleResolutionKind,resolutionValue),
      moduleDetection:computedEnumName(ts.ModuleDetectionKind,ts.getEmitModuleDetectionKind(options)),
      strict:!!options.strict,
      noImplicitAny:strictFlag('noImplicitAny'), strictNullChecks:strictFlag('strictNullChecks'),
      strictFunctionTypes:strictFlag('strictFunctionTypes'), strictBindCallApply:strictFlag('strictBindCallApply'),
      strictPropertyInitialization:strictFlag('strictPropertyInitialization'), noImplicitThis:strictFlag('noImplicitThis'),
      useUnknownInCatchVariables:strictFlag('useUnknownInCatchVariables'), alwaysStrict:strictFlag('alwaysStrict'),
      exactOptionalPropertyTypes:!!options.exactOptionalPropertyTypes,
      noUncheckedIndexedAccess:!!options.noUncheckedIndexedAccess, noImplicitReturns:!!options.noImplicitReturns,
      noPropertyAccessFromIndexSignature:!!options.noPropertyAccessFromIndexSignature,
      noImplicitOverride:!!options.noImplicitOverride, noUnusedLocals:!!options.noUnusedLocals,
      noUnusedParameters:!!options.noUnusedParameters, noFallthroughCasesInSwitch:!!options.noFallthroughCasesInSwitch,
      allowUnreachableCode:options.allowUnreachableCode, allowUnusedLabels:options.allowUnusedLabels,
      experimentalDecorators:!!options.experimentalDecorators, esModuleInterop:computedFlag('getESModuleInterop'),
      allowSyntheticDefaultImports:computedFlag('getAllowSyntheticDefaultImports'),
      resolveJsonModule:!!options.resolveJsonModule, noResolve:!!options.noResolve, allowJs:!!options.allowJs,
      checkJs:!!options.checkJs, skipLibCheck:!!options.skipLibCheck,
      noUncheckedSideEffectImports:!!options.noUncheckedSideEffectImports,
      noTypesAndSymbols:!!options.noTypesAndSymbols,
      useDefineForClassFields:options.useDefineForClassFields,
      jsx:options.jsx === undefined ? undefined : reverseName(ts.JsxEmit,options.jsx,undefined),
      jsxFactory:options.jsxFactory, jsxFragmentFactory:options.jsxFragmentFactory,
      jsxImportSource:options.jsxImportSource, reactNamespace:options.reactNamespace,
      lib:options.lib, types:options.types, typeRoots:options.typeRoots,
      baseUrl:options.baseUrl, paths:options.paths,
    };
    // Preserve absent legacy fields byte-for-byte. Newly admitted flags retain
    // their raw values; TSZ derives implications such as isolated-from-verbatim.
    for (const name of ['verbatimModuleSyntax','isolatedModules','erasableSyntaxOnly','allowImportingTsExtensions','noEmit','emitDeclarationOnly','declaration','composite','preserveConstEnums','rewriteRelativeImportExtensions','isolatedDeclarations']) {
      if (options[name] !== undefined) normalized[name] = options[name];
    }
    const checkerRelevant = new Set(['verbatimModuleSyntax','isolatedModules','erasableSyntaxOnly','allowImportingTsExtensions','noEmit','emitDeclarationOnly','declaration','composite','rewriteRelativeImportExtensions','preserveConstEnums','isolatedDeclarations','target','module','moduleResolution','moduleDetection','jsx','jsxFactory','jsxFragmentFactory','jsxImportSource','reactNamespace','lib','types','typeRoots','baseUrl','paths','strict','noImplicitAny','strictNullChecks','strictFunctionTypes','strictBindCallApply','strictPropertyInitialization','noImplicitThis','useUnknownInCatchVariables','alwaysStrict','exactOptionalPropertyTypes','noUncheckedIndexedAccess','noImplicitReturns','noPropertyAccessFromIndexSignature','noImplicitOverride','noUnusedLocals','noUnusedParameters','noFallthroughCasesInSwitch','allowUnreachableCode','allowUnusedLabels','experimentalDecorators','esModuleInterop','allowSyntheticDefaultImports','resolveJsonModule','noResolve','allowJs','checkJs','skipLibCheck','noUncheckedSideEffectImports','noTypesAndSymbols','useDefineForClassFields']);
    const unsupported = [];
    for (const declaration of (ts.optionDeclarations || [])) {
      const name = declaration.name;
      if ((declaration.affectsSemanticDiagnostics || declaration.affectsBindDiagnostics) && options[name] !== undefined && !checkerRelevant.has(name)) unsupported.push(name);
    }
    result({schema:1, version:ts.version, config_path:configPath ? path.resolve(configPath) : "", compiler_options:{compilerOptions:normalized},
      root_names:parsed.fileNames.map(name => path.resolve(name)),
      project_references:admitProjectForest ? (parsed.projectReferences || []).map(r => path.resolve(ts.resolveProjectReferencePath(r))) : [], files:sourceFiles, resolutions, accesses:Array.from(observations.values()),
      directory_views:Array.from(directoryViews.values()), access_operations:accessOperations, directory_queries:directoryQueries,
      directory_enumerations:directoryEnumerations, directory_visited_entries:directoryVisitedEntries, directory_verification_entries:directoryVerificationEntries,
      matcher_result_entries:matcherResultEntries,
      matcher_pattern_items:matcherPatternItems, matcher_pattern_bytes:matcherPatternBytes,
      directory_query_cache_bytes:directoryQueryCacheBytes,
      unsupported_options:Array.from(new Set(unsupported)).sort(), error:null, failure_kind:null,
      missing_compiler_api_export:null});
  }
} catch (error) {
  result({schema:1, version:'', config_path:configPath ? path.resolve(configPath) : "", compiler_options:{compilerOptions:{}},
    root_names:[], project_references:[], files:[], resolutions:[], accesses:Array.from(observations.values()), directory_views:Array.from(directoryViews.values()),
    access_operations:accessOperations, directory_queries:directoryQueries, directory_enumerations:directoryEnumerations,
    directory_visited_entries:directoryVisitedEntries, directory_verification_entries:directoryVerificationEntries,
    matcher_result_entries:matcherResultEntries,
    matcher_pattern_items:matcherPatternItems, matcher_pattern_bytes:matcherPatternBytes,
    directory_query_cache_bytes:directoryQueryCacheBytes,
    unsupported_options:[], error:String(error && error.message || error), failure_kind:error && error.closureFailureKind || null,
    missing_compiler_api_export:error && error.missingCompilerApiExport || null});
}
"#;

/// Resolves one checked TypeScript program using only the exact admitted compiler installation.
///
/// The production caller is `PackageSemanticCompiler`'s configured-TypeScript branch in
/// `application/compiler.rs`: it obtains `AdmittedTypeScriptProject::inputs()`, creates the
/// project-scoped resolver capability, and passes the package's complete source frontier here.
/// This function consumes that resolver ledger when it seals `NativeTypeScriptInputs`; the
/// caller retains the resulting closure witness in staged compilation and validates it before
/// staging, preparation, and publication. Thus the captured TypeScript query closure stays
/// attached to the full package result, rather than ending with this adapter call.

/// Separate programs, selected by the captured configuration DAG. Edges never
/// imply that a source is a member of the referencing program.
pub(crate) struct NativeTypeScriptForestInputs {
    pub(crate) root: u32,
    pub(crate) configured_forest: bool,
    /// Independently captured configured leaves, with no invented reference edges.
    pub(crate) project_set: bool,
    pub(crate) nodes: Vec<NativeTypeScriptProgramNode>,
    pub(crate) package_membership: BTreeMap<Box<str>, TypeScriptForestSourceMembership>,
    pub(crate) work_units: u64,
}
pub(crate) struct NativeTypeScriptProgramNode {
    pub(crate) config: Option<(String, ContentId<SourceFactDomain>)>,
    pub(crate) references: Vec<u32>,
    pub(crate) native: NativeTypeScriptInputs,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TypeScriptForestSourceMembership {
    Selected {
        program: usize,
        program_path: Box<str>,
    },
    ExcludedFromConfiguredProgram,
}
#[derive(Debug)]
pub(crate) struct TypeScriptForestClosureWitness(pub(crate) Vec<TypeScriptProgramClosureWitness>);
impl TypeScriptForestClosureWitness {
    pub(crate) fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>().saturating_add(retained_vec_bytes(
            &self.0,
            TypeScriptProgramClosureWitness::retained_bytes,
        ))
    }

    pub(crate) fn validate_current(
        &self,
        witness: &super::typescript_host::TypeScriptProjectWitness,
    ) -> Result<(), TypeScriptProjectHostError> {
        self.validate_current_controlled(witness, &mut || Ok(()))
    }

    pub(crate) fn validate_current_controlled(
        &self,
        witness: &super::typescript_host::TypeScriptProjectWitness,
        control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
    ) -> Result<(), TypeScriptProjectHostError> {
        let host = witness.validated_controlled(control)?;
        for program in &self.0 {
            control()?;
            program.validate_with_host_controlled(&host, control)?;
        }
        control()?;
        Ok(())
    }
}

/// A single aggregate allowance across the actual Compiler API reports. The
/// next child stdout limit uses the remaining allowance before its allocation.
#[derive(Default)]
struct ForestAdmissionBudget {
    report_bytes: usize,
    files: usize,
    requests: usize,
    source_bytes: usize,
    replay_work: u64,
    metrics: ProgramClosureMetrics,
    libraries: BTreeMap<
        (String, ContentId<SourceFactDomain>),
        std::sync::Arc<backend_frontend_typescript::TszLibFile>,
    >,
}
impl ForestAdmissionBudget {
    fn child_limits(&self) -> serde_json::Value {
        serde_json::json!({
            "files": MAX_BRIDGE_FILES.saturating_sub(self.files),
            "requests": MAX_BRIDGE_REQUESTS.saturating_sub(self.requests),
            "access_operations": MAX_BRIDGE_ACCESS_OPERATIONS.saturating_sub(self.metrics.access_operations),
            "directory_queries": MAX_BRIDGE_DIRECTORY_QUERIES.saturating_sub(self.metrics.directory_queries),
            "directory_enumerations": MAX_BRIDGE_DIRECTORY_ENUMERATIONS.saturating_sub(self.metrics.directory_enumerations),
            "directory_entries": MAX_BRIDGE_DIRECTORY_ENTRIES.saturating_sub(self.metrics.directory_visited_entries.max(self.metrics.directory_verification_entries)),
            "matcher_result_entries": MAX_BRIDGE_MATCHER_RESULT_WORK.saturating_sub(self.metrics.matcher_result_entries),
            "matcher_pattern_items": MAX_BRIDGE_MATCHER_PATTERN_ITEMS.saturating_sub(self.metrics.matcher_pattern_items),
            "matcher_pattern_bytes": MAX_BRIDGE_MATCHER_PATTERN_BYTES.saturating_sub(self.metrics.matcher_pattern_bytes),
            "directory_query_cache_bytes": MAX_BRIDGE_DIRECTORY_QUERY_CACHE_BYTES.saturating_sub(self.metrics.directory_query_cache_bytes),
        })
    }
    fn retain(&mut self, report: &ProgramReport) -> Result<(), TypeScriptProjectHostError> {
        macro_rules! charge {
            ($field:ident, $amount:expr, $maximum:expr) => {{
                self.$field = self.$field.saturating_add($amount);
                if self.$field > $maximum {
                    return Err(closure_limit(
                        concat!("forest ", stringify!($field)),
                        self.$field,
                        $maximum,
                    ));
                }
            }};
        }
        charge!(files, report.files.len(), MAX_BRIDGE_FILES);
        charge!(requests, report.resolutions.len(), MAX_BRIDGE_REQUESTS);
        let metrics = report.closure_metrics();
        macro_rules! metric {
            ($field:ident, $maximum:expr) => {{
                self.metrics.$field = self.metrics.$field.saturating_add(metrics.$field);
                if self.metrics.$field > $maximum {
                    return Err(closure_limit(
                        concat!("forest ", stringify!($field)),
                        self.metrics.$field,
                        $maximum,
                    ));
                }
            }};
        }
        metric!(access_operations, MAX_BRIDGE_ACCESS_OPERATIONS);
        metric!(directory_queries, MAX_BRIDGE_DIRECTORY_QUERIES);
        metric!(directory_enumerations, MAX_BRIDGE_DIRECTORY_ENUMERATIONS);
        metric!(directory_visited_entries, MAX_BRIDGE_DIRECTORY_ENTRIES);
        metric!(directory_verification_entries, MAX_BRIDGE_DIRECTORY_ENTRIES);
        metric!(matcher_result_entries, MAX_BRIDGE_MATCHER_RESULT_WORK);
        metric!(matcher_pattern_items, MAX_BRIDGE_MATCHER_PATTERN_ITEMS);
        metric!(matcher_pattern_bytes, MAX_BRIDGE_MATCHER_PATTERN_BYTES);
        metric!(
            directory_query_cache_bytes,
            MAX_BRIDGE_DIRECTORY_QUERY_CACHE_BYTES
        );
        Ok(())
    }
}

pub(crate) fn build_native_forest_inputs(
    inputs: &TypeScriptProjectInputs<'_>,
    package_sources: &[PackageSource<'_>],
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<NativeTypeScriptForestInputs, TypeScriptProjectHostError> {
    let selected = select_config(inputs)?;
    let configured_forest = selected.is_some_and(|config| !config.references.is_empty());
    let mut configs = Vec::new();
    if let Some(root) = selected {
        fn visit<'a>(
            config: &'a super::typescript_host::TypeScriptConfigInput,
            inputs: &TypeScriptProjectInputs<'a>,
            active: &mut BTreeSet<PathBuf>,
            seen: &mut BTreeSet<PathBuf>,
            output: &mut Vec<&'a super::typescript_host::TypeScriptConfigInput>,
        ) -> Result<(), TypeScriptProjectHostError> {
            use super::typescript_host::TypeScriptConfigDependencyAuthority;
            let path = normalize_path(&config.path);
            if active.contains(&path) {
                return Err(closure_mismatch("project-reference graph contains a cycle"));
            }
            if !seen.insert(path.clone()) {
                return Ok(());
            }
            if config.dependency_authority != TypeScriptConfigDependencyAuthority::Program {
                return Err(closure_mismatch(
                    "project reference was not admitted as a program config",
                ));
            }
            active.insert(path.clone());
            output.push(config);
            if output.len() > backend_semantic::ir::MAX_NATIVE_PROGRAM_NODES {
                return Err(closure_limit(
                    "project-reference nodes",
                    output.len(),
                    backend_semantic::ir::MAX_NATIVE_PROGRAM_NODES,
                ));
            }
            let mut unique = BTreeSet::new();
            for reference in &config.references {
                let reference = normalize_path(reference);
                if !unique.insert(reference.clone()) {
                    return Err(closure_mismatch("project config repeats a reference"));
                }
                let child = inputs
                    .config_candidates
                    .iter()
                    .find(|child| normalize_path(&child.path) == reference)
                    .ok_or_else(|| closure_mismatch("project reference has no captured config"))?;
                visit(child, inputs, active, seen, output)?;
            }
            active.remove(&path);
            Ok(())
        }
        visit(
            root,
            inputs,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
            &mut configs,
        )?;
        configs.sort_by(|a, b| a.path.cmp(&b.path));
    }
    let root = selected.map_or(0, |selected| {
        configs
            .iter()
            .position(|config| config.path == selected.path)
            .expect("selected root visited")
    }) as u32;
    let mut nodes = Vec::new();
    let mut budget = ForestAdmissionBudget::default();
    let host = inputs.validated_host()?;
    for config in configs
        .iter()
        .map(|config| Some(*config))
        .chain((selected.is_none()).then_some(None))
    {
        check_admission_control(deadline, cancelled)?;
        let references = config.map_or_else(Vec::new, |config| {
            config
                .references
                .iter()
                .map(|reference| {
                    configs
                        .iter()
                        .position(|child| normalize_path(&child.path) == normalize_path(reference))
                        .expect("captured edge visited") as u32
                })
                .collect()
        });
        let mut resolver = inputs.resolver();
        let native = build_native_inputs_for_config(
            inputs,
            &mut resolver,
            package_sources,
            config,
            configured_forest,
            &mut budget,
            Some(&host),
            deadline,
            cancelled,
        )?;
        if configured_forest && native.sources.is_empty() && references.is_empty() {
            return Err(closure_mismatch(
                "an empty referenced program has no solution edges",
            ));
        }
        let config = config
            .map(|config| {
                let path = config
                    .path
                    .strip_prefix(inputs.workspace_root)
                    .ok()
                    .and_then(Path::to_str)
                    .ok_or_else(|| {
                        closure_mismatch("program config has no canonical workspace path")
                    })?;
                Ok((path.replace('\\', "/"), config.content_id))
            })
            .transpose()?;
        nodes.push(NativeTypeScriptProgramNode {
            config,
            references,
            native,
        });
    }
    // Keep the producer-selected build environment and ownership. If it does
    // not select every captured source, a real conventional editor config may
    // admit the remainder under its own options and native filesystem closure.
    // This is a set of actual programs, not a source-root union injected into
    // either checker. Only the direct linked TSZ path has this admission policy.
    let has_unowned = package_sources.iter().any(|source| {
        nodes.iter().all(|node| {
            node.native.package_membership[source.relative_path()]
                .selected_program_path()
                .is_none()
        })
    });
    let mut supplemental_path = None;
    if has_unowned
        && matches!(
            inputs.authority,
            super::typescript_host::TypeScriptProjectInputAuthority::Native
        )
        && let Some(selected) = selected
        && let Some(config) = super::typescript_host::supplemental_conventional_config(
            selected,
            inputs.config_candidates,
        )?
    {
        check_admission_control(deadline, cancelled)?;
        let mut resolver = inputs.resolver();
        let native = build_native_inputs_for_config(
            inputs,
            &mut resolver,
            package_sources,
            Some(config),
            false,
            &mut budget,
            Some(&host),
            deadline,
            cancelled,
        )?;
        let adds_members = package_sources.iter().any(|source| {
            native.package_membership[source.relative_path()]
                .selected_program_path()
                .is_some()
                && nodes.iter().all(|node| {
                    node.native.package_membership[source.relative_path()]
                        .selected_program_path()
                        .is_none()
                })
        });
        if adds_members {
            let path = config
                .path
                .strip_prefix(inputs.workspace_root)
                .ok()
                .and_then(Path::to_str)
                .ok_or_else(|| {
                    closure_mismatch("independent config has no canonical workspace path")
                })?
                .replace('\\', "/");
            supplemental_path = Some(path.clone());
            nodes.push(NativeTypeScriptProgramNode {
                config: Some((path, config.content_id)),
                references: Vec::new(),
                native,
            });
        }
    }
    let project_set = supplemental_path.is_some();
    // Node indices are durable identities. Preserve canonical config ordering
    // and remap the real reference edges when adding an independent leaf.
    let mut root = root;
    if project_set {
        let mut ordered = nodes.into_iter().enumerate().collect::<Vec<_>>();
        ordered.sort_by(|(_, left), (_, right)| {
            left.config
                .as_ref()
                .unwrap()
                .0
                .cmp(&right.config.as_ref().unwrap().0)
        });
        let mut remap = vec![0u32; ordered.len()];
        for (new, (old, _)) in ordered.iter().enumerate() {
            remap[*old] = new as u32;
        }
        root = remap[root as usize];
        nodes = ordered
            .into_iter()
            .map(|(_, mut node)| {
                for reference in &mut node.references {
                    *reference = remap[*reference as usize];
                }
                node
            })
            .collect();
    }
    let supplemental_index = supplemental_path.as_ref().and_then(|path| {
        nodes
            .iter()
            .position(|node| node.config.as_ref().is_some_and(|config| &config.0 == path))
    });
    let mut membership = package_sources
        .iter()
        .map(|source| {
            (
                Box::<str>::from(source.relative_path()),
                TypeScriptForestSourceMembership::ExcludedFromConfiguredProgram,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut work_units = 0u64;
    let ownership_order = (0..nodes.len())
        .filter(|index| Some(*index) != supplemental_index)
        .chain(supplemental_index);
    for index in ownership_order {
        let node = &nodes[index];
        // The forest shares one counter. Its sum is clamped to the project cap
        // rather than refused: a large project must degrade, not fail admission.
        work_units = work_units
            .saturating_add(node.native.work_units)
            .min(MAX_PROJECT_WORK_UNITS);
        for (path, member) in &node.native.package_membership {
            if let Some(program_path) = member.selected_program_path() {
                let owner = membership.get_mut(path).ok_or_else(|| {
                    closure_mismatch("program member has no captured package source")
                })?;
                if matches!(owner, TypeScriptForestSourceMembership::Selected { .. }) {
                    if Some(index) == supplemental_index {
                        // An imported build source retains its producer-selected
                        // owner. It remains a dependency of this actual program.
                        continue;
                    }
                    return Err(closure_mismatch(
                        "captured package source belongs to multiple actual TypeScript programs",
                    ));
                }
                *owner = TypeScriptForestSourceMembership::Selected {
                    program: index,
                    program_path: program_path.into(),
                };
            }
        }
    }
    Ok(NativeTypeScriptForestInputs {
        root,
        configured_forest,
        project_set,
        nodes,
        package_membership: membership,
        work_units,
    })
}

pub(crate) fn build_native_inputs(
    inputs: &TypeScriptProjectInputs<'_>,
    resolver: &mut TypeScriptResolverCapability<'_>,
    package_sources: &[PackageSource<'_>],
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<NativeTypeScriptInputs, TypeScriptProjectHostError> {
    build_native_inputs_for_config(
        inputs,
        resolver,
        package_sources,
        select_config(inputs)?,
        false,
        &mut ForestAdmissionBudget::default(),
        None,
        deadline,
        cancelled,
    )
}

fn build_native_inputs_for_config(
    inputs: &TypeScriptProjectInputs<'_>,
    resolver: &mut TypeScriptResolverCapability<'_>,
    package_sources: &[PackageSource<'_>],
    config: Option<&super::typescript_host::TypeScriptConfigInput>,
    forest: bool,
    aggregate: &mut ForestAdmissionBudget,
    validated_host: Option<&ValidatedTypeScriptHost<'_>>,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<NativeTypeScriptInputs, TypeScriptProjectHostError> {
    if matches!(
        inputs.authority,
        super::typescript_host::TypeScriptProjectInputAuthority::Native
    ) {
        return direct::build_direct_inputs_for_config(
            inputs,
            resolver,
            package_sources,
            config,
            forest,
            aggregate,
            validated_host,
            deadline,
            cancelled,
        );
    }
    check_admission_control(deadline, cancelled)?;
    let api = inputs.compiler_api()?;
    // The admitted `compiler_path` is the executable CLI shim (`bin/tsc`). It
    // is probed separately to establish the selected tool identity, but it is
    // not the Compiler API module. Resolve that module only from the exact
    // captured TypeScript package closure so `require` cannot silently select
    // a different install from Node's module search path.
    let compiler_api_path = api
        .typescript_module_root
        .join("typescript/lib/typescript.js");
    let expected_version = std::str::from_utf8(api.compiler_version)
        .ok()
        .map(str::trim)
        .and_then(|version| version.strip_prefix("Version ").or(Some(version)))
        .ok_or_else(|| bridge_error("the admitted TypeScript package version is not valid text"))?;
    let compiler_api = inputs
        .typescript_files
        .iter()
        .find(|file| normalize_path(&file.path) == normalize_path(&compiler_api_path))
        .ok_or_else(|| TypeScriptProjectHostError::CompilerApiEntryMissing {
            package_root: api
                .typescript_module_root
                .join("typescript")
                .into_boxed_path(),
            package_version: expected_version.to_owned().into_boxed_str(),
            entrypoint: compiler_api_path.clone().into_boxed_path(),
        })?;
    let report = run_program_bridge_with_forest_limits(
        inputs,
        config.map(|config| config.path.as_ref()),
        package_sources,
        &compiler_api_path,
        forest,
        &aggregate.child_limits(),
        &mut aggregate.report_bytes,
        deadline,
        cancelled,
    )?;
    aggregate.retain(&report)?;
    if report.schema != 1 {
        return Err(bridge_error("unsupported compiler API report schema"));
    }
    if let Some(message) = report.error.as_deref() {
        if report.failure_kind.as_deref() == Some("capability") {
            if let Some(export) = report.missing_compiler_api_export.as_deref() {
                return Err(TypeScriptProjectHostError::CompilerApiExportMissing {
                    package_root: inputs
                        .compiler_api()?
                        .typescript_module_root
                        .join("typescript")
                        .into_boxed_path(),
                    package_version: expected_version.to_owned().into_boxed_str(),
                    entrypoint: compiler_api_path.clone().into_boxed_path(),
                    export: export.to_owned().into_boxed_str(),
                });
            }
            return Err(bridge_error(
                "the compiler API capability report omitted its missing export name",
            ));
        }
        return Err(match report.failure_kind.as_deref() {
            Some("limit") => TypeScriptProjectHostError::CompilerIoClosureLimit {
                phase: "TypeScript compiler bridge".into(),
                detail: message.into(),
            },
            Some("mismatch") => TypeScriptProjectHostError::CompilerIoClosureMismatch {
                detail: message.into(),
            },
            _ => bridge_error(message),
        });
    }
    if expected_version != report.version.as_str() {
        return Err(bridge_error(
            "the compiler API version differs from the admitted TypeScript identity",
        ));
    }
    let observed_config = Path::new(&report.config_path);
    if config.map(|selected| normalize_path(selected.path.as_ref()))
        != (!report.config_path.is_empty()).then(|| normalize_path(observed_config))
    {
        return Err(bridge_error(
            "the compiler API parsed a different tsconfig than the selected project config",
        ));
    }
    if !forest && !report.project_references.is_empty() {
        return Err(bridge_error(
            "project references are not admitted as one TSZ project",
        ));
    }
    if forest {
        let expected = config.map_or(&[][..], |config| config.references.as_ref());
        if report.project_references.len() != expected.len() {
            return Err(closure_mismatch(
                "Compiler API reference graph differs from captured config",
            ));
        }
        for (reported, expected) in report.project_references.iter().zip(expected) {
            let path = reported
                .as_str()
                .ok_or_else(|| closure_mismatch("Compiler API reference path is not text"))?;
            let canonical = resolver
                .realpath(Path::new(path))?
                .ok_or_else(|| closure_mismatch("Compiler API reference config disappeared"))?;
            if normalize_path(&canonical) != normalize_path(expected) {
                return Err(closure_mismatch(
                    "Compiler API reference selects another config",
                ));
            }
        }
    }
    let solution = forest && report.root_names.is_empty() && !report.project_references.is_empty();
    if forest && report.root_names.is_empty() && report.project_references.is_empty() {
        return Err(closure_mismatch(
            "an empty config with no references is not a solution program",
        ));
    }
    if solution && !report.files.is_empty() {
        return Err(closure_mismatch(
            "solution container unexpectedly has Compiler API source members",
        ));
    }
    if let Some(option) = report.unsupported_options.first() {
        return Err(bridge_error(&format!(
            "compiler option {option:?} affects semantic analysis but is outside the TSZ option model"
        )));
    }
    if report.files.len() > MAX_BRIDGE_FILES {
        return Err(bridge_error(
            "compiler API program source count exceeds the admitted bound",
        ));
    }
    if report.resolutions.len() > MAX_BRIDGE_REQUESTS {
        return Err(bridge_error(
            "compiler API module-request count exceeds the admitted bound",
        ));
    }
    let metrics = report.closure_metrics();
    let previous_work = aggregate.replay_work;
    resolver.charge_closure_replay_work(
        usize::try_from(previous_work).map_err(|_| bridge_error("forest replay work overflow"))?,
        "previous forest programs",
    )?;
    replay_observations(resolver, &report.accesses, &report.directory_views, metrics)?;

    let checker = compiler_api_checker_options(&report.compiler_options)?;
    let mut virtual_by_canonical = BTreeMap::<PathBuf, String>::new();
    let mut source_by_virtual = BTreeMap::<String, String>::new();
    let mut libraries = Vec::new();
    let mut content_ids = BTreeMap::<String, ContentId<SourceFactDomain>>::new();
    let mut source_paths = BTreeSet::new();
    let mut file_formats = Vec::new();

    let mut files = report.files.iter().collect::<Vec<_>>();
    files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    for file in files {
        check_admission_control(deadline, cancelled)?;
        let path = Path::new(&file.path);
        let Some(admitted) = resolver.try_load_source(path)? else {
            return Err(bridge_error(&format!(
                "the compiler API included a missing source file {:?}",
                file.path
            )));
        };
        aggregate.source_bytes = aggregate.source_bytes.saturating_add(admitted.bytes.len());
        if aggregate.source_bytes > 256 * 1024 * 1024 {
            return Err(closure_limit(
                "forest source text",
                aggregate.source_bytes,
                256 * 1024 * 1024,
            ));
        }
        let text = std::str::from_utf8(&admitted.bytes)
            .map_err(|_| bridge_error("a compiler program source is not UTF-8"))?;
        if compiler_api_utf8_text_sha256(&admitted.bytes)? != file.sha256 {
            return Err(bridge_error(&format!(
                "the compiler API source bytes changed at {:?}",
                file.path
            )));
        }
        let (virtual_path, is_library) = if file.default_library {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| bridge_error("a compiler library path is not portable UTF-8"))?;
            (format!("{LIB_VIRTUAL_PREFIX}{name}"), true)
        } else {
            (workspace_virtual_path(inputs, &admitted.path)?, false)
        };
        if !source_paths.insert(virtual_path.clone()) {
            return Err(bridge_error(&format!(
                "two compiler program files collapse to TSZ path {virtual_path:?}"
            )));
        }
        virtual_by_canonical.insert(normalize_path(&admitted.path), virtual_path.clone());
        content_ids.insert(virtual_path.clone(), admitted.content_id);
        if !is_library {
            file_formats.push(TszProjectFileFormatInput {
                path: virtual_path.clone(),
                format: file.implied_node_format,
            });
        }
        if is_library {
            let key = (virtual_path.clone(), admitted.content_id);
            let library = match aggregate.libraries.get(&key) {
                Some(library) => library.clone(),
                None => {
                    let input = TszLibraryInput::from_utf8(virtual_path, admitted.bytes.to_vec())
                        .map_err(|error| {
                        bridge_error(&format!("compiler library input was rejected: {error}"))
                    })?;
                    let library = input.into_lib_file();
                    aggregate.libraries.insert(key, library.clone());
                    library
                }
            };
            libraries.push(library);
        } else {
            source_by_virtual.insert(virtual_path, text.to_owned());
        }
    }

    let mut package_membership = BTreeMap::new();
    for source in package_sources {
        check_admission_control(deadline, cancelled)?;
        let package_path = source.relative_path();
        let absolute = inputs.package_root.join(package_path);
        let Some(admitted) = resolver.try_load_source(&absolute)? else {
            return Err(TypeScriptProjectHostError::CompilerProgramSourceMissing {
                package_relative_path: package_path.into(),
            });
        };
        let expected = source.source().as_bytes();
        if admitted.bytes.as_ref() != expected {
            return Err(bridge_error(&format!(
                "captured package source bytes differ from admitted bytes at {package_path:?}"
            )));
        }
        let canonical = normalize_path(&admitted.path);
        let membership = match virtual_by_canonical.get(&canonical) {
            Some(program_path) => TypeScriptPackageSourceMembership::Selected {
                program_path: program_path.clone().into_boxed_str(),
            },
            None if config.is_some() => {
                TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram
            }
            // Inferred programs are rooted in the complete captured frontier. Absence
            // here is a broken handoff, not configured exclusion.
            None => {
                return Err(TypeScriptProjectHostError::CompilerProgramSourceMissing {
                    package_relative_path: package_path.into(),
                });
            }
        };
        package_membership.insert(package_path.into(), membership);
    }

    let mut resolutions = Vec::with_capacity(report.resolutions.len());
    let mut resolution_keys = BTreeSet::new();
    for request in report.resolutions {
        check_admission_control(deadline, cancelled)?;
        let importer =
            resolve_virtual_path(inputs, resolver, &virtual_by_canonical, &request.importer)?;
        let request_kind = match request.kind.as_str() {
            "EsmImport" => TszProjectModuleRequestKind::EsmImport,
            "DynamicImport" => TszProjectModuleRequestKind::DynamicImport,
            "CjsRequire" => TszProjectModuleRequestKind::CjsRequire,
            "EsmReExport" => TszProjectModuleRequestKind::EsmReExport,
            other => TszProjectModuleRequestKind::Unsupported {
                syntax_kind: other.to_owned(),
            },
        };
        let resolution_mode = match request.mode.as_deref() {
            None => None,
            Some("import") => Some(backend_frontend_typescript::TszResolutionModeOverride::Import),
            Some("require") => {
                Some(backend_frontend_typescript::TszResolutionModeOverride::Require)
            }
            Some(other) => {
                return Err(bridge_error(&format!(
                    "unsupported TypeScript resolution mode {other:?}"
                )));
            }
        };
        let target = if let Some(target_path) = request.target_path.as_deref() {
            let target_path = Path::new(target_path);
            let Some(target_input) = resolver.try_load_source(target_path)? else {
                return Err(bridge_error(&format!(
                    "compiler resolution target {:?} was not admitted",
                    target_path
                )));
            };
            let canonical_target = normalize_path(&target_input.path);
            if let Some(path) = virtual_by_canonical.get(&canonical_target) {
                TszProjectModuleResolutionTarget::File { path: path.clone() }
            } else {
                if forest {
                    return Err(closure_mismatch(
                        "resolved reference target is outside its actual owning program",
                    ));
                }
                let mut identity = Sha256::new();
                identity.update(b"typescript.external-module-target.v1\0");
                identity.update(target_input.content_id.as_ref());
                identity.update(target_input.path.as_os_str().as_encoded_bytes());
                if let Some(package_id) = request.package_id.as_deref() {
                    identity.update(package_id.as_bytes());
                }
                TszProjectModuleResolutionTarget::External {
                    identity: format!("ts-module:{}", hex_digest(&identity.finalize())),
                }
            }
        } else {
            TszProjectModuleResolutionTarget::Unresolved
        };
        let resolution = TszProjectModuleResolution {
            importer_path: importer,
            specifier: request.specifier,
            request_kind,
            resolution_mode,
            target,
        };
        if !resolution_keys.insert(format!("{:?}", resolution)) {
            return Err(bridge_error(
                "compiler API returned a duplicate module-resolution record",
            ));
        }
        resolutions.push(resolution);
    }
    resolutions.sort_by(|left, right| {
        (
            &left.importer_path,
            &left.specifier,
            format!("{:?}", left.request_kind),
            format!("{:?}", left.resolution_mode),
        )
            .cmp(&(
                &right.importer_path,
                &right.specifier,
                format!("{:?}", right.request_kind),
                format!("{:?}", right.resolution_mode),
            ))
    });

    let mut sources = Vec::with_capacity(source_by_virtual.len());
    for (path, source) in source_by_virtual {
        sources.push(TszFileInput { path, source });
    }
    let compiler_accesses = report.accesses.into_boxed_slice();
    let directory_views = report.directory_views.into_boxed_slice();
    let access_digest = compiler_access_digest(&compiler_accesses, &directory_views, metrics)?;
    check_admission_control(deadline, cancelled)?;
    aggregate.replay_work = resolver.closure_replay_work_units();
    let closure_work_units = aggregate.replay_work.saturating_sub(previous_work);
    let resolver_witness = match validated_host {
        Some(host) => resolver.seal_with_host(host)?,
        None => resolver.seal()?,
    };
    let environment = program_environment_fingerprint(
        inputs,
        config,
        &report.version,
        &report.compiler_options,
        &content_ids,
        &compiler_api.content_id,
        &access_digest,
        &resolutions,
        &file_formats,
        resolver_witness.digest(),
    )?;
    let work = calculate_work_allowance(inputs, &sources, &libraries, closure_work_units);
    let options = TszProjectOptions {
        checker,
        semantic_options: TszProjectSemanticOptions::declaration_scoped(),
        program_diagnostics: Vec::new(),
        module_resolutions: resolutions,
        file_formats,
        environment,
    };
    check_admission_control(deadline, cancelled)?;
    Ok(NativeTypeScriptInputs {
        sources,
        libraries,
        options,
        closure_witness: TypeScriptProgramClosureWitness {
            resolver: resolver_witness,
            compiler_accesses,
            directory_views,
            metrics,
            access_digest,
        },
        package_membership,
        work_units: work.program,
        file_work_units: work.per_file,
        solution,
    })
}

// Excluded sources do not reach TSZ's execution checkpoint, so admission itself
// must still refuse cancellation/deadline expiry throughout the full frontier.
pub(super) fn check_admission_control(
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<(), TypeScriptProjectHostError> {
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return Err(TypeScriptProjectHostError::CompilerProgramCancelled);
    }
    if Instant::now() >= deadline {
        return Err(TypeScriptProjectHostError::CompilerProgramDeadline);
    }
    Ok(())
}

fn run_program_bridge(
    inputs: &TypeScriptProjectInputs<'_>,
    config_path: Option<&Path>,
    package_sources: &[PackageSource<'_>],
    compiler_api_path: &Path,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<ProgramReport, TypeScriptProjectHostError> {
    run_program_bridge_with_forest_limits(
        inputs,
        config_path,
        package_sources,
        compiler_api_path,
        false,
        &ForestAdmissionBudget::default().child_limits(),
        &mut 0,
        deadline,
        cancelled,
    )
}

fn run_program_bridge_with_forest_limits(
    inputs: &TypeScriptProjectInputs<'_>,
    config_path: Option<&Path>,
    package_sources: &[PackageSource<'_>],
    compiler_api_path: &Path,
    forest: bool,
    child_limits: &serde_json::Value,
    aggregate_report_bytes: &mut usize,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<ProgramReport, TypeScriptProjectHostError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(TypeScriptProjectHostError::CompilerProgramDeadline);
    }
    let limits = ToolchainProbeLimits::new(
        remaining,
        NonZeroUsize::new(MAX_BRIDGE_STDOUT_BYTES.saturating_sub(*aggregate_report_bytes))
            .ok_or_else(|| bridge_error("forest report stream allowance exhausted"))?,
    )
    .map_err(|error| bridge_error(&error.to_string()))?;
    // A configless package uses exactly the caller's already selected source frontier.
    // No file is synthesized, and the existing resolver still witnesses every compiler read.
    let inferred_roots = package_sources
        .iter()
        .map(|source| inputs.package_root.join(source.relative_path()))
        .collect::<Vec<_>>();
    let inferred_roots = serde_json::to_string(&inferred_roots).map_err(|error| {
        bridge_error(&format!(
            "captured source roots could not be encoded: {error}"
        ))
    })?;
    let arguments = [
        OsString::from(compiler_api_path.as_os_str()),
        config_path
            .map(|path| OsString::from(path.as_os_str()))
            .unwrap_or_default(),
        OsString::from(inputs.workspace_root.as_os_str()),
        OsString::from(inferred_roots),
        OsString::from(if forest { "forest" } else { "single" }),
        OsString::from(
            serde_json::to_string(child_limits)
                .map_err(|_| bridge_error("forest allowance encoding"))?,
        ),
    ];
    let output = run_typescript_program_bridge(
        inputs.compiler_api()?.node_path,
        COMPILER_API_PROGRAM_SCRIPT,
        &arguments,
        inputs.package_root,
        limits,
        cancelled,
    )
    .map_err(|source| TypeScriptProjectHostError::CompilerProgramProbe { source })?;
    *aggregate_report_bytes = aggregate_report_bytes.saturating_add(output.len());
    serde_json::from_slice(&output).map_err(|error| {
        bridge_error(&format!(
            "compiler API output was not bounded valid JSON: {error}"
        ))
    })
}

fn select_config<'a>(
    inputs: &'a TypeScriptProjectInputs<'_>,
) -> Result<Option<&'a super::typescript_host::TypeScriptConfigInput>, TypeScriptProjectHostError> {
    let selected = super::typescript_host::select_project_config(
        inputs.package_root,
        inputs.config_candidates,
    );
    if selected.is_some_and(|config| {
        config.dependency_authority
            != super::typescript_host::TypeScriptConfigDependencyAuthority::Program
    }) {
        return Err(bridge_error(
            "the selected TypeScript project config has no admitted dependency closure",
        ));
    }
    Ok(selected)
}

fn compiler_api_checker_options(
    options: &serde_json::Value,
) -> Result<TszCheckerOptions, TypeScriptProjectHostError> {
    let mut normalized = options.clone();
    normalize_compiler_api_lib_names(&mut normalized);
    let json = serde_json::to_string(&normalized).map_err(|error| {
        bridge_error(&format!(
            "normalized TypeScript options could not be encoded: {error}"
        ))
    })?;
    checker_options_from_compiler_api_json(&json).map_err(|error| {
        bridge_error(&format!(
            "normalized TypeScript checker options are unsupported: {error}"
        ))
    })
}

fn normalize_compiler_api_lib_names(options: &mut serde_json::Value) {
    let Some(libraries) = options
        .get_mut("compilerOptions")
        .and_then(|options| options.get_mut("lib"))
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for library in libraries {
        let Some(name) = library.as_str() else {
            continue;
        };
        let Some(without_prefix) = name
            .get(..4)
            .filter(|prefix| prefix.eq_ignore_ascii_case("lib."))
            .map(|_| &name[4..])
        else {
            continue;
        };
        let without_suffix = without_prefix
            .get(..without_prefix.len().saturating_sub(5))
            .filter(|_| without_prefix.to_ascii_lowercase().ends_with(".d.ts"));
        if let Some(canonical) = without_suffix {
            *library = serde_json::Value::String(canonical.to_owned());
        }
    }
}

fn replay_observations(
    resolver: &mut TypeScriptResolverCapability<'_>,
    accesses: &[ProgramAccess],
    directory_views: &[ProgramDirectoryView],
    metrics: ProgramClosureMetrics,
) -> Result<u64, TypeScriptProjectHostError> {
    if metrics.access_operations > MAX_BRIDGE_ACCESS_OPERATIONS {
        return Err(closure_limit(
            "access operation",
            metrics.access_operations,
            MAX_BRIDGE_ACCESS_OPERATIONS,
        ));
    }
    if metrics.directory_queries > MAX_BRIDGE_DIRECTORY_QUERIES {
        return Err(closure_limit(
            "directory query",
            metrics.directory_queries,
            MAX_BRIDGE_DIRECTORY_QUERIES,
        ));
    }
    if metrics.directory_enumerations > MAX_BRIDGE_DIRECTORY_ENUMERATIONS {
        return Err(closure_limit(
            "directory enumeration",
            metrics.directory_enumerations,
            MAX_BRIDGE_DIRECTORY_ENUMERATIONS,
        ));
    }
    if metrics.directory_visited_entries > MAX_BRIDGE_DIRECTORY_ENTRIES
        || metrics.directory_verification_entries > MAX_BRIDGE_DIRECTORY_ENTRIES
    {
        return Err(closure_limit(
            "directory entry traversal",
            metrics
                .directory_visited_entries
                .max(metrics.directory_verification_entries),
            MAX_BRIDGE_DIRECTORY_ENTRIES,
        ));
    }
    if metrics.matcher_result_entries > MAX_BRIDGE_MATCHER_RESULT_WORK {
        return Err(closure_limit(
            "matcher result verification",
            metrics.matcher_result_entries,
            MAX_BRIDGE_MATCHER_RESULT_WORK,
        ));
    }
    if metrics.matcher_pattern_items > MAX_BRIDGE_MATCHER_PATTERN_ITEMS {
        return Err(closure_limit(
            "matcher pattern items",
            metrics.matcher_pattern_items,
            MAX_BRIDGE_MATCHER_PATTERN_ITEMS,
        ));
    }
    if metrics.matcher_pattern_bytes > MAX_BRIDGE_MATCHER_PATTERN_BYTES {
        return Err(closure_limit(
            "matcher pattern bytes",
            metrics.matcher_pattern_bytes,
            MAX_BRIDGE_MATCHER_PATTERN_BYTES,
        ));
    }
    if metrics.directory_query_cache_bytes > MAX_BRIDGE_DIRECTORY_QUERY_CACHE_BYTES {
        return Err(closure_limit(
            "exact directory-query cache bytes",
            metrics.directory_query_cache_bytes,
            MAX_BRIDGE_DIRECTORY_QUERY_CACHE_BYTES,
        ));
    }
    if accesses.len() > MAX_BRIDGE_ACCESS_OPERATIONS {
        return Err(closure_limit(
            "retained filesystem access",
            accesses.len(),
            MAX_BRIDGE_ACCESS_OPERATIONS,
        ));
    }
    if directory_views.len() > MAX_BRIDGE_DIRECTORY_ENUMERATIONS {
        return Err(closure_limit(
            "retained directory view",
            directory_views.len(),
            MAX_BRIDGE_DIRECTORY_ENUMERATIONS,
        ));
    }

    resolver.charge_closure_replay_work(metrics.access_operations, "retained filesystem access")?;
    resolver.charge_closure_replay_work(metrics.directory_queries, "directory query replay")?;
    resolver.charge_closure_replay_work(
        metrics.directory_enumerations,
        "directory enumeration replay",
    )?;
    resolver
        .charge_closure_replay_work(metrics.directory_visited_entries, "directory entry replay")?;
    resolver.charge_closure_replay_work(
        metrics.directory_verification_entries,
        "directory result verification",
    )?;
    resolver.charge_closure_replay_work(
        metrics.matcher_result_entries,
        "matcher result verification",
    )?;
    resolver
        .charge_closure_replay_work(metrics.matcher_pattern_items, "matcher pattern item replay")?;
    resolver
        .charge_closure_replay_work(metrics.matcher_pattern_bytes, "matcher pattern byte replay")?;
    resolver.charge_closure_replay_work(
        metrics.directory_query_cache_bytes,
        "exact directory-query cache admission",
    )?;

    let mut view_by_path = BTreeMap::new();
    let mut view_entries = 0_usize;
    let query_accesses = accesses
        .iter()
        .filter(|access| {
            matches!(
                access.kind,
                ProgramAccessKind::ReadDirectory | ProgramAccessKind::GetDirectories
            )
        })
        .count();
    if metrics.access_operations < accesses.len()
        || metrics.directory_queries < query_accesses
        || metrics.directory_enumerations < directory_views.len()
    {
        return Err(closure_mismatch(
            "compiler filesystem work counters do not cover the retained closure",
        ));
    }
    for view in directory_views {
        let path = Path::new(&view.path);
        if !path.is_absolute() || normalize_path(path).to_string_lossy() != view.path {
            return Err(closure_mismatch(
                "TypeScript directory view used a non-canonical path",
            ));
        }
        if view_by_path.insert(view.path.as_str(), view).is_some() {
            return Err(closure_mismatch(
                "TypeScript directory transcript repeated one directory view",
            ));
        }
        for (label, names) in [("file", &view.files), ("directory", &view.directories)] {
            if names
                .windows(2)
                .any(|pair| typescript_string_cmp(&pair[0], &pair[1]) != std::cmp::Ordering::Less)
            {
                return Err(closure_mismatch(&format!(
                    "TypeScript directory {label} view is not strictly sorted and unique"
                )));
            }
            for name in names {
                let name_path = Path::new(name);
                if name.is_empty()
                    || name_path.is_absolute()
                    || name_path.components().count() != 1
                    || name_path.file_name().and_then(|value| value.to_str()) != Some(name)
                {
                    return Err(closure_mismatch(
                        "TypeScript directory view contains a non-basename entry",
                    ));
                }
            }
        }
        if !view.read_succeeded && (!view.files.is_empty() || !view.directories.is_empty()) {
            return Err(closure_mismatch(
                "failed TypeScript directory enumeration retained entries",
            ));
        }
        view_entries = view_entries
            .saturating_add(view.files.len())
            .saturating_add(view.directories.len());
        if view_entries > MAX_BRIDGE_DIRECTORY_ENTRIES {
            return Err(closure_limit(
                "retained directory view entries",
                view_entries,
                MAX_BRIDGE_DIRECTORY_ENTRIES,
            ));
        }
    }
    if view_entries > metrics.directory_visited_entries {
        return Err(closure_mismatch(
            "compiler reported fewer visited entries than its retained directory views",
        ));
    }
    resolver.charge_closure_replay_work(view_entries, "directory snapshot comparison")?;

    let mut referenced_views = BTreeSet::new();
    let mut checked_views = BTreeSet::new();
    let mut checked_realpaths = BTreeMap::<String, (String, bool)>::new();
    for access in accesses {
        resolver.charge_closure_replay_work(1, "filesystem access replay")?;
        let path = Path::new(&access.path);
        if !path.is_absolute() || normalize_path(path).to_string_lossy() != access.path {
            return Err(closure_mismatch(
                "compiler filesystem observation used a non-canonical query path",
            ));
        }
        match access.kind {
            ProgramAccessKind::File | ProgramAccessKind::Exists => {
                let source = resolver.try_load_source(path)?;
                if source.is_some() != access.exists.unwrap_or(false) {
                    return Err(closure_mismatch(&format!(
                        "compiler filesystem result changed at {:?}",
                        access.path
                    )));
                }
                if let (Some(source), Some(expected)) = (source, access.sha256.as_deref())
                    && compiler_api_utf8_text_sha256(&source.bytes)? != expected
                {
                    return Err(closure_mismatch(&format!(
                        "compiler filesystem bytes changed at {:?}",
                        access.path
                    )));
                }
            }
            ProgramAccessKind::Directory => {
                if access.exists == Some(false) {
                    resolver.observe_absent_directory(path)?;
                } else if resolver.directory_exists(path)? != access.exists.unwrap_or(false) {
                    return Err(closure_mismatch(&format!(
                        "compiler directory result changed at {:?}",
                        access.path
                    )));
                }
            }
            ProgramAccessKind::Realpath => {
                let observed_path = resolver.realpath(path)?;
                let expected = access
                    .realpath
                    .as_deref()
                    .map(Path::new)
                    .map(normalize_path);
                if observed_path.as_deref().map(normalize_path) != expected {
                    return Err(closure_mismatch(&format!(
                        "compiler realpath result changed at {:?}",
                        access.path
                    )));
                }
            }
            ProgramAccessKind::ReadDirectory | ProgramAccessKind::GetDirectories => {
                let entries = access.entries.as_deref().ok_or_else(|| {
                    closure_mismatch("compiler API omitted a directory query result set")
                })?;
                if entries.windows(2).any(|pair| {
                    typescript_string_cmp(&pair[0], &pair[1]) != std::cmp::Ordering::Less
                }) {
                    return Err(closure_mismatch(
                        "compiler directory query result is not strictly sorted and unique",
                    ));
                }
                if access.directory_view_paths.is_empty()
                    || access.directory_view_paths.windows(2).any(|pair| {
                        typescript_string_cmp(&pair[0], &pair[1]) != std::cmp::Ordering::Less
                    })
                {
                    return Err(closure_mismatch(
                        "compiler directory query omitted or repeated its exact enumeration views",
                    ));
                }
                if access.directory_realpaths.windows(2).any(|pair| {
                    typescript_string_cmp(&pair[0].path, &pair[1].path) != std::cmp::Ordering::Less
                }) {
                    return Err(closure_mismatch(
                        "compiler directory realpath transcript is not strictly sorted and unique",
                    ));
                }
                if access.kind == ProgramAccessKind::ReadDirectory {
                    if access.recursive.is_none()
                        || access.current_directory.as_deref().is_none_or(|current| {
                            let current = Path::new(current);
                            !current.is_absolute()
                                || normalize_path(current).to_string_lossy()
                                    != access.current_directory.as_deref().unwrap_or_default()
                        })
                        || access.use_case_sensitive_file_names.is_none()
                    {
                        return Err(closure_mismatch(
                            "compiler readDirectory transcript omitted exact matcher parameters",
                        ));
                    }
                }
                validate_matcher_transcript(access, entries)?;
                for resolution in &access.directory_realpaths {
                    resolver.charge_closure_replay_work(1, "directory realpath replay")?;
                    let path = Path::new(&resolution.path);
                    let target = Path::new(&resolution.target);
                    if !path.is_absolute()
                        || normalize_path(path).to_string_lossy() != resolution.path
                        || !target.is_absolute()
                        || normalize_path(target).to_string_lossy() != resolution.target
                    {
                        return Err(closure_mismatch(
                            "compiler directory realpath transcript used a non-canonical path",
                        ));
                    }
                    let observed = match checked_realpaths.get(&resolution.path) {
                        Some(previous)
                            if previous != &(resolution.target.clone(), resolution.succeeded) =>
                        {
                            return Err(closure_mismatch(
                                "one TypeScript directory had conflicting realpath observations",
                            ));
                        }
                        Some(_) => continue,
                        None => resolver.realpath(path)?,
                    };
                    let observed_target = observed
                        .as_deref()
                        .map(normalize_path)
                        .unwrap_or_else(|| normalize_path(path));
                    if observed_target != normalize_path(target)
                        || observed.is_some() != resolution.succeeded
                    {
                        return Err(closure_mismatch(&format!(
                            "TypeScript directory realpath changed at {:?}",
                            resolution.path
                        )));
                    }
                    checked_realpaths.insert(
                        resolution.path.clone(),
                        (resolution.target.clone(), resolution.succeeded),
                    );
                }
                let mut access_files = BTreeSet::new();
                let mut access_directories = BTreeSet::new();
                for view_path in &access.directory_view_paths {
                    resolver.charge_closure_replay_work(1, "directory snapshot replay")?;
                    let view = view_by_path
                        .get(view_path.as_str())
                        .copied()
                        .ok_or_else(|| {
                            closure_mismatch(
                                "compiler directory query references an uncaptured view",
                            )
                        })?;
                    referenced_views.insert(view_path.as_str());
                    let view_root = Path::new(&view.path);
                    for name in &view.files {
                        access_files.insert(normalize_path(&view_root.join(name)));
                    }
                    for name in &view.directories {
                        access_directories.insert(normalize_path(&view_root.join(name)));
                    }
                    if checked_views.insert(view_path.as_str()) {
                        let canonical = resolver.observe_program_directory(Path::new(view_path))?;
                        match canonical {
                            Some(canonical) => {
                                if !view.read_succeeded {
                                    return Err(closure_mismatch(&format!(
                                        "TypeScript could not enumerate admitted directory {:?}",
                                        view.path
                                    )));
                                }
                                let snapshot_entries = resolver
                                    .observed_program_directory_entries(&canonical)
                                    .ok_or_else(|| {
                                        closure_mismatch(
                                            "admitted directory has no retained immutable snapshot",
                                        )
                                    })?
                                    .len();
                                resolver.charge_closure_replay_work(
                                    snapshot_entries,
                                    "directory snapshot entry comparison",
                                )?;
                                let snapshot = resolver
                                    .observed_program_directory_entries(&canonical)
                                    .expect("retained immutable snapshot remains present");
                                let mut files = Vec::new();
                                let mut directories = Vec::new();
                                for entry in snapshot {
                                    if !resolver.program_directory_entry_is_admitted(entry) {
                                        return Err(
                                            TypeScriptProjectHostError::SourceOutsideCapability {
                                                path: entry
                                                    .canonical_path
                                                    .as_deref()
                                                    .unwrap_or(entry.path.as_ref())
                                                    .to_path_buf()
                                                    .into_boxed_path(),
                                            },
                                        );
                                    }
                                    let name = entry
                                        .path
                                        .file_name()
                                        .and_then(|name| name.to_str())
                                        .ok_or_else(|| {
                                            closure_mismatch(
                                                "admitted directory contains a non-portable entry name",
                                            )
                                        })?;
                                    match entry.effective_kind {
                                        Some(TypeScriptDirectoryEntryKind::RegularFile) => {
                                            files.push(name.to_owned());
                                        }
                                        Some(TypeScriptDirectoryEntryKind::Directory) => {
                                            directories.push(name.to_owned());
                                        }
                                        Some(TypeScriptDirectoryEntryKind::Symlink)
                                        | Some(TypeScriptDirectoryEntryKind::Other)
                                        | None => {}
                                    }
                                }
                                files.sort_unstable_by(|left, right| {
                                    typescript_string_cmp(left, right)
                                });
                                directories.sort_unstable_by(|left, right| {
                                    typescript_string_cmp(left, right)
                                });
                                if files != view.files || directories != view.directories {
                                    return Err(closure_mismatch(&format!(
                                        "TypeScript enumeration differs from admitted snapshot at {:?}",
                                        view.path
                                    )));
                                }
                            }
                            None => {
                                if view.read_succeeded
                                    || !view.files.is_empty()
                                    || !view.directories.is_empty()
                                {
                                    return Err(closure_mismatch(&format!(
                                        "TypeScript reported directory entries for absent or non-directory path {:?}",
                                        view.path
                                    )));
                                }
                            }
                        }
                    }
                }
                for entry in entries {
                    resolver.charge_closure_replay_work(1, "directory result verification")?;
                    let entry_path = Path::new(entry);
                    if !entry_path.is_absolute()
                        || normalize_path(entry_path).to_string_lossy() != entry.as_str()
                    {
                        return Err(closure_mismatch(
                            "compiler directory query returned a non-canonical path",
                        ));
                    }
                    let admitted = if access.kind == ProgramAccessKind::ReadDirectory {
                        access_files.contains(&normalize_path(entry_path))
                    } else {
                        entry_path.parent() == Some(path)
                            && access_directories.contains(&normalize_path(entry_path))
                    };
                    if !admitted {
                        return Err(closure_mismatch(&format!(
                            "compiler directory result {entry:?} is absent from its exact enumerated views"
                        )));
                    }
                }
            }
        }
    }
    if referenced_views.len() != view_by_path.len() {
        return Err(closure_mismatch(
            "compiler returned an unreferenced directory enumeration view",
        ));
    }
    Ok(resolver.closure_replay_work_units())
}

fn validate_matcher_transcript(
    access: &ProgramAccess,
    entries: &[String],
) -> Result<(), TypeScriptProjectHostError> {
    if entries
        .windows(2)
        .any(|pair| typescript_string_cmp(&pair[0], &pair[1]) != std::cmp::Ordering::Less)
    {
        return Err(closure_mismatch(
            "compiler directory query result is not a sorted unique full matcher result",
        ));
    }
    let replay_digest = access.matcher_replay_sha256.as_deref().ok_or_else(|| {
        closure_mismatch("compiler directory query omitted full-result matchFiles replay evidence")
    })?;
    if matcher_path_list_digest(entries) != replay_digest {
        return Err(closure_mismatch(
            "compiler directory query result differs from its complete matchFiles replay",
        ));
    }
    if access.kind == ProgramAccessKind::GetDirectories {
        if !access.matcher_arguments.is_empty() {
            return Err(closure_mismatch(
                "compiler getDirectories transcript contains unsupported extra arguments",
            ));
        }
        return Ok(());
    }
    if access.matcher_arguments.len() > 4 {
        return Err(closure_mismatch(
            "compiler readDirectory transcript contains unsupported trailing arguments",
        ));
    }
    validate_matcher_patterns(
        access.matcher_arguments.first(),
        access.extensions.as_deref(),
        "extensions",
    )?;
    validate_matcher_patterns(
        access.matcher_arguments.get(1),
        access.excludes.as_deref(),
        "excludes",
    )?;
    validate_matcher_patterns(
        access.matcher_arguments.get(2),
        access.includes.as_deref(),
        "includes",
    )?;
    match access.matcher_arguments.get(3) {
        None | Some(ProgramMatcherArgument::Undefined) => {
            if access.recursive != Some(true) || access.depth.is_some() {
                return Err(closure_mismatch(
                    "compiler readDirectory transcript changed omitted depth semantics",
                ));
            }
        }
        Some(ProgramMatcherArgument::Null) => {
            if access.recursive != Some(false) || access.depth.is_some() {
                return Err(closure_mismatch(
                    "compiler readDirectory transcript changed null depth semantics",
                ));
            }
        }
        Some(ProgramMatcherArgument::Value(value)) => {
            let depth = value.as_u64().and_then(|depth| usize::try_from(depth).ok());
            if depth.is_none() || access.recursive != Some(false) || access.depth != depth {
                return Err(closure_mismatch(
                    "compiler readDirectory transcript changed exact depth value",
                ));
            }
        }
    }
    Ok(())
}

fn validate_matcher_patterns(
    argument: Option<&ProgramMatcherArgument>,
    captured: Option<&[String]>,
    label: &str,
) -> Result<(), TypeScriptProjectHostError> {
    let matches = match (argument, captured) {
        (None | Some(ProgramMatcherArgument::Undefined | ProgramMatcherArgument::Null), None) => {
            true
        }
        (Some(ProgramMatcherArgument::Value(serde_json::Value::Array(values))), Some(captured)) => {
            values.len() == captured.len()
                && values
                    .iter()
                    .zip(captured)
                    .all(|(value, captured)| value.as_str() == Some(captured.as_str()))
        }
        _ => false,
    };
    if !matches {
        return Err(closure_mismatch(&format!(
            "compiler readDirectory transcript changed exact {label} argument"
        )));
    }
    Ok(())
}

fn matcher_path_list_digest(entries: &[String]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"compiler.typescript.matchFiles.result.v1|");
    for entry in entries {
        digest.update(&(entry.len() as u64).to_le_bytes());
        digest.update(entry.as_bytes());
    }
    hex_digest(&digest.finalize())
}

fn typescript_string_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

fn closure_limit(phase: &str, observed: usize, maximum: usize) -> TypeScriptProjectHostError {
    TypeScriptProjectHostError::CompilerIoClosureLimit {
        phase: phase.into(),
        detail: format!("observed {observed}, maximum {maximum}").into_boxed_str(),
    }
}

fn closure_mismatch(detail: &str) -> TypeScriptProjectHostError {
    TypeScriptProjectHostError::CompilerIoClosureMismatch {
        detail: detail.into(),
    }
}
fn resolve_virtual_path(
    inputs: &TypeScriptProjectInputs<'_>,
    resolver: &mut TypeScriptResolverCapability<'_>,
    paths: &BTreeMap<PathBuf, String>,
    path: &str,
) -> Result<String, TypeScriptProjectHostError> {
    let Some(source) = resolver.try_load_source(Path::new(path))? else {
        return Err(bridge_error(&format!(
            "compiler module importer {path:?} is not admitted"
        )));
    };
    if let Some(virtual_path) = paths.get(&normalize_path(&source.path)) {
        return Ok(virtual_path.clone());
    }
    workspace_virtual_path(inputs, &source.path)
}

fn workspace_virtual_path(
    inputs: &TypeScriptProjectInputs<'_>,
    path: &Path,
) -> Result<String, TypeScriptProjectHostError> {
    let relative = path.strip_prefix(inputs.workspace_root).map_err(|_| {
        bridge_error(&format!(
            "TypeScript program source {:?} is outside its admitted workspace",
            path
        ))
    })?;
    let mut components = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(value) = component else {
            return Err(bridge_error(
                "TypeScript program path is not a normalized workspace-relative path",
            ));
        };
        let value = value
            .to_str()
            .ok_or_else(|| bridge_error("TypeScript source path is not portable UTF-8"))?;
        components.push(value);
    }
    if components.is_empty() {
        return Err(bridge_error(
            "TypeScript program path names the workspace root",
        ));
    }
    Ok(components.join("/"))
}

fn program_environment_fingerprint(
    inputs: &TypeScriptProjectInputs<'_>,
    config: Option<&super::typescript_host::TypeScriptConfigInput>,
    compiler_version: &str,
    options: &serde_json::Value,
    content_ids: &BTreeMap<String, ContentId<SourceFactDomain>>,
    compiler_api_content_id: &ContentId<SourceFactDomain>,
    compiler_access_digest: &[u8; 32],
    resolutions: &[TszProjectModuleResolution],
    file_formats: &[TszProjectFileFormatInput],
    resolver_digest: &[u8; 32],
) -> Result<TszEnvironmentFingerprint, TypeScriptProjectHostError> {
    let mut digest = blake3::Hasher::new();
    digest.update(b"compiler.typescript.tsz-program.v3\0");
    digest.update(&inputs.fingerprint);
    digest.update(compiler_version.as_bytes());
    match config {
        Some(config) => {
            digest.update(&[1]);
            digest.update(config.content_id.as_ref());
        }
        None => {
            digest.update(&[2]);
        }
    }
    digest.update(compiler_api_content_id.as_ref());
    digest.update(compiler_access_digest);
    digest.update(resolver_digest);
    let options = serde_json::to_vec(options).map_err(|error| {
        bridge_error(&format!(
            "normalized options could not be fingerprinted: {error}"
        ))
    })?;
    digest.update(&(options.len() as u64).to_le_bytes());
    digest.update(&options);
    for (path, content) in content_ids {
        digest.update(&(path.len() as u64).to_le_bytes());
        digest.update(path.as_bytes());
        digest.update(content.as_ref());
    }
    for resolution in resolutions {
        let encoded = format!("{resolution:?}");
        digest.update(&(encoded.len() as u64).to_le_bytes());
        digest.update(encoded.as_bytes());
    }
    digest.update(b"compiler.source-file.implied-node-format.v1\0");
    digest.update(&(file_formats.len() as u64).to_le_bytes());
    for input in file_formats {
        digest.update(&(input.path.len() as u64).to_le_bytes());
        digest.update(input.path.as_bytes());
        digest.update(&[match input.format {
            TszProjectFileFormat::Unknown => 0,
            TszProjectFileFormat::CommonJs => 1,
            TszProjectFileFormat::EsModule => 2,
        }]);
    }
    Ok(TszEnvironmentFingerprint::from_sha256(
        *digest.finalize().as_bytes(),
    ))
}

fn compiler_access_digest(
    accesses: &[ProgramAccess],
    directory_views: &[ProgramDirectoryView],
    metrics: ProgramClosureMetrics,
) -> Result<[u8; 32], TypeScriptProjectHostError> {
    let mut digest = Sha256::new();
    digest.update(b"compiler.typescript.api-io-closure.v3\0");
    for value in [
        metrics.access_operations,
        metrics.directory_queries,
        metrics.directory_enumerations,
        metrics.directory_visited_entries,
        metrics.directory_verification_entries,
        metrics.matcher_result_entries,
        metrics.matcher_pattern_items,
        metrics.matcher_pattern_bytes,
        metrics.directory_query_cache_bytes,
    ] {
        digest.update(&(value as u64).to_le_bytes());
    }
    for access in accesses {
        let encoded = serde_json::to_vec(access).map_err(|error| {
            bridge_error(&format!(
                "Compiler API filesystem transcript failed to encode: {error}"
            ))
        })?;
        digest.update(&(encoded.len() as u64).to_le_bytes());
        digest.update(&encoded);
    }
    for view in directory_views {
        let encoded = serde_json::to_vec(view).map_err(|error| {
            bridge_error(&format!(
                "Compiler API directory view failed to encode: {error}"
            ))
        })?;
        digest.update(&(encoded.len() as u64).to_le_bytes());
        digest.update(&encoded);
    }
    Ok(digest.finalize().into())
}

/// Work admitted to one native TypeScript program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProgramWorkAllowance {
    /// Everything parse, bind, merge, checker planning, the application files'
    /// checks, and every lazy semantic query of this program may spend.
    pub(crate) program: u64,
    /// What one application file's semantic check, or one file's lowering
    /// queries, may spend before that file alone becomes a coverage gap.
    pub(crate) per_file: u64,
}

/// The measured shape of one admitted program: what its allowance is derived from.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ProgramWorkShape {
    /// Program members that are not declaration files; these are semantically
    /// checked and lowered, so they carry the program's real work.
    pub(crate) application_files: u64,
    /// Bytes of those application files.
    pub(crate) application_bytes: u64,
    /// Bytes of declaration sources, default libraries, and captured
    /// configuration. They are parsed, bound, and merged, but only the parts
    /// that application code reaches are ever resolved.
    pub(crate) declaration_bytes: u64,
    /// Replay work of the compiler API's filesystem transcript.
    pub(crate) closure_replay_work: u64,
}

impl ProgramWorkShape {
    // Calibrated on measured real projects. Units admitted for a complete publication:
    // Angular 19 starter 17M (4 application files), Next.js 15 app 6M (6 files).
    // Parse, bind and merge cost about 0.2 units per declaration byte. Checking and
    // lowering cost is set by the application files and by how much library surface
    // they touch (the first file to touch Angular core pays about 12M), so every
    // application file carries its own generous term (an Angular 21 RealWorld app
    // spent 11 to 15M per file, 459M units for its first 30 files). The allowance is
    // a runaway guard with headroom, not a prediction.
    const BASE_UNITS: u64 = 150_000_000;
    const DECLARATION_UNITS_PER_BYTE: u64 = 8;
    const APPLICATION_UNITS_PER_BYTE: u64 = 1_000;
    const APPLICATION_FILE_UNITS: u64 = 20_000_000;
    const CLOSURE_REPLAY_UNITS: u64 = 256;
    /// A single file may spend at most this share of its program's allowance...
    const FILE_SHARE_DENOMINATOR: u64 = 8;
    /// ...but never less than this, so that a small program cannot starve a
    /// file that legitimately resolves a large generic library.
    const MIN_FILE_UNITS: u64 = 60_000_000;

    /// Allowances for this shape, never above the global project cap.
    pub(crate) fn allowance(self) -> ProgramWorkAllowance {
        let program = Self::BASE_UNITS
            .saturating_add(
                self.declaration_bytes
                    .saturating_mul(Self::DECLARATION_UNITS_PER_BYTE),
            )
            .saturating_add(
                self.application_bytes
                    .saturating_mul(Self::APPLICATION_UNITS_PER_BYTE),
            )
            .saturating_add(
                self.application_files
                    .saturating_mul(Self::APPLICATION_FILE_UNITS),
            )
            .saturating_add(
                self.closure_replay_work
                    .saturating_mul(Self::CLOSURE_REPLAY_UNITS),
            )
            .min(MAX_PROJECT_WORK_UNITS);
        let per_file = (program / Self::FILE_SHARE_DENOMINATOR)
            .max(Self::MIN_FILE_UNITS)
            .min(program);
        ProgramWorkAllowance { program, per_file }
    }
}

/// Whether a program path names a declaration file (`.d.ts`, `.d.mts`, `.d.cts`,
/// or an arbitrary-extension `.d.<ext>.ts`). Such files are parsed and bound but
/// carry no application work of their own.
fn is_declaration_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    [".ts", ".mts", ".cts"]
        .iter()
        .filter_map(|suffix| name.strip_suffix(suffix))
        .any(|stem| stem.ends_with(".d") || stem.contains(".d."))
}

fn calculate_work_allowance(
    inputs: &TypeScriptProjectInputs<'_>,
    sources: &[TszFileInput],
    libraries: &[std::sync::Arc<backend_frontend_typescript::TszLibFile>],
    closure_work_units: u64,
) -> ProgramWorkAllowance {
    let mut shape = ProgramWorkShape {
        closure_replay_work: closure_work_units,
        ..ProgramWorkShape::default()
    };
    for source in sources {
        let bytes = source.source.len() as u64;
        if is_declaration_path(&source.path) {
            shape.declaration_bytes = shape.declaration_bytes.saturating_add(bytes);
        } else {
            shape.application_files = shape.application_files.saturating_add(1);
            shape.application_bytes = shape.application_bytes.saturating_add(bytes);
        }
    }
    for library in libraries {
        let bytes = library
            .arena
            .get_source_file_at(library.root_index)
            .map_or(0, |source| source.text.len() as u64);
        shape.declaration_bytes = shape.declaration_bytes.saturating_add(bytes);
    }
    for config in inputs.config_candidates.iter() {
        shape.declaration_bytes = shape
            .declaration_bytes
            .saturating_add(config.bytes.len() as u64);
    }
    shape.allowance()
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR_STR),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::Normal(value) => normalized.push(value),
        }
    }
    normalized
}

/// The selected Compiler API hashes `sys.readFile` text, whereas the resolver
/// retains and content-addresses the exact raw file. Its UTF-8 read removes one
/// leading BOM. Keep that text receipt separate from the raw source identity;
/// unsupported encodings and malformed UTF-8 remain inadmissible.
fn compiler_api_utf8_text_sha256(bytes: &[u8]) -> Result<String, TypeScriptProjectHostError> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| bridge_error("a compiler filesystem source is not UTF-8"))?;
    Ok(sha256_hex(
        text.strip_prefix('\u{feff}').unwrap_or(text).as_bytes(),
    ))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_digest(&Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::{
        ProgramAccess, ProgramClosureMetrics, ProgramDirectoryView, ProgramMatcherArgument,
        ProgramPathResolution, build_native_inputs, compiler_access_digest,
        matcher_path_list_digest, normalize_compiler_api_lib_names, validate_matcher_transcript,
    };
    use serde_json::json;

    #[test]
    fn retained_transcript_charges_duplicate_paths_capacity_and_nested_allocations() {
        let mut access: ProgramAccess = serde_json::from_value(json!({
            "kind": "file", "path": "/same/path.ts"
        }))
        .unwrap();
        let before = super::retained_slice_bytes(
            std::slice::from_ref(&access),
            ProgramAccess::retained_bytes,
        );
        let path_capacity = access.path.capacity();
        access.path.reserve(4096);
        let after = super::retained_slice_bytes(
            std::slice::from_ref(&access),
            ProgramAccess::retained_bytes,
        );
        assert_eq!(
            after - before,
            access.path.capacity() - path_capacity,
            "String spare capacity is owned even when its path value is unchanged"
        );

        access.matcher_arguments = vec![ProgramMatcherArgument::Value(json!({
            "nested": ["**/*.ts", {"exclude": "node_modules/**"}]
        }))];
        access.directory_realpaths = vec![ProgramPathResolution {
            path: "/lexical".into(),
            target: "/canonical".into(),
            succeeded: true,
        }];
        access.entries = Some(vec!["/same/path.ts".into()]);
        access.matcher_replay_sha256 = Some("a".repeat(64).into_boxed_str());
        let one = super::retained_slice_bytes(
            std::slice::from_ref(&access),
            ProgramAccess::retained_bytes,
        );
        let duplicate = access.clone();
        let duplicate_payload = duplicate.retained_bytes();
        let accesses = [access, duplicate];
        let two = super::retained_slice_bytes(&accesses, ProgramAccess::retained_bytes);
        assert_eq!(
            two - one,
            std::mem::size_of::<ProgramAccess>() + duplicate_payload,
            "deduplicating the resolver path cannot remove a separate retained access object"
        );
        assert!(
            one > after + 64,
            "matcher JSON and nested transcript values add owned bytes"
        );

        let mut view = ProgramDirectoryView {
            path: "/same".into(),
            files: vec![],
            directories: vec![],
            read_succeeded: true,
        };
        let empty = view.retained_bytes();
        view.files.reserve(8);
        view.directories.push("child".into());
        assert!(view.retained_bytes() >= empty + 8 * std::mem::size_of::<String>() + "child".len());
        assert!(
            super::retained_slice_bytes(&[view], ProgramDirectoryView::retained_bytes)
                >= std::mem::size_of::<ProgramDirectoryView>()
        );
    }

    #[test]
    fn compiler_text_receipts_remove_only_one_utf8_bom_and_refuse_other_encodings() {
        let text = b"export const value = 1;\n";
        let mut raw = b"\xef\xbb\xbf".to_vec();
        raw.extend_from_slice(text);
        assert_eq!(
            super::compiler_api_utf8_text_sha256(&raw).unwrap(),
            super::sha256_hex(text)
        );
        let mut two_boms = b"\xef\xbb\xbf".to_vec();
        two_boms.extend_from_slice(&raw);
        assert_eq!(
            super::compiler_api_utf8_text_sha256(&two_boms).unwrap(),
            super::sha256_hex(&raw),
            "the second BOM remains compiler text"
        );
        for unsupported in [
            b"\xff\xfev\0".as_slice(),
            b"\xfe\xff\0v".as_slice(),
            b"export const value = '\xff';".as_slice(),
        ] {
            assert!(super::compiler_api_utf8_text_sha256(unsupported).is_err());
        }
    }

    #[test]
    fn bridge_script_pins_the_hosts_negative_module_ancestor_bound() {
        assert!(super::COMPILER_API_PROGRAM_SCRIPT.contains(&format!(
            "const MAX_PROJECT_ANCESTORS = {};",
            super::super::typescript_host::MAX_PROJECT_ANCESTORS
        )));
    }

    #[test]
    #[ignore = "requires pinned Node and TypeScript API paths in TSZ_PROGRAM_TEST_NODE and TSZ_PROGRAM_TEST_TYPESCRIPT_API"]
    fn rooted_specifiers_outside_the_admitted_roots_are_absent_without_observation() {
        use std::{env, fs, path::PathBuf};

        let node = PathBuf::from(
            env::var_os("TSZ_PROGRAM_TEST_NODE").expect("set the pinned Node executable"),
        );
        let compiler_api = PathBuf::from(
            env::var_os("TSZ_PROGRAM_TEST_TYPESCRIPT_API").expect("set the pinned TypeScript API"),
        );
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "nudox-typescript-rooted-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create isolated TypeScript source fixture");
        let fixture = TemporaryProgramRoot(root);
        let root = fixture.0.as_path();
        // Vite's public-directory import: TypeScript treats `/vite.svg` as a rooted
        // module path and probes the filesystem root, which no program may witness.
        fs::write(
            root.join("main.ts"),
            "import logo from '/vite.svg';\nexport const url: string = logo;\n",
        )
        .expect("write rooted import");
        fs::write(
            root.join("assets.d.ts"),
            "declare module '*.svg' { const url: string; export default url; }\n",
        )
        .expect("write ambient svg module");
        fs::write(
            root.join("tsconfig.json"),
            r#"{"compilerOptions":{"module":"esnext","moduleResolution":"bundler","noEmit":true},"include":["*.ts"]}"#,
        )
        .expect("write config");
        let config = fs::canonicalize(root.join("tsconfig.json")).expect("canonical config");
        let canonical_root = fs::canonicalize(root).expect("canonical root");
        let report = run_program_report(&canonical_root, &node, &compiler_api, Some(&config), &[]);
        assert!(report.error.is_none(), "bridge error: {:?}", report.error);
        let compiler_root = compiler_api
            .ancestors()
            .nth(3)
            .expect("module root above typescript/lib/typescript.js")
            .to_path_buf();
        for access in &report.accesses {
            let path = std::path::Path::new(&access.path);
            let negative_candidate = path
                .file_name()
                .is_some_and(|name| name == "node_modules" || name == "@types");
            assert!(
                path.starts_with(&canonical_root)
                    || path.starts_with(&compiler_root)
                    || negative_candidate,
                "outside probe was observed: {}",
                access.path
            );
        }
    }

    struct TemporaryProgramRoot(std::path::PathBuf);

    impl Drop for TemporaryProgramRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn report_contains_source(report: &super::ProgramReport, path: &std::path::Path) -> bool {
        let path = path.to_string_lossy();
        report.files.iter().any(|file| file.path == path.as_ref())
    }

    fn control_contains_source(report: &serde_json::Value, path: &std::path::Path) -> bool {
        let path = path.to_string_lossy();
        report["files"].as_array().is_some_and(|files| {
            files
                .iter()
                .any(|file| file.as_str() == Some(path.as_ref()))
        })
    }

    const JAVASCRIPT_DIAGNOSTICS_CONTROL_SCRIPT: &str = r#"
'use strict';
const ts = require(process.argv[1]);
const rootNames = JSON.parse(process.argv[2]);
const options = {
  ...ts.getDefaultCompilerOptions(),
  allowJs: true,
  checkJs: false,
  jsx: ts.JsxEmit.Preserve,
  noEmit: true
};
const program = ts.createProgram({rootNames, options});
const diagnostics = ts.getPreEmitDiagnostics(program);
process.stdout.write(JSON.stringify({
  files:program.getSourceFiles().map(file => file.fileName),
  diagnostics:diagnostics.map(diagnostic => ({code:diagnostic.code, category:diagnostic.category}))
}));
"#;

    fn run_program_report(
        root: &std::path::Path,
        node: &std::path::Path,
        compiler_api: &std::path::Path,
        config: Option<&std::path::Path>,
        roots: &[std::path::PathBuf],
    ) -> super::ProgramReport {
        use std::{ffi::OsString, num::NonZeroUsize, sync::atomic::AtomicBool, time::Duration};

        use crate::application::{
            ToolchainProbeLimits, toolchain_probe::run_typescript_program_bridge,
        };

        let root_names = roots
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let arguments = [
            OsString::from(compiler_api.as_os_str()),
            config
                .map(|path| OsString::from(path.as_os_str()))
                .unwrap_or_default(),
            OsString::from(root.as_os_str()),
            OsString::from(serde_json::to_string(&root_names).expect("encode test roots")),
        ];
        let output = run_typescript_program_bridge(
            node,
            super::COMPILER_API_PROGRAM_SCRIPT,
            &arguments,
            root,
            ToolchainProbeLimits::new(
                Duration::from_secs(30),
                NonZeroUsize::new(super::MAX_BRIDGE_STDOUT_BYTES).expect("nonzero test output cap"),
            )
            .expect("bounded test probe"),
            &AtomicBool::new(false),
        )
        .expect("run the pinned TypeScript Compiler API fixture");
        serde_json::from_slice(&output).expect("decode the pinned TypeScript program report")
    }

    fn run_javascript_diagnostics_control(
        root: &std::path::Path,
        node: &std::path::Path,
        compiler_api: &std::path::Path,
        roots: &[std::path::PathBuf],
    ) -> serde_json::Value {
        use std::{ffi::OsString, num::NonZeroUsize, sync::atomic::AtomicBool, time::Duration};

        use crate::application::{
            ToolchainProbeLimits, toolchain_probe::run_typescript_program_bridge,
        };

        let root_names = roots
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let arguments = [
            OsString::from(compiler_api.as_os_str()),
            OsString::from(serde_json::to_string(&root_names).expect("encode test roots")),
        ];
        let output = run_typescript_program_bridge(
            node,
            JAVASCRIPT_DIAGNOSTICS_CONTROL_SCRIPT,
            &arguments,
            root,
            ToolchainProbeLimits::new(
                Duration::from_secs(30),
                NonZeroUsize::new(super::MAX_BRIDGE_STDOUT_BYTES).expect("nonzero test output cap"),
            )
            .expect("bounded test probe"),
            &AtomicBool::new(false),
        )
        .expect("run the TypeScript JavaScript diagnostics control");
        serde_json::from_slice(&output).expect("decode TypeScript JavaScript diagnostics")
    }

    #[test]
    #[ignore = "requires pinned Node and TypeScript API paths in TSZ_PROGRAM_TEST_NODE and TSZ_PROGRAM_TEST_TYPESCRIPT_API"]
    fn configless_javascript_roots_are_admitted_without_checking_js() {
        use std::{env, fs, path::PathBuf};

        let node = PathBuf::from(
            env::var_os("TSZ_PROGRAM_TEST_NODE").expect("set the pinned Node executable"),
        );
        let compiler_api = PathBuf::from(
            env::var_os("TSZ_PROGRAM_TEST_TYPESCRIPT_API").expect("set the pinned TypeScript API"),
        );
        use std::time::{SystemTime, UNIX_EPOCH};

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let root = env::temp_dir().join(format!(
            "nudox-typescript-js-control-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create isolated TypeScript source fixture");
        let fixture = TemporaryProgramRoot(root);
        let root = fixture.0.as_path();
        fs::write(root.join("plain.js"), "module.exports = 1;\n").expect("write JavaScript");
        fs::write(
            root.join("view.jsx"),
            "const view = <div data-kind=\"ok\" />;\nmodule.exports = view;\n",
        )
        .expect("write JSX");
        fs::write(root.join("typed.ts"), "export const typed: number = 1;\n")
            .expect("write TypeScript");

        let javascript_roots = [root.join("plain.js"), root.join("view.jsx")];
        let report = run_program_report(root, &node, &compiler_api, None, &javascript_roots);
        assert!(report.error.is_none(), "bridge error: {:?}", report.error);
        let options = &report.compiler_options["compilerOptions"];
        assert_eq!(options["allowJs"], true);
        assert_eq!(
            options["checkJs"], false,
            "inference must not enable JS checking"
        );
        for source in &javascript_roots {
            assert!(report_contains_source(&report, source));
        }

        let diagnostics =
            run_javascript_diagnostics_control(root, &node, &compiler_api, &javascript_roots);
        assert_eq!(diagnostics["diagnostics"], serde_json::json!([]));
        for source in &javascript_roots {
            assert!(control_contains_source(&diagnostics, source));
        }

        let config = root.join("tsconfig.json");
        fs::write(
            &config,
            r#"{"compilerOptions":{"allowJs":false,"strict":true,"target":"ES2020","jsx":"preserve"},"include":["plain.js"]}"#,
        )
        .expect("write explicit JavaScript exclusion config");
        let explicit =
            run_program_report(root, &node, &compiler_api, Some(&config), &javascript_roots);
        assert!(
            explicit.error.is_none(),
            "bridge error: {:?}",
            explicit.error
        );
        let explicit_options = &explicit.compiler_options["compilerOptions"];
        assert_eq!(explicit_options["allowJs"], false);
        assert_eq!(explicit_options["strict"], true);
        assert_eq!(explicit_options["target"], "ES2020");
        assert_eq!(explicit_options["jsx"], "Preserve");
        assert!(!report_contains_source(&explicit, &root.join("plain.js")));

        let typescript_root = [root.join("typed.ts")];
        let typescript_only =
            run_program_report(root, &node, &compiler_api, None, &typescript_root);
        assert!(
            typescript_only.error.is_none(),
            "bridge error: {:?}",
            typescript_only.error
        );
        let typescript_options = &typescript_only.compiler_options["compilerOptions"];
        assert_eq!(typescript_options["allowJs"], false);
        assert_eq!(typescript_options["checkJs"], false);
        assert_eq!(typescript_options["target"], "ES5");
        assert!(report_contains_source(
            &typescript_only,
            &root.join("typed.ts")
        ));
    }

    /// Re-runs TypeScript 5.9's own matcher against the complete directory
    /// snapshots retained from an installed configured project. Every probe
    /// must remain inside that captured closure; an uncaptured traversal is an
    /// error instead of an ambient filesystem fallback.
    const MATCHER_NEGATIVE_PROBE_SCRIPT: &str = r#"
'use strict';
const path = require('path');
const ts = require(process.argv[1]);
const input = JSON.parse(process.argv[2]);
const views = new Map(input.views.map(view => [path.resolve(view.path), view]));
const realpaths = new Map(input.realpaths.filter(item => item.succeeded)
  .map(item => [path.resolve(item.path), item.target]));
const argument = index => {
  const value = input.matcher_arguments[index];
  if (!value || value.kind === 'undefined') return undefined;
  if (value.kind === 'null') return null;
  if (value.kind !== 'value') throw new Error('unknown matcher argument encoding');
  return value.value;
};
const run = (name, overrides = {}) => {
  const extensions = Object.hasOwn(overrides, 'extensions') ? overrides.extensions : argument(0);
  const excludes = Object.hasOwn(overrides, 'excludes') ? overrides.excludes : argument(1);
  const includes = Object.hasOwn(overrides, 'includes') ? overrides.includes : argument(2);
  const depth = Object.hasOwn(overrides, 'depth') ? overrides.depth : argument(3);
  const result = ts.matchFiles(input.path, extensions, excludes, includes,
    input.case_sensitive, input.current_directory, depth, directory => {
      const view = views.get(path.resolve(directory));
      if (!view) throw new Error(`uncaptured directory: ${directory}`);
      if (!view.read_succeeded) throw new Error(`incomplete directory: ${directory}`);
      return {files:view.files, directories:view.directories};
    }, absolutePath => {
      const target = realpaths.get(path.resolve(absolutePath));
      if (!target) throw new Error(`uncaptured realpath: ${absolutePath}`);
      return target;
    });
  return result.map(item => path.isAbsolute(item) ? path.resolve(item) : path.resolve(input.path, item)).sort();
};
const outputs = {
  original:run('original'),
  exclude_all:run('exclude_all', {excludes:['**/*']}),
  include_one_directory:run('include_one_directory', {includes:['src/lib/*.ts']}),
  no_matching_extension:run('no_matching_extension', {extensions:['.__nudox_no_such_extension__']}),
  depth_one:run('depth_one', {depth:1})
};
// This captured in-memory filesystem models Linux's case-sensitive sys setting.
// It deliberately has two names differing only by case and no ambient FS callback.
const caseRoot = '/case-sensitive-fixture';
const caseViews = new Map([[caseRoot, {files:['Widget.ts', 'widget.ts'], directories:[], read_succeeded:true}]]);
const caseSensitiveResult = ts.matchFiles(caseRoot, ['.ts'], undefined, ['**/widget.ts'], true,
  caseRoot, undefined, directory => {
    const view = caseViews.get(path.resolve(directory));
    if (!view) throw new Error(`uncaptured case-sensitive fixture directory: ${directory}`);
    return view;
  }, absolutePath => {
    const normalized = path.resolve(absolutePath);
    if (normalized !== caseRoot) throw new Error(`uncaptured case-sensitive fixture realpath: ${absolutePath}`);
    return normalized;
  });
outputs.linux_case_sensitive = caseSensitiveResult
  .map(item => path.isAbsolute(item) ? path.resolve(item) : path.resolve(caseRoot, item)).sort();
process.stdout.write(JSON.stringify(outputs));
"#;

    #[test]
    fn program_admission_controls_are_typed_before_any_compiler_launch() {
        use super::TypeScriptProjectHostError;
        use std::{
            sync::atomic::AtomicBool,
            time::{Duration, Instant},
        };
        let future = Instant::now() + Duration::from_secs(30);
        assert!(matches!(
            super::check_admission_control(future, &AtomicBool::new(true)),
            Err(TypeScriptProjectHostError::CompilerProgramCancelled)
        ));
        assert!(matches!(
            super::check_admission_control(Instant::now(), &AtomicBool::new(false)),
            Err(TypeScriptProjectHostError::CompilerProgramDeadline)
        ));
        assert!(
            matches!(
                super::check_admission_control(Instant::now(), &AtomicBool::new(true)),
                Err(TypeScriptProjectHostError::CompilerProgramCancelled)
            ),
            "cooperative cancellation retains precedence over elapsed time"
        );
        assert!(super::check_admission_control(future, &AtomicBool::new(false)).is_ok());
    }

    #[test]
    fn forest_child_uses_only_remaining_shared_bridge_allowance() {
        let mut budget = super::ForestAdmissionBudget::default();
        budget.files = super::MAX_BRIDGE_FILES - 1;
        budget.requests = super::MAX_BRIDGE_REQUESTS;
        budget.metrics.access_operations = super::MAX_BRIDGE_ACCESS_OPERATIONS - 3;
        budget.metrics.matcher_pattern_bytes = super::MAX_BRIDGE_MATCHER_PATTERN_BYTES;
        let remaining = budget.child_limits();
        assert_eq!(remaining["files"], 1);
        assert_eq!(remaining["requests"], 0);
        assert_eq!(remaining["access_operations"], 3);
        assert_eq!(remaining["matcher_pattern_bytes"], 0);
        let report: super::ProgramReport = serde_json::from_value(json!({
            "schema":1,"version":"5.9.3","config_path":"","compiler_options":{},
            "root_names":[],"project_references":[],"files":[],"resolutions":[],
            "access_operations":4
        }))
        .unwrap();
        assert!(matches!(
            budget.retain(&report),
            Err(super::TypeScriptProjectHostError::CompilerIoClosureLimit { .. })
        ));
    }

    #[test]
    fn compiler_api_library_filenames_map_to_tsconfig_library_names() {
        let mut options = json!({
            "compilerOptions": {
                "lib": ["lib.dom.d.ts", "lib.dom.iterable.d.ts", "lib.esnext.d.ts"]
            }
        });
        normalize_compiler_api_lib_names(&mut options);
        assert_eq!(
            options["compilerOptions"]["lib"],
            json!(["dom", "dom.iterable", "esnext"])
        );
    }

    #[test]
    fn compiler_api_library_normalizer_preserves_unrecognized_values() {
        let mut options = json!({
            "compilerOptions": {
                "lib": ["DOM", "@typescript/lib-dom", 7]
            }
        });
        normalize_compiler_api_lib_names(&mut options);
        assert_eq!(
            options["compilerOptions"]["lib"],
            json!(["DOM", "@typescript/lib-dom", 7])
        );
    }

    #[test]
    fn compiler_filesystem_operation_set_is_closed() {
        assert!(
            serde_json::from_value::<ProgramAccess>(json!({
                "kind": "unrecognizedOperation",
                "path": "/workspace/app"
            }))
            .is_err()
        );
    }

    #[test]
    fn selected_typescript_path_order_matches_utf16_for_non_bmp_basenames() {
        let astral = "a😀.ts";
        let private_use = "a\u{e000}.ts";
        assert!(private_use < astral, "Rust scalar-value order is different");
        assert_eq!(
            super::typescript_string_cmp(astral, private_use),
            std::cmp::Ordering::Less,
            "TypeScript sorts names by UTF-16 code units"
        );
    }

    #[test]
    fn compiler_directory_transcript_binds_exact_matcher_parameters() {
        let access = |extensions: Option<Vec<&str>>,
                      excludes: Option<Vec<&str>>,
                      includes: Option<Vec<&str>>,
                      depth: Option<usize>,
                      case_sensitive: bool|
         -> ProgramAccess {
            let entries = vec!["/workspace/app/src/a.ts".to_owned()];
            let depth_value = depth;
            ProgramAccess {
                kind: super::ProgramAccessKind::ReadDirectory,
                path: "/workspace/app".to_owned(),
                exists: None,
                sha256: None,
                realpath: None,
                extensions: extensions
                    .clone()
                    .map(|items| items.into_iter().map(str::to_owned).collect()),
                excludes: excludes
                    .clone()
                    .map(|items| items.into_iter().map(str::to_owned).collect()),
                includes: includes
                    .clone()
                    .map(|items| items.into_iter().map(str::to_owned).collect()),
                recursive: Some(depth_value.is_none()),
                depth: depth_value,
                current_directory: Some("/workspace/app".to_owned()),
                use_case_sensitive_file_names: Some(case_sensitive),
                directory_view_paths: vec!["/workspace/app".to_owned()],
                directory_realpaths: vec![ProgramPathResolution {
                    path: "/workspace/app".to_owned(),
                    target: "/workspace/app".to_owned(),
                    succeeded: true,
                }],
                matcher_arguments: vec![
                    extensions
                        .as_ref()
                        .map(|items| ProgramMatcherArgument::Value(json!(items)))
                        .unwrap_or(ProgramMatcherArgument::Undefined),
                    excludes
                        .as_ref()
                        .map(|items| ProgramMatcherArgument::Value(json!(items)))
                        .unwrap_or(ProgramMatcherArgument::Undefined),
                    includes
                        .as_ref()
                        .map(|items| ProgramMatcherArgument::Value(json!(items)))
                        .unwrap_or(ProgramMatcherArgument::Undefined),
                    depth_value
                        .map(|depth| ProgramMatcherArgument::Value(json!(depth)))
                        .unwrap_or(ProgramMatcherArgument::Undefined),
                ],
                matcher_replay_sha256: Some(matcher_path_list_digest(&entries).into_boxed_str()),
                entries: Some(entries),
            }
        };
        let metrics = ProgramClosureMetrics {
            access_operations: 1,
            directory_queries: 1,
            directory_enumerations: 1,
            directory_visited_entries: 2,
            directory_verification_entries: 2,
            matcher_result_entries: 3,
            matcher_pattern_items: 4,
            matcher_pattern_bytes: 25,
            directory_query_cache_bytes: 128,
        };
        let view = ProgramDirectoryView {
            path: "/workspace/app".to_owned(),
            files: vec!["a.ts".to_owned()],
            directories: vec!["src".to_owned()],
            read_succeeded: true,
        };
        let original = compiler_access_digest(
            &[access(
                Some(vec![".ts"]),
                Some(vec!["**/node_modules/**"]),
                Some(vec!["**/*"]),
                None,
                true,
            )],
            std::slice::from_ref(&view),
            metrics,
        )
        .expect("hash exact compiler directory transcript");
        let exact = access(
            Some(vec![".ts"]),
            Some(vec!["**/node_modules/**"]),
            Some(vec!["**/*"]),
            None,
            true,
        );
        validate_matcher_transcript(&exact, exact.entries.as_deref().unwrap())
            .expect("retain the exact complete TypeScript matcher result");
        let mut changed_result = exact.clone();
        changed_result
            .entries
            .as_mut()
            .unwrap()
            .push("/workspace/app/src/excluded.ts".to_owned());
        changed_result
            .entries
            .as_mut()
            .unwrap()
            .sort_unstable_by(|left, right| super::typescript_string_cmp(left, right));
        assert!(
            validate_matcher_transcript(
                &changed_result,
                changed_result.entries.as_deref().unwrap()
            )
            .is_err()
        );
        let mut changed_depth = exact.clone();
        changed_depth.matcher_arguments[3] = ProgramMatcherArgument::Value(json!(2));
        assert!(
            validate_matcher_transcript(&changed_depth, changed_depth.entries.as_deref().unwrap())
                .is_err()
        );
        for (index, argument) in [
            (
                0,
                ProgramMatcherArgument::Value(json!([".__missing_extension__"])),
            ),
            (1, ProgramMatcherArgument::Value(json!(["**/*"]))),
            (
                2,
                ProgramMatcherArgument::Value(json!(["**/__missing_glob__/**"])),
            ),
        ] {
            let mut changed = exact.clone();
            changed.matcher_arguments[index] = argument;
            assert!(
                validate_matcher_transcript(&changed, changed.entries.as_deref().unwrap()).is_err()
            );
        }
        let mut missing_result = exact.clone();
        missing_result.entries.as_mut().unwrap().clear();
        assert!(
            validate_matcher_transcript(
                &missing_result,
                missing_result.entries.as_deref().unwrap()
            )
            .is_err()
        );
        let mut duplicate_result = exact.clone();
        duplicate_result
            .entries
            .as_mut()
            .unwrap()
            .push("/workspace/app/src/a.ts".to_owned());
        duplicate_result
            .entries
            .as_mut()
            .unwrap()
            .sort_unstable_by(|left, right| super::typescript_string_cmp(left, right));
        assert!(
            validate_matcher_transcript(
                &duplicate_result,
                duplicate_result.entries.as_deref().unwrap()
            )
            .is_err()
        );
        let changed = [
            access(
                Some(vec![".tsx"]),
                Some(vec!["**/node_modules/**"]),
                Some(vec!["**/*"]),
                None,
                true,
            ),
            access(
                Some(vec![".ts"]),
                Some(Vec::new()),
                Some(vec!["**/*"]),
                None,
                true,
            ),
            access(
                Some(vec![".ts"]),
                Some(vec!["**/node_modules/**"]),
                Some(Vec::new()),
                None,
                true,
            ),
            access(
                Some(vec![".ts"]),
                Some(vec!["**/node_modules/**"]),
                Some(vec!["**/*"]),
                Some(2),
                true,
            ),
            access(
                Some(vec![".ts"]),
                Some(vec!["**/node_modules/**"]),
                Some(vec!["**/*"]),
                None,
                false,
            ),
        ];
        for changed in changed {
            assert_ne!(
                original,
                compiler_access_digest(&[changed], std::slice::from_ref(&view), metrics)
                    .expect("hash changed matcher parameters")
            );
        }
        let mut changed_realpath = access(
            Some(vec![".ts"]),
            Some(vec!["**/node_modules/**"]),
            Some(vec!["**/*"]),
            None,
            true,
        );
        changed_realpath.directory_realpaths[0].target = "/workspace/redirected-app".to_owned();
        assert_ne!(
            original,
            compiler_access_digest(&[changed_realpath], std::slice::from_ref(&view), metrics)
                .expect("hash changed directory realpath witness")
        );
        let changed_results = ProgramDirectoryView {
            path: "/workspace/app".to_owned(),
            files: vec!["a.ts".to_owned(), "hidden.ts".to_owned()],
            directories: vec!["src".to_owned()],
            read_succeeded: true,
        };
        assert_ne!(
            original,
            compiler_access_digest(
                &[access(
                    Some(vec![".ts"]),
                    Some(vec!["**/node_modules/**"]),
                    Some(vec!["**/*"]),
                    None,
                    true
                )],
                &[changed_results],
                metrics,
            )
            .expect("hash changed directory enumeration")
        );
        let changed_work = ProgramClosureMetrics {
            directory_visited_entries: 3,
            ..metrics
        };
        assert_ne!(
            original,
            compiler_access_digest(
                &[access(
                    Some(vec![".ts"]),
                    Some(vec!["**/node_modules/**"]),
                    Some(vec!["**/*"]),
                    None,
                    true
                )],
                std::slice::from_ref(&view),
                changed_work,
            )
            .expect("hash changed closure work accounting")
        );
    }

    #[test]
    #[ignore = "requires a fully installed configured TypeScript corpus and admitted Node path"]
    fn selected_typescript_matcher_negative_probes_stay_inside_captured_next_closure() {
        use std::{
            collections::BTreeSet,
            ffi::OsString,
            num::NonZeroUsize,
            path::{Path, PathBuf},
            sync::atomic::AtomicBool,
            time::{Duration, Instant},
        };

        use crate::application::{
            PackageSource, ToolchainProbeLimits, TypeScriptProjectHost,
            toolchain_probe::run_typescript_program_bridge,
        };

        let root = PathBuf::from(
            std::env::var_os("TSZ_CONFIGURED_CORPUS_ROOT")
                .expect("set TSZ_CONFIGURED_CORPUS_ROOT to the installed Next corpus"),
        );
        let node = PathBuf::from(
            std::env::var_os("TSZ_CONFIGURED_NODE")
                .expect("set TSZ_CONFIGURED_NODE to the admitted Node executable"),
        );
        let source_paths = [
            "src/app/api/route.ts",
            "src/app/page.tsx",
            "src/app/tutorial/page.tsx",
            "src/components/NavBar.tsx",
            "src/lib/scheduler.ts",
        ];
        let source_texts = source_paths
            .iter()
            .map(|path| std::fs::read_to_string(root.join(path)).expect("installed source"))
            .collect::<Vec<_>>();
        let package_sources = source_paths
            .iter()
            .zip(&source_texts)
            .map(|(path, source)| PackageSource::new(path, source).expect("package source"))
            .collect::<Vec<_>>();
        let limits = ToolchainProbeLimits::new(
            Duration::from_secs(900),
            NonZeroUsize::new(1024 * 1024).expect("nonzero output limit"),
        )
        .expect("bounded test probe");
        let host = TypeScriptProjectHost::new(None, Some(node), None, None, limits);
        let admitted = host
            .admit(&root)
            .expect("admit installed configured project")
            .expect("project-local TypeScript installation");
        let inputs = admitted.inputs();
        let mut resolver = inputs.resolver();
        let cancelled = AtomicBool::new(false);
        let native = build_native_inputs(
            &inputs,
            &mut resolver,
            &package_sources,
            Instant::now() + Duration::from_secs(900),
            &cancelled,
        )
        .expect("construct exact configured compiler program and closure");
        native
            .closure_witness
            .validate_current(admitted.witness().as_ref())
            .expect("captured compiler and resolver closure still matches");

        let project_root = super::normalize_path(&root);
        let access = native
            .closure_witness
            .compiler_accesses
            .iter()
            .find(|access| {
                access.kind == super::ProgramAccessKind::ReadDirectory
                    && super::normalize_path(Path::new(&access.path)) == project_root
                    && access
                        .entries
                        .as_ref()
                        .is_some_and(|entries| !entries.is_empty())
            })
            .expect("configured project root readDirectory query");
        assert!(
            access.use_case_sensitive_file_names.is_some(),
            "use the selected TypeScript sys value rather than host assumptions"
        );
        assert_eq!(
            access.current_directory.as_deref(),
            Some(access.path.as_str())
        );

        let mut changed_views = native.closure_witness.directory_views.to_vec();
        let changed_view = changed_views
            .iter_mut()
            .find(|view| access.directory_view_paths.contains(&view.path))
            .expect("one captured view from the exact matcher query");
        changed_view
            .files
            .push("unexpected-from-mutated-snapshot.ts".to_owned());
        changed_view
            .files
            .sort_unstable_by(|left, right| super::typescript_string_cmp(left, right));
        let mut changed_resolver = inputs.resolver();
        assert!(matches!(
            super::replay_observations(
                &mut changed_resolver,
                &native.closure_witness.compiler_accesses,
                &changed_views,
                native.closure_witness.metrics
            ),
            Err(crate::application::typescript_host::TypeScriptProjectHostError::CompilerIoClosureMismatch { .. })
        ));

        let mut undercounted_metrics = native.closure_witness.metrics;
        undercounted_metrics.directory_visited_entries = 0;
        let mut undercounted_resolver = inputs.resolver();
        assert!(matches!(
            super::replay_observations(
                &mut undercounted_resolver,
                &native.closure_witness.compiler_accesses,
                &native.closure_witness.directory_views,
                undercounted_metrics
            ),
            Err(crate::application::typescript_host::TypeScriptProjectHostError::CompilerIoClosureMismatch { .. })
        ));

        let view_paths = access
            .directory_view_paths
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        let views = native
            .closure_witness
            .directory_views
            .iter()
            .filter(|view| view_paths.contains(view.path.as_str()))
            .map(|view| {
                json!({
                    "path": view.path,
                    "files": view.files,
                    "directories": view.directories,
                    "read_succeeded": view.read_succeeded
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(
            views.len(),
            view_paths.len(),
            "retain each complete query view"
        );
        let matcher_input = json!({
            "path": access.path,
            "current_directory": access.current_directory,
            "case_sensitive": access.use_case_sensitive_file_names,
            "matcher_arguments": access.matcher_arguments,
            "views": views,
            "realpaths": access.directory_realpaths,
        });
        let serialized = serde_json::to_string(&matcher_input).expect("serialize exact snapshots");
        let compiler_api_path = inputs
            .compiler_api()
            .unwrap()
            .typescript_module_root
            .join("typescript/lib/typescript.js");
        let arguments = [
            OsString::from(compiler_api_path.as_os_str()),
            OsString::from(serialized),
        ];
        let output = run_typescript_program_bridge(
            inputs.compiler_api().unwrap().node_path,
            MATCHER_NEGATIVE_PROBE_SCRIPT,
            &arguments,
            &root,
            ToolchainProbeLimits::new(
                Duration::from_secs(30),
                NonZeroUsize::new(1024 * 1024).expect("nonzero probe output limit"),
            )
            .expect("bounded matcher probe"),
            &cancelled,
        )
        .expect("selected TypeScript matchFiles probes");
        let output: serde_json::Value =
            serde_json::from_slice(&output).expect("matcher probe JSON output");
        assert_eq!(
            output["original"],
            json!(access.entries.as_ref().expect("complete original query")),
            "the selected runtime reproduces the captured full result"
        );
        assert_eq!(output["exclude_all"], json!([]));
        assert_eq!(
            output["include_one_directory"],
            json!([project_root
                .join("src/lib/scheduler.ts")
                .to_string_lossy()
                .to_string()])
        );
        assert_eq!(output["no_matching_extension"], json!([]));
        // TypeScript decrements depth before checking its stop condition, so
        // depth=1 is the exact boundary that visits the root and skips children.
        assert_eq!(output["depth_one"], json!([]));
        assert_eq!(
            output["linux_case_sensitive"],
            json!(["/case-sensitive-fixture/widget.ts"]),
            "explicit Linux-style case-sensitive sys value excludes differently-cased name"
        );
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn bridge_error(message: &str) -> TypeScriptProjectHostError {
    TypeScriptProjectHostError::CompilerApiBridge {
        message: message.into(),
    }
}

#[cfg(all(test, unix))]
#[path = "typescript_program/membership_tests.rs"]
mod membership_tests;

#[cfg(test)]
mod work_allowance_tests {
    use super::{MAX_PROJECT_WORK_UNITS, ProgramWorkShape, is_declaration_path};

    #[test]
    fn declaration_paths_are_recognised_in_every_spelling_and_application_paths_are_not() {
        for declaration in [
            "node_modules/@angular/core/index.d.ts",
            "src/types/globals.d.ts",
            "lib/esm/index.d.mts",
            "lib/cjs/index.d.cts",
            "src/assets/logo.d.svg.ts",
            "index.d.ts",
        ] {
            assert!(is_declaration_path(declaration), "{declaration}");
        }
        for application in [
            "src/main.ts",
            "src/app/app.component.tsx",
            "src/dts.ts",
            "src/odd.d.txt",
            "src/mod.mts",
            "src/index.cts",
            "src/model.d/x.ts",
            "declare.ts",
        ] {
            assert!(!is_declaration_path(application), "{application}");
        }
    }

    #[test]
    fn an_empty_program_still_gets_a_working_floor_and_a_file_slice_within_it() {
        let allowance = ProgramWorkShape::default().allowance();
        assert!(allowance.program >= ProgramWorkShape::BASE_UNITS);
        assert!(allowance.per_file >= 1);
        assert!(allowance.per_file <= allowance.program);
    }

    #[test]
    fn work_scales_with_application_files_not_with_dependency_declarations() {
        let declarations_only = ProgramWorkShape {
            declaration_bytes: 50 * 1024 * 1024,
            ..ProgramWorkShape::default()
        }
        .allowance();
        let one_more_application_file = ProgramWorkShape {
            declaration_bytes: 50 * 1024 * 1024,
            application_files: 1,
            application_bytes: 4 * 1024,
            ..ProgramWorkShape::default()
        }
        .allowance();
        assert!(
            one_more_application_file.program
                >= declarations_only.program + ProgramWorkShape::APPLICATION_FILE_UNITS,
            "every application file adds its own admitted work"
        );
        // 50 MiB of dependency typings add parse/bind cost, not application-sized work.
        assert!(
            declarations_only.program - ProgramWorkShape::BASE_UNITS
                <= 50 * 1024 * 1024 * ProgramWorkShape::DECLARATION_UNITS_PER_BYTE
        );
    }

    #[test]
    fn a_files_slice_is_a_bounded_share_of_the_program_and_never_below_the_floor() {
        let small = ProgramWorkShape {
            application_files: 3,
            ..ProgramWorkShape::default()
        }
        .allowance();
        assert_eq!(
            small.per_file,
            ProgramWorkShape::MIN_FILE_UNITS.min(small.program)
        );
        let large = ProgramWorkShape {
            application_files: 4_000,
            application_bytes: 40 * 1024 * 1024,
            declaration_bytes: 64 * 1024 * 1024,
            ..ProgramWorkShape::default()
        }
        .allowance();
        assert_eq!(
            large.per_file,
            (large.program / ProgramWorkShape::FILE_SHARE_DENOMINATOR)
                .max(ProgramWorkShape::MIN_FILE_UNITS)
        );
        assert!(
            large.per_file < large.program,
            "one hot file can never consume the whole program"
        );
    }

    #[test]
    fn the_global_cap_bounds_every_allowance_without_overflow() {
        let absurd = ProgramWorkShape {
            application_files: u64::MAX,
            application_bytes: u64::MAX,
            declaration_bytes: u64::MAX,
            closure_replay_work: u64::MAX,
        }
        .allowance();
        assert_eq!(absurd.program, MAX_PROJECT_WORK_UNITS);
        assert!(absurd.per_file <= absurd.program);
    }
}
