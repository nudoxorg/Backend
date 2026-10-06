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
    time::{Duration, Instant},
};

use backend_frontend_typescript::{
    TszCheckerOptions, TszEnvironmentFingerprint, TszFileInput, TszLibraryInput,
    TszProjectModuleRequestKind, TszProjectModuleResolution, TszProjectModuleResolutionTarget,
    TszProjectOptions, TszProjectSemanticOptions, checker_options_from_compiler_api_json,
};
use backend_version::{ContentId, SourceFactDomain};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::typescript_host::{
    TypeScriptDirectoryEntryKind, TypeScriptProjectHostError, TypeScriptProjectInputs,
    TypeScriptResolverCapability, TypeScriptResolverWitness,
};
use super::{ToolchainProbeLimits, toolchain_probe::run_typescript_program_bridge};
use crate::application::compiler::PackageSource;

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
const MAX_CLOSURE_REPLAY_WORK_UNITS: u64 = 2_000_000;
const MAX_PROJECT_WORK_UNITS: u64 = 16_000_000_000;
const LIB_VIRTUAL_PREFIX: &str = "@compiler/lib.";

/// Fully admitted source/lib/options set produced by the selected compiler API.
pub(crate) struct NativeTypeScriptInputs {
    pub(crate) sources: Vec<TszFileInput>,
    pub(crate) libraries: Vec<std::sync::Arc<backend_frontend_typescript::TszLibFile>>,
    pub(crate) options: TszProjectOptions,
    pub(crate) closure_witness: TypeScriptProgramClosureWitness,
    /// Package-relative source path to the exact workspace-root TSZ path.
    pub(crate) package_paths: BTreeMap<Box<str>, Box<str>>,
    pub(crate) work_units: u64,
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
    pub(crate) fn validate_current(
        &self,
        witness: &super::typescript_host::TypeScriptProjectWitness,
    ) -> Result<(), TypeScriptProjectHostError> {
        if compiler_access_digest(&self.compiler_accesses, &self.directory_views, self.metrics)?
            != self.access_digest
        {
            return Err(bridge_error(
                "retained Compiler API directory transcript changed after construction",
            ));
        }
        self.resolver.validate_current(witness)
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramFile {
    path: String,
    sha256: String,
    default_library: bool,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
const observations = new Map();
const directoryViews = new Map();
const directoryEntryCache = new Map();
const directoryQueryCache = new Map();
const MAX_ACCESS_OPERATIONS = 131072;
const MAX_DIRECTORY_QUERIES = 16384;
const MAX_DIRECTORY_ENUMERATIONS = 65536;
const MAX_DIRECTORY_ENTRIES = 65536;
const MAX_ENTRIES_PER_DIRECTORY = 16384;
const MAX_MATCHER_RESULT_ENTRIES = 65536;
const MAX_MATCHER_RESULT_WORK = 196608;
const MAX_MATCHER_PATTERN_ITEMS = 131072;
const MAX_MATCHER_PATTERN_BYTES = 512 * 1024;
const MAX_DIRECTORY_QUERY_CACHE_BYTES = 1024 * 1024;
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
  if (typeof ts.matchFiles !== 'function') throw closureFault('mismatch', 'selected TypeScript runtime does not expose its exact matchFiles implementation');
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
  const parsed = ts.getParsedCommandLineOfConfigFile(configPath, {}, parseHost);
  if (!parsed) throw new Error('getParsedCommandLineOfConfigFile returned no project');
  if (parsed.projectReferences && parsed.projectReferences.length) {
    result({schema:1, version:ts.version, config_path:path.resolve(configPath), compiler_options:{compilerOptions:{}},
      project_references:parsed.projectReferences.map(r => r.path), files:[], resolutions:[], accesses:Array.from(observations.values()),
      directory_views:Array.from(directoryViews.values()), access_operations:accessOperations, directory_queries:directoryQueries,
      directory_enumerations:directoryEnumerations, directory_visited_entries:directoryVisitedEntries, directory_verification_entries:directoryVerificationEntries,
      matcher_result_entries:matcherResultEntries,
      matcher_pattern_items:matcherPatternItems, matcher_pattern_bytes:matcherPatternBytes,
      directory_query_cache_bytes:directoryQueryCacheBytes,
      unsupported_options:[], error:'project references are not yet admitted as one TSZ program', failure_kind:null});
  } else {
    const options = parsed.options;
    const host = ts.createCompilerHost(options, true);
    for (const name of ['readFile', 'fileExists', 'directoryExists', 'realpath', 'readDirectory', 'getDirectories']) {
      if (typeof host[name] === 'function' && typeof ts.sys[name] === 'function') host[name] = ts.sys[name].bind(ts.sys);
    }
    const program = ts.createProgram({rootNames:parsed.fileNames, options, host});
    const programFiles = program.getSourceFiles();
    const byPath = new Map(programFiles.map(file => [path.resolve(file.fileName), file]));
    const sourceFiles = programFiles.map(file => ({
      path:path.resolve(file.fileName),
      sha256:digest(file.text),
      default_library:program.isSourceFileDefaultLibrary(file)
    }));
    const resolutions = [];
    const resolutionKeys = new Map();
    const modeFor = (file, literal, kind) => {
      const attributes = literal.parent && literal.parent.attributes;
      if (attributes && Array.isArray(attributes.elements)) {
        for (const item of attributes.elements) {
          if (item.name && item.name.text === 'resolution-mode' && item.value && (item.value.text === 'import' || item.value.text === 'require')) return item.value.text;
        }
      }
      if (kind === 'DynamicImport') return 'import';
      if (kind === 'CjsRequire') return 'require';
      const mr = options.moduleResolution === undefined ? undefined : ts.ModuleResolutionKind[options.moduleResolution];
      if (mr !== 'Node16' && mr !== 'NodeNext') return null;
      const format = file.impliedNodeFormat;
      if (format === ts.ModuleKind.CommonJS) return 'require';
      if (format === ts.ModuleKind.ESNext) return 'import';
      const mod = options.module === undefined ? undefined : ts.ModuleKind[options.module];
      if (mod === 'CommonJS' || mod === 'Node16' && kind === 'CjsRequire') return 'require';
      if (mod === 'NodeNext' || mod === 'ESNext' || mod === 'ES2022' || mod === 'ES2020' || mod === 'ES2015') return 'import';
      return null;
    };
    const addRequest = (file, literal, kind) => {
      if (!literal || typeof literal.text !== 'string') return;
      const resolution = program.getResolvedModuleFromModuleSpecifier(literal, file);
      const resolved = resolution && resolution.resolvedModule;
      const mode = modeFor(file, literal, kind);
      const value = {importer:path.resolve(file.fileName), specifier:literal.text, kind, mode,
        target_path:resolved ? path.resolve(resolved.resolvedFileName) : null,
        package_id:resolved && resolved.packageId ? [resolved.packageId.name, resolved.packageId.version, resolved.packageId.subModuleName || ''].join('/') : null};
      const requestKey = JSON.stringify([value.importer, value.specifier, value.kind, value.mode]);
      const encoded = JSON.stringify(value);
      if (resolutionKeys.has(requestKey) && resolutionKeys.get(requestKey) !== encoded) throw new Error('conflicting compiler resolutions for one request key');
      if (!resolutionKeys.has(requestKey)) { resolutionKeys.set(requestKey, encoded); resolutions.push(value); }
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
    const scriptTarget = options.target === undefined ? ts.ScriptTarget.ES5 : options.target;
    const targetName = reverseName(ts.ScriptTarget, scriptTarget, 'ES5');
    const resolutionValue = options.moduleResolution === undefined
      ? ((options.module === ts.ModuleKind.Node16 || options.module === ts.ModuleKind.NodeNext) ? options.module : ts.ModuleResolutionKind.Node10)
      : options.moduleResolution;
    const moduleValue = options.module === undefined
      ? (resolutionValue === ts.ModuleResolutionKind.Node16 ? ts.ModuleKind.Node16 : resolutionValue === ts.ModuleResolutionKind.NodeNext ? ts.ModuleKind.NodeNext : (scriptTarget >= ts.ScriptTarget.ES2015 ? ts.ModuleKind.ES2015 : ts.ModuleKind.CommonJS))
      : options.module;
    const strictFlag = key => options[key] === undefined ? !!options.strict : !!options[key];
    const normalized = {
      target:targetName,
      module:reverseName(ts.ModuleKind,moduleValue,'CommonJS'),
      moduleResolution:reverseName(ts.ModuleResolutionKind,resolutionValue,'Node10'),
      moduleDetection:reverseName(ts.ModuleDetectionKind,options.moduleDetection, 'Auto'),
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
      experimentalDecorators:!!options.experimentalDecorators, esModuleInterop:!!options.esModuleInterop,
      allowSyntheticDefaultImports:options.allowSyntheticDefaultImports === undefined ? !!options.esModuleInterop : !!options.allowSyntheticDefaultImports,
      resolveJsonModule:!!options.resolveJsonModule, noResolve:!!options.noResolve, allowJs:!!options.allowJs,
      checkJs:!!options.checkJs, skipLibCheck:!!options.skipLibCheck,
      noUncheckedSideEffectImports:!!options.noUncheckedSideEffectImports,
      noTypesAndSymbols:!!options.noTypesAndSymbols,
      useDefineForClassFields:options.useDefineForClassFields,
      jsx:options.jsx === undefined ? undefined : reverseName(ts.JsxEmit,options.jsx,undefined),
      jsxFactory:options.jsxFactory, jsxFragmentFactory:options.jsxFragmentFactory,
      jsxImportSource:options.jsxImportSource, reactNamespace:options.reactNamespace,
      lib:options.lib, types:options.types, typeRoots:options.typeRoots,
      baseUrl:options.baseUrl, paths:options.paths
    };
    const checkerRelevant = new Set(['target','module','moduleResolution','moduleDetection','jsx','jsxFactory','jsxFragmentFactory','jsxImportSource','reactNamespace','lib','types','typeRoots','baseUrl','paths','strict','noImplicitAny','strictNullChecks','strictFunctionTypes','strictBindCallApply','strictPropertyInitialization','noImplicitThis','useUnknownInCatchVariables','alwaysStrict','exactOptionalPropertyTypes','noUncheckedIndexedAccess','noImplicitReturns','noPropertyAccessFromIndexSignature','noImplicitOverride','noUnusedLocals','noUnusedParameters','noFallthroughCasesInSwitch','allowUnreachableCode','allowUnusedLabels','experimentalDecorators','esModuleInterop','allowSyntheticDefaultImports','resolveJsonModule','noResolve','allowJs','checkJs','skipLibCheck','noUncheckedSideEffectImports','noTypesAndSymbols','useDefineForClassFields']);
    const unsupported = [];
    for (const declaration of (ts.optionDeclarations || [])) {
      const name = declaration.name;
      if ((declaration.affectsSemanticDiagnostics || declaration.affectsBindDiagnostics) && options[name] !== undefined && !checkerRelevant.has(name)) unsupported.push(name);
    }
    result({schema:1, version:ts.version, config_path:path.resolve(configPath), compiler_options:{compilerOptions:normalized},
      project_references:[], files:sourceFiles, resolutions, accesses:Array.from(observations.values()),
      directory_views:Array.from(directoryViews.values()), access_operations:accessOperations, directory_queries:directoryQueries,
      directory_enumerations:directoryEnumerations, directory_visited_entries:directoryVisitedEntries, directory_verification_entries:directoryVerificationEntries,
      matcher_result_entries:matcherResultEntries,
      matcher_pattern_items:matcherPatternItems, matcher_pattern_bytes:matcherPatternBytes,
      directory_query_cache_bytes:directoryQueryCacheBytes,
      unsupported_options:Array.from(new Set(unsupported)).sort(), error:null, failure_kind:null});
  }
} catch (error) {
  result({schema:1, version:'', config_path:path.resolve(configPath), compiler_options:{compilerOptions:{}},
    project_references:[], files:[], resolutions:[], accesses:Array.from(observations.values()), directory_views:Array.from(directoryViews.values()),
    access_operations:accessOperations, directory_queries:directoryQueries, directory_enumerations:directoryEnumerations,
    directory_visited_entries:directoryVisitedEntries, directory_verification_entries:directoryVerificationEntries,
    matcher_result_entries:matcherResultEntries,
    matcher_pattern_items:matcherPatternItems, matcher_pattern_bytes:matcherPatternBytes,
    directory_query_cache_bytes:directoryQueryCacheBytes,
    unsupported_options:[], error:String(error && error.message || error), failure_kind:error && error.closureFailureKind || null});
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
pub(crate) fn build_native_inputs(
    inputs: &TypeScriptProjectInputs<'_>,
    resolver: &mut TypeScriptResolverCapability<'_>,
    package_sources: &[PackageSource<'_>],
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<NativeTypeScriptInputs, TypeScriptProjectHostError> {
    let config = select_config(inputs)?;
    // The admitted `compiler_path` is the executable CLI shim (`bin/tsc`). It
    // is probed separately to establish the selected tool identity, but it is
    // not the Compiler API module. Resolve that module only from the exact
    // captured TypeScript package closure so `require` cannot silently select
    // a different install from Node's module search path.
    let compiler_api_path = inputs
        .typescript_module_root
        .join("typescript/lib/typescript.js");
    let compiler_api = inputs
        .typescript_files
        .iter()
        .find(|file| normalize_path(&file.path) == normalize_path(&compiler_api_path))
        .ok_or_else(|| {
            bridge_error("the admitted TypeScript package has no witnessed Compiler API module")
        })?;
    let report = run_program_bridge(
        inputs,
        config.path.as_ref(),
        &compiler_api_path,
        deadline,
        cancelled,
    )?;
    if report.schema != 1 {
        return Err(bridge_error("unsupported compiler API report schema"));
    }
    if let Some(message) = report.error.as_deref() {
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
    let expected_version = std::str::from_utf8(inputs.compiler_version)
        .ok()
        .map(str::trim)
        .and_then(|version| version.strip_prefix("Version ").or(Some(version)));
    if expected_version != Some(report.version.as_str()) {
        return Err(bridge_error(
            "the compiler API version differs from the admitted TypeScript identity",
        ));
    }
    let observed_config = Path::new(&report.config_path);
    if normalize_path(observed_config) != normalize_path(config.path.as_ref()) {
        return Err(bridge_error(
            "the compiler API parsed a different tsconfig than the selected project config",
        ));
    }
    if !report.project_references.is_empty() {
        return Err(bridge_error(
            "project references are not admitted as one TSZ project",
        ));
    }
    if let Some(option) = report.unsupported_options.first() {
        return Err(bridge_error(&format!(
            "compiler option {option:?} affects semantic analysis but is outside the TSZ option model"
        )));
    }
    if report.files.is_empty() || report.files.len() > MAX_BRIDGE_FILES {
        return Err(bridge_error(
            "compiler API program source count is empty or exceeds the admitted bound",
        ));
    }
    if report.resolutions.len() > MAX_BRIDGE_REQUESTS {
        return Err(bridge_error(
            "compiler API module-request count exceeds the admitted bound",
        ));
    }
    let metrics = report.closure_metrics();
    let closure_work_units =
        replay_observations(resolver, &report.accesses, &report.directory_views, metrics)?;

    let checker = compiler_api_checker_options(&report.compiler_options)?;
    let mut virtual_by_canonical = BTreeMap::<PathBuf, String>::new();
    let mut source_by_virtual = BTreeMap::<String, String>::new();
    let mut libraries = Vec::new();
    let mut content_ids = BTreeMap::<String, ContentId<SourceFactDomain>>::new();
    let mut source_paths = BTreeSet::new();

    let mut files = report.files.iter().collect::<Vec<_>>();
    files.sort_unstable_by(|left, right| left.path.cmp(&right.path));
    for file in files {
        let path = Path::new(&file.path);
        let Some(admitted) = resolver.try_load_source(path)? else {
            return Err(bridge_error(&format!(
                "the compiler API included a missing source file {:?}",
                file.path
            )));
        };
        let text = std::str::from_utf8(&admitted.bytes)
            .map_err(|_| bridge_error("a compiler program source is not UTF-8"))?;
        let without_bom = text.strip_prefix('\u{feff}').unwrap_or(text);
        if sha256_hex(without_bom.as_bytes()) != file.sha256 {
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
        if is_library {
            let input = TszLibraryInput::from_utf8(virtual_path, admitted.bytes.to_vec()).map_err(
                |error| bridge_error(&format!("compiler library input was rejected: {error}")),
            )?;
            libraries.push(input.into_lib_file());
        } else {
            source_by_virtual.insert(virtual_path, text.to_owned());
        }
    }

    let mut package_paths = BTreeMap::new();
    for source in package_sources {
        let package_path = source.relative_path();
        let absolute = inputs.package_root.join(package_path);
        let Some(admitted) = resolver.try_load_source(&absolute)? else {
            return Err(bridge_error(&format!(
                "package source {package_path:?} is absent from the admitted compiler program"
            )));
        };
        let canonical = normalize_path(&admitted.path);
        let Some(virtual_path) = virtual_by_canonical.get(&canonical) else {
            return Err(bridge_error(&format!(
                "package source {package_path:?} is absent from the selected tsconfig program"
            )));
        };
        let expected = source.source().as_bytes();
        if admitted.bytes.as_ref() != expected {
            return Err(bridge_error(&format!(
                "package source bytes differ from the compiler program at {package_path:?}"
            )));
        }
        package_paths.insert(package_path.into(), virtual_path.clone().into_boxed_str());
    }

    let mut resolutions = Vec::with_capacity(report.resolutions.len());
    let mut resolution_keys = BTreeSet::new();
    for request in report.resolutions {
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
    let resolver_witness = resolver.seal()?;
    let environment = program_environment_fingerprint(
        inputs,
        config,
        &report.version,
        &report.compiler_options,
        &content_ids,
        &compiler_api.content_id,
        &access_digest,
        &resolutions,
        resolver_witness.digest(),
    )?;
    let work_units = calculate_work_units(inputs, &sources, &libraries, closure_work_units);
    let options = TszProjectOptions {
        checker,
        semantic_options: TszProjectSemanticOptions::declaration_scoped(),
        module_resolutions: resolutions,
        environment,
    };
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
        package_paths,
        work_units,
    })
}

fn run_program_bridge(
    inputs: &TypeScriptProjectInputs<'_>,
    config_path: &Path,
    compiler_api_path: &Path,
    deadline: Instant,
    cancelled: &AtomicBool,
) -> Result<ProgramReport, TypeScriptProjectHostError> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(bridge_error(
            "deadline elapsed before compiler API program construction",
        ));
    }
    let limits = ToolchainProbeLimits::new(
        remaining,
        NonZeroUsize::new(MAX_BRIDGE_STDOUT_BYTES).expect("nonzero bridge stream cap"),
    )
    .map_err(|error| bridge_error(&error.to_string()))?;
    let arguments = [
        OsString::from(compiler_api_path.as_os_str()),
        OsString::from(config_path.as_os_str()),
        OsString::from(inputs.workspace_root.as_os_str()),
    ];
    let output = run_typescript_program_bridge(
        inputs.node_path,
        COMPILER_API_PROGRAM_SCRIPT,
        &arguments,
        inputs.package_root,
        limits,
        cancelled,
    )
    .map_err(|error| bridge_error(&error.to_string()))?;
    serde_json::from_slice(&output).map_err(|error| {
        bridge_error(&format!(
            "compiler API output was not bounded valid JSON: {error}"
        ))
    })
}

fn select_config<'a>(
    inputs: &'a TypeScriptProjectInputs<'_>,
) -> Result<&'a super::typescript_host::TypeScriptConfigInput, TypeScriptProjectHostError> {
    let selected = inputs
        .config_candidates
        .iter()
        .filter(|candidate| candidate.selected_build_config)
        .collect::<Vec<_>>();
    if selected.len() == 1 {
        return Ok(selected[0]);
    }
    if selected.len() > 1 {
        return Err(bridge_error(
            "the host admitted more than one selected TypeScript build config",
        ));
    }
    let mut candidates = inputs
        .config_candidates
        .iter()
        .filter(|candidate| candidate.project_candidate)
        .filter(|candidate| {
            inputs
                .package_root
                .starts_with(candidate.path.parent().unwrap_or(Path::new("/")))
        })
        .collect::<Vec<_>>();
    candidates
        .sort_unstable_by_key(|candidate| std::cmp::Reverse(candidate.path.components().count()));
    match candidates.as_slice() {
        [only] => Ok(*only),
        [first, second, ..]
            if first.path.components().count() == second.path.components().count() =>
        {
            Err(bridge_error(
                "the nearest TypeScript project config is ambiguous",
            ))
        }
        [first, ..] => Ok(*first),
        [] => Err(bridge_error(
            "no admitted TypeScript project config applies to this package",
        )),
    }
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

    let mut work_units = 0_u64;
    let charge = |work_units: &mut u64,
                  units: usize,
                  phase: &str|
     -> Result<(), TypeScriptProjectHostError> {
        let next = work_units.saturating_add(u64::try_from(units).unwrap_or(u64::MAX));
        if next > MAX_CLOSURE_REPLAY_WORK_UNITS {
            return Err(closure_limit(
                phase,
                usize::try_from(next).unwrap_or(usize::MAX),
                usize::try_from(MAX_CLOSURE_REPLAY_WORK_UNITS).unwrap_or(usize::MAX),
            ));
        }
        *work_units = next;
        Ok(())
    };
    charge(
        &mut work_units,
        metrics.access_operations,
        "retained filesystem access",
    )?;
    charge(
        &mut work_units,
        metrics.directory_queries,
        "directory query replay",
    )?;
    charge(
        &mut work_units,
        metrics.directory_enumerations,
        "directory enumeration replay",
    )?;
    charge(
        &mut work_units,
        metrics.directory_visited_entries,
        "directory entry replay",
    )?;
    charge(
        &mut work_units,
        metrics.directory_verification_entries,
        "directory result verification",
    )?;
    charge(
        &mut work_units,
        metrics.matcher_result_entries,
        "matcher result verification",
    )?;
    charge(
        &mut work_units,
        metrics.matcher_pattern_items,
        "matcher pattern item replay",
    )?;
    charge(
        &mut work_units,
        metrics.matcher_pattern_bytes,
        "matcher pattern byte replay",
    )?;
    charge(
        &mut work_units,
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
    charge(
        &mut work_units,
        view_entries,
        "directory snapshot comparison",
    )?;

    let mut referenced_views = BTreeSet::new();
    let mut checked_views = BTreeSet::new();
    let mut checked_realpaths = BTreeMap::<String, (String, bool)>::new();
    for access in accesses {
        charge(&mut work_units, 1, "filesystem access replay")?;
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
                    && sha256_hex(&source.bytes) != expected
                {
                    return Err(closure_mismatch(&format!(
                        "compiler filesystem bytes changed at {:?}",
                        access.path
                    )));
                }
            }
            ProgramAccessKind::Directory => {
                if resolver.directory_exists(path)? != access.exists.unwrap_or(false) {
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
                    charge(&mut work_units, 1, "directory realpath replay")?;
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
                    charge(&mut work_units, 1, "directory snapshot replay")?;
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
                                let snapshot = resolver
                                    .observed_program_directory_entries(&canonical)
                                    .ok_or_else(|| {
                                        closure_mismatch(
                                            "admitted directory has no retained immutable snapshot",
                                        )
                                    })?;
                                let mut files = Vec::new();
                                let mut directories = Vec::new();
                                for entry in snapshot {
                                    charge(
                                        &mut work_units,
                                        1,
                                        "directory snapshot entry comparison",
                                    )?;
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
                    charge(&mut work_units, 1, "directory result verification")?;
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
    Ok(work_units)
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
    config: &super::typescript_host::TypeScriptConfigInput,
    compiler_version: &str,
    options: &serde_json::Value,
    content_ids: &BTreeMap<String, ContentId<SourceFactDomain>>,
    compiler_api_content_id: &ContentId<SourceFactDomain>,
    compiler_access_digest: &[u8; 32],
    resolutions: &[TszProjectModuleResolution],
    resolver_digest: &[u8; 32],
) -> Result<TszEnvironmentFingerprint, TypeScriptProjectHostError> {
    let mut digest = blake3::Hasher::new();
    digest.update(b"compiler.typescript.tsz-program.v1\0");
    digest.update(&inputs.fingerprint);
    digest.update(compiler_version.as_bytes());
    digest.update(config.content_id.as_ref());
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

fn calculate_work_units(
    inputs: &TypeScriptProjectInputs<'_>,
    sources: &[TszFileInput],
    libraries: &[std::sync::Arc<backend_frontend_typescript::TszLibFile>],
    closure_work_units: u64,
) -> u64 {
    let source_bytes = sources
        .iter()
        .map(|source| source.source.len() as u64)
        .sum::<u64>();
    let library_bytes = libraries
        .iter()
        .map(|library| {
            library
                .arena
                .get_source_file_at(library.root_index)
                .map(|source| source.text.len() as u64)
                .unwrap_or(0)
        })
        .sum::<u64>();
    let config_bytes = inputs
        .config_candidates
        .iter()
        .map(|config| config.bytes.len() as u64)
        .sum::<u64>();
    let bytes = source_bytes
        .saturating_add(library_bytes)
        .saturating_add(config_bytes);
    let count = (sources.len() as u64)
        .saturating_add(libraries.len() as u64)
        .saturating_add(inputs.typescript_files.len() as u64);
    1_000_000_u64
        .saturating_add(bytes.saturating_mul(48))
        .saturating_add(count.saturating_mul(65_536))
        .saturating_add(closure_work_units.saturating_mul(256))
        .min(MAX_PROJECT_WORK_UNITS)
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
            .validate_current(admitted.witness.as_ref())
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
            .typescript_module_root
            .join("typescript/lib/typescript.js");
        let arguments = [
            OsString::from(compiler_api_path.as_os_str()),
            OsString::from(serialized),
        ];
        let output = run_typescript_program_bridge(
            inputs.node_path,
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
