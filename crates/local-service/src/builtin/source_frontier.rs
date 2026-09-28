//! Conservative Git-backed hints for reusing exact prior local source rows.
//!
//! The process-local frontier is bounded and disposable. A miss always returns
//! to ordinary ingestion. Git supplies a clean tracked-path witness; an exact
//! filesystem revision additionally fences raw worktree bytes from Git's clean
//! conversions such as autocrlf, eol, and working-tree-encoding.
//!
//! On Unix the revision includes device, inode, size, modification time, and
//! change time. That is a practical local-filesystem fence rather than a
//! cryptographic proof against a filesystem or concurrent writer that can
//! restore or coarsen every observed metadata value. Unsupported platforms,
//! unusual Git state, and any failed fence return to the full source scan.
//! This cache does not establish a compiler read closure and never skips
//! semantic compilation.

use super::ingest::{FileSystemRevision, IndexSnapshot};
use backend_compile::SourceLanguage;
use backend_engine::{ProductSourceRecord, ProductSourceRelation, Relation as _};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

const SOURCE_FRONTIER_VERSION: u8 = 1;
const SOURCE_FRONTIER_POLICY_IDENTITY: &str =
    "backend.local-source-frontier.v1;selection=source-selection-policy.v1;git-clean-paths.v1";
const MAX_SOURCE_FRONTIERS: usize = 8;
const MAX_SOURCE_FRONTIER_BYTES: usize = 8 * 1024 * 1024;
const MAX_SOURCE_FRONTIER_CACHE_BYTES: usize = 32 * 1024 * 1024;
const MAX_GIT_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_GIT_POLICY_FILE_BYTES: u64 = 8 * 1024 * 1024;
const SOURCE_POLICY_FILE_NAMES: [&str; 4] =
    [".gitignore", ".ignore", ".rgignore", ".gitattributes"];

pub(super) struct SourcePath {
    pub(super) absolute: PathBuf,
    pub(super) relative: String,
    pub(super) language: SourceLanguage,
    pub(super) analysis: [u8; 32],
    pub(super) revision: Option<FileSystemRevision>,
}

#[derive(Clone, Debug)]
pub(super) struct SourceFrontierFile {
    pub(super) relative_path: String,
    pub(super) key: [u8; 32],
    pub(super) content: [u8; 32],
    pub(super) analysis: [u8; 32],
    record_digest: [u8; 32],
    git_blob: String,
    pub(super) revision: FileSystemRevision,
    pub(super) encoded_record_bytes: usize,
}

/// Compact cache of exact CAS row fingerprints; no source record bodies live
/// here, and the cache can be dropped without affecting correctness.
#[derive(Clone, Debug)]
struct SourceFrontier {
    version: u8,
    root: PathBuf,
    project: [u8; 32],
    policy_identity: &'static str,
    git_policy_digest: [u8; 32],
    policy_blobs: Vec<(String, String)>,
    files: Vec<SourceFrontierFile>,
    estimated_bytes: usize,
}

#[derive(Default)]
pub(super) struct SourceDelta {
    version: u8,
    pub(super) unchanged: BTreeMap<PathBuf, SourceFrontierFile>,
}

impl SourceDelta {
    pub(super) const fn is_current(&self) -> bool {
        self.version == SOURCE_FRONTIER_VERSION
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SourceFrontierKey {
    root: PathBuf,
    project: [u8; 32],
}

#[derive(Default)]
struct SourceFrontierCache {
    entries: BTreeMap<SourceFrontierKey, SourceFrontier>,
    order: VecDeque<SourceFrontierKey>,
    bytes: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct GitIndexEntry {
    mode: u32,
    blob: String,
}

#[derive(Debug)]
pub(super) struct GitSourceState {
    head: String,
    index_digest: [u8; 32],
    git_policy_digest: [u8; 32],
    policy_blobs: Vec<(String, String)>,
    entries: BTreeMap<String, GitIndexEntry>,
    changed_paths: BTreeSet<String>,
    untracked_paths: BTreeSet<String>,
}

fn source_frontier_cache() -> &'static Mutex<SourceFrontierCache> {
    static CACHE: OnceLock<Mutex<SourceFrontierCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(SourceFrontierCache::default()))
}

fn source_frontier_key(root: &Path, project: [u8; 32]) -> SourceFrontierKey {
    SourceFrontierKey {
        root: root.to_path_buf(),
        project,
    }
}

fn cached_source_frontier(root: &Path, project: [u8; 32]) -> Option<SourceFrontier> {
    let key = source_frontier_key(root, project);
    let mut cache = source_frontier_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let frontier = cache.entries.get(&key)?.clone();
    cache.order.retain(|candidate| candidate != &key);
    cache.order.push_back(key);
    Some(frontier)
}

fn remember_source_frontier(frontier: SourceFrontier) {
    if frontier.estimated_bytes > MAX_SOURCE_FRONTIER_BYTES {
        return;
    }
    let key = source_frontier_key(&frontier.root, frontier.project);
    let mut cache = source_frontier_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(previous) = cache.entries.remove(&key) {
        cache.bytes = cache.bytes.saturating_sub(previous.estimated_bytes);
        cache.order.retain(|candidate| candidate != &key);
    }
    while cache.entries.len() >= MAX_SOURCE_FRONTIERS
        || cache.bytes.saturating_add(frontier.estimated_bytes) > MAX_SOURCE_FRONTIER_CACHE_BYTES
    {
        let Some(evicted) = cache.order.pop_front() else {
            break;
        };
        if let Some(previous) = cache.entries.remove(&evicted) {
            cache.bytes = cache.bytes.saturating_sub(previous.estimated_bytes);
        }
    }
    cache.bytes = cache.bytes.saturating_add(frontier.estimated_bytes);
    cache.order.push_back(key.clone());
    cache.entries.insert(key, frontier);
}

#[cfg(test)]
pub(super) fn clear_source_frontier_for(root: &Path, project: [u8; 32]) {
    let mut cache = source_frontier_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let key = source_frontier_key(root, project);
    if let Some(previous) = cache.entries.remove(&key) {
        cache.bytes = cache.bytes.saturating_sub(previous.estimated_bytes);
    }
    cache.order.retain(|candidate| candidate != &key);
}

#[cfg(test)]
pub(super) fn install_source_frontier_race(root: PathBuf, path: PathBuf, bytes: Vec<u8>) {
    source_frontier_races()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(root, (path, bytes));
}

#[cfg(test)]
pub(super) fn trigger_source_frontier_race(root: &Path) {
    let mutation = source_frontier_races()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(root);
    if let Some((path, bytes)) = mutation {
        let _ = fs::write(path, bytes);
    }
}

#[cfg(test)]
fn source_frontier_races() -> &'static Mutex<BTreeMap<PathBuf, (PathBuf, Vec<u8>)>> {
    static RACES: OnceLock<Mutex<BTreeMap<PathBuf, (PathBuf, Vec<u8>)>>> = OnceLock::new();
    RACES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// Captures the Git facts that can prove a selected tracked file still names
/// the same bytes as the last scan. The gate is Unix-only until each platform
/// has an audited no-follow and index-witness implementation.
#[cfg(unix)]
pub(super) fn git_source_state(
    root: &Path,
    wanted_paths: &BTreeSet<String>,
    local_policy_paths: &[PathBuf],
) -> Option<GitSourceState> {
    if std::env::var_os("HOME").is_none() && std::env::var_os("XDG_CONFIG_HOME").is_none() {
        return None;
    }
    let top = git_command(root, &["rev-parse", "--show-toplevel"])?;
    let top = String::from_utf8(trim_ascii(&top).to_vec()).ok()?;
    let top = PathBuf::from(top).canonicalize().ok()?;
    if top != root {
        return None;
    }
    let head = git_command(root, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let head = String::from_utf8(trim_ascii(&head).to_vec()).ok()?;
    if !matches!(head.len(), 40 | 64) || !head.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    let config = git_command(
        root,
        &["config", "--null", "--list", "--show-origin", "--includes"],
    )?;
    let config_names = git_command(root, &["config", "--name-only", "--list", "--includes"])?;
    if config_names
        .split(|byte| *byte == b'\n')
        .any(|name| name.eq_ignore_ascii_case(b"core.fsmonitor"))
    {
        return None;
    }
    let mut git_policy = blake3::Hasher::new();
    git_policy.update(b"backend.source-frontier.git-policy.v1\0");
    if let Some(version) = git_command(root, &["--version"]) {
        git_policy.update(trim_ascii(&version));
    } else {
        return None;
    }
    git_policy.update(std::env::consts::OS.as_bytes());
    git_policy.update(std::env::consts::ARCH.as_bytes());
    git_policy.update(&config);
    hash_git_environment(&mut git_policy)?;
    hash_git_policy_paths(root, local_policy_paths, &mut git_policy)?;
    let index = git_command(root, &["ls-files", "--stage", "-z"])?;
    let flags = git_command(root, &["ls-files", "-v", "-z"])?;
    if flags
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .any(|entry| entry.first() != Some(&b'H') || entry.get(1) != Some(&b' '))
    {
        // Skip-worktree and assume-unchanged entries can suppress worktree
        // checks even while Git reports a clean status.
        return None;
    }
    let mut index_hasher = blake3::Hasher::new();
    index_hasher.update(&index);
    index_hasher.update(&flags);
    let index_digest = *index_hasher.finalize().as_bytes();
    let mut entries = BTreeMap::new();
    let mut policy_blobs = Vec::new();
    for raw in index
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
    {
        let tab = raw.iter().position(|byte| *byte == b'\t')?;
        let header = std::str::from_utf8(&raw[..tab]).ok()?;
        let path = std::str::from_utf8(&raw[tab + 1..]).ok()?;
        let relative_path = Path::new(path);
        if path.is_empty()
            || path.contains(['\\', '\0'])
            || relative_path.is_absolute()
            || relative_path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return None;
        }
        let mut fields = header.split_ascii_whitespace();
        let mode = u32::from_str_radix(fields.next()?, 8).ok()?;
        let blob = fields.next()?.to_owned();
        let stage = fields.next()?;
        if fields.next().is_some()
            || stage != "0"
            || !matches!(mode, 0o100644 | 0o100755)
            || !matches!(blob.len(), 40 | 64)
            || !blob.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return None;
        }
        if is_source_policy_path(path) {
            policy_blobs.push((path.to_owned(), blob.clone()));
        }
        if wanted_paths.contains(path) {
            entries.insert(path.to_owned(), GitIndexEntry { mode, blob });
        }
    }
    policy_blobs.sort();
    // Hash raw worktree ignore/attribute files too. Their Git blobs can remain
    // equal across line-ending or working-tree-encoding conversions while
    // their exact bytes and policy interpretation change.
    for (path, _) in &policy_blobs {
        hash_policy_file(&mut git_policy, &root.join(path))?;
    }
    let git_policy_digest = *git_policy.finalize().as_bytes();

    let status = git_command(
        root,
        &[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--no-renames",
            "--ignore-submodules=none",
        ],
    )?;
    let mut changed_paths = BTreeSet::new();
    let mut untracked_paths = BTreeSet::new();
    for raw in status
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
    {
        let line = std::str::from_utf8(raw).ok()?;
        let path = if let Some(path) = line.strip_prefix("? ") {
            untracked_paths.insert(path.to_owned());
            path
        } else if line.starts_with("1 ") {
            line.splitn(9, ' ').nth(8)?
        } else if line.starts_with("u ") {
            line.splitn(11, ' ').nth(10)?
        } else {
            // No branch headers are requested. Rename records are disabled;
            // any other status shape is outside this version of the witness.
            return None;
        };
        changed_paths.insert(path.to_owned());
    }

    Some(GitSourceState {
        head,
        index_digest,
        git_policy_digest,
        policy_blobs,
        entries,
        changed_paths,
        untracked_paths,
    })
}

#[cfg(not(unix))]
fn git_source_state(
    _root: &Path,
    _wanted_paths: &BTreeSet<String>,
    _local_policy_paths: &[PathBuf],
) -> Option<GitSourceState> {
    None
}

#[cfg(unix)]
fn git_command(root: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .args([
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
        ])
        .arg("-C")
        .arg(root)
        .args(arguments)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    let mut output = Vec::new();
    let mut stdout = child.stdout.take()?;
    let read = stdout
        .by_ref()
        .take(u64::try_from(MAX_GIT_OUTPUT_BYTES.saturating_add(1)).ok()?)
        .read_to_end(&mut output);
    if read.is_err() || output.len() > MAX_GIT_OUTPUT_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    child.wait().ok()?.success().then_some(output)
}

#[cfg(unix)]
fn hash_git_environment(hasher: &mut blake3::Hasher) -> Option<()> {
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "GIT_CONFIG_GLOBAL",
        "GIT_CONFIG_SYSTEM",
        "GIT_CONFIG_NOSYSTEM",
    ] {
        hasher.update(name.as_bytes());
        hasher.update(&[0]);
        if let Some(value) = std::env::var_os(name) {
            hasher.update(value.as_encoded_bytes());
        }
        hasher.update(&[0]);
    }
    for (name, value) in
        std::env::vars_os().filter(|(name, _)| name.as_encoded_bytes().starts_with(b"GIT_"))
    {
        let Some(name_text) = name.to_str() else {
            // These controls can redirect the index, worktree, object store,
            // or attributes source away from the ordinary repository view.
            return None;
        };
        if matches!(
            name_text,
            "GIT_DIR"
                | "GIT_WORK_TREE"
                | "GIT_INDEX_FILE"
                | "GIT_COMMON_DIR"
                | "GIT_OBJECT_DIRECTORY"
                | "GIT_ALTERNATE_OBJECT_DIRECTORIES"
                | "GIT_CEILING_DIRECTORIES"
                | "GIT_DISCOVERY_ACROSS_FILESYSTEM"
                | "GIT_NAMESPACE"
                | "GIT_ATTR_SOURCE"
                | "GIT_CONFIG_COUNT"
                | "GIT_CONFIG_PARAMETERS"
        ) || name_text.starts_with("GIT_CONFIG_KEY_")
            || name_text.starts_with("GIT_CONFIG_VALUE_")
        {
            // `git_source_state` will fail closed when the environment cannot
            // be represented by the normal configured repository witness.
            return None;
        }
        hasher.update(name.as_encoded_bytes());
        hasher.update(&[0]);
        hasher.update(value.as_encoded_bytes());
        hasher.update(&[0]);
    }
    Some(())
}

#[cfg(unix)]
fn hash_git_policy_paths(
    root: &Path,
    local_policy_paths: &[PathBuf],
    hasher: &mut blake3::Hasher,
) -> Option<()> {
    let git_dir = git_command(root, &["rev-parse", "--absolute-git-dir"])?;
    let git_dir = PathBuf::from(String::from_utf8(trim_ascii(&git_dir).to_vec()).ok()?);
    let mut paths = vec![
        git_dir.join("info/exclude"),
        git_dir.join("info/attributes"),
    ];
    for key in ["core.excludesFile", "core.attributesFile"] {
        if let Some(configured) = git_command(root, &["config", "--path", "--get-all", key]) {
            let configured = std::str::from_utf8(trim_ascii(&configured)).ok()?;
            for line in configured.lines().filter(|line| !line.is_empty()) {
                let path = PathBuf::from(line);
                paths.push(if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                });
            }
        }
    }
    let default_global_ignore = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|path| path.join("git/ignore"));
    if let Some(path) = default_global_ignore {
        paths.push(path);
    }
    paths.sort();
    paths.dedup();
    for path in paths {
        hash_policy_file(hasher, &path)?;
    }
    for path in local_policy_paths {
        hash_policy_file(hasher, path)?;
    }
    Some(())
}

#[cfg(unix)]
fn hash_policy_file(hasher: &mut blake3::Hasher, path: &Path) -> Option<()> {
    use std::os::unix::fs::MetadataExt as _;

    hasher.update(path.as_os_str().as_encoded_bytes());
    hasher.update(&[0]);
    let before = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => metadata,
        Ok(_) => return None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            hasher.update(b"absent\0");
            return Some(());
        }
        Err(_) => return None,
    };
    if before.len() > MAX_GIT_POLICY_FILE_BYTES {
        return None;
    }
    let mut file = fs::File::open(path).ok()?;
    let opened_before = file.metadata().ok()?;
    if !opened_before.is_file() || opened_before.ino() != before.ino() {
        return None;
    }
    hasher.update(b"present\0");
    hasher.update(&before.len().to_le_bytes());
    let mut buffer = [0_u8; 16 * 1024];
    let mut total = 0_u64;
    loop {
        let read = file.read(&mut buffer).ok()?;
        if read == 0 {
            break;
        }
        total = total.checked_add(u64::try_from(read).ok()?)?;
        if total > MAX_GIT_POLICY_FILE_BYTES || total > before.len() {
            return None;
        }
        hasher.update(&buffer[..read]);
    }
    let after = file.metadata().ok()?;
    let current = fs::symlink_metadata(path).ok()?;
    if total != before.len()
        || after.dev() != before.dev()
        || after.ino() != before.ino()
        || after.ctime() != before.ctime()
        || after.ctime_nsec() != before.ctime_nsec()
        || current.dev() != before.dev()
        || current.ino() != before.ino()
        || current.ctime() != before.ctime()
        || current.ctime_nsec() != before.ctime_nsec()
    {
        return None;
    }
    Some(())
}

fn trim_ascii(mut bytes: &[u8]) -> &[u8] {
    while bytes.last().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[..bytes.len() - 1];
    }
    while bytes.first().is_some_and(u8::is_ascii_whitespace) {
        bytes = &bytes[1..];
    }
    bytes
}

fn is_source_policy_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| SOURCE_POLICY_FILE_NAMES.contains(&name))
}

pub(super) fn is_policy_file_name(name: &str) -> bool {
    SOURCE_POLICY_FILE_NAMES.contains(&name)
}

/// Finds local selection-policy files in directories discovery already
/// visited, including files hidden by a higher-priority ignore rule. This is
/// only four metadata probes per discovered directory, not another walk.
pub(super) fn source_policy_candidates(
    root: &Path,
    directories: &[(PathBuf, Option<FileSystemRevision>)],
) -> Option<Vec<PathBuf>> {
    let mut candidates = Vec::new();
    for (relative_directory, _) in directories {
        let directory = if relative_directory.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(relative_directory)
        };
        for name in SOURCE_POLICY_FILE_NAMES {
            let path = directory.join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_file() => candidates.push(path),
                Ok(_) => return None,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
        }
    }
    candidates.sort();
    candidates.dedup();
    Some(candidates)
}

fn source_policy_changed(state: &GitSourceState) -> bool {
    state
        .changed_paths
        .iter()
        .chain(state.untracked_paths.iter())
        .any(|path| is_source_policy_path(path))
}

pub(super) fn source_wanted_paths(
    root: &Path,
    sources: &[SourcePath],
    configurations: &[PathBuf],
) -> Option<BTreeSet<String>> {
    let mut wanted = BTreeSet::new();
    for path in sources
        .iter()
        .map(|source| &source.absolute)
        .chain(configurations.iter())
    {
        let relative = path.strip_prefix(root).ok()?.to_str()?;
        if relative.is_empty()
            || relative.contains(['\\', '\0'])
            || relative.chars().any(char::is_control)
        {
            return None;
        }
        wanted.insert(relative.replace(std::path::MAIN_SEPARATOR, "/"));
    }
    Some(wanted)
}

fn stable_source_delta(
    root: &Path,
    project: [u8; 32],
    sources: &[SourcePath],
    configurations: &[PathBuf],
    directories_stable: bool,
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontier: &SourceFrontier,
    state: &GitSourceState,
    is_supported_source: impl Fn(&Path) -> bool,
) -> Option<SourceDelta> {
    if frontier.version != SOURCE_FRONTIER_VERSION
        || frontier.root != root
        || frontier.project != project
        || frontier.policy_identity != SOURCE_FRONTIER_POLICY_IDENTITY
        || frontier.git_policy_digest != state.git_policy_digest
        || frontier.policy_blobs != state.policy_blobs
        || source_policy_changed(state)
        || !directories_stable
        || sources.iter().any(|source| source.revision.is_none())
    {
        return None;
    }

    let wanted = source_wanted_paths(root, sources, configurations)?;
    for path in &wanted {
        // A selected path without a stage-zero regular Git entry can be an
        // ignored or untracked input. Let the ordinary scanner classify it.
        if !state.entries.contains_key(path) {
            return None;
        }
    }
    for path in &state.untracked_paths {
        if is_source_policy_path(path)
            || wanted.contains(path)
            || is_supported_source(Path::new(path))
        {
            return None;
        }
    }

    let mut delta = SourceDelta {
        version: SOURCE_FRONTIER_VERSION,
        unchanged: BTreeMap::new(),
    };
    for source in sources {
        let relative = &source.relative;
        if state.changed_paths.contains(relative) {
            continue;
        }
        let Some(cached) = frontier
            .files
            .binary_search_by(|entry| entry.relative_path.cmp(relative))
            .ok()
            .and_then(|index| frontier.files.get(index))
        else {
            continue;
        };
        let Some(indexed) = state.entries.get(relative) else {
            return None;
        };
        if indexed.mode != 0o100644 && indexed.mode != 0o100755 {
            return None;
        }
        if indexed.blob != cached.git_blob {
            continue;
        }
        let Some(record) = reusable.get(&cached.key) else {
            continue;
        };
        let (record_digest, encoded_record_bytes) = source_record_digest(record)?;
        let Some(fields) = record.file_fields() else {
            continue;
        };
        if record_digest != cached.record_digest
            || fields.project != project
            || fields.path != relative
            || fields.language != source.language
            || fields.content_version != cached.content
            || fields.content_version == [0; 32]
            || fields.analysis_version != cached.analysis
            || fields.analysis_version != source.analysis
        {
            continue;
        }
        let Some(revision) = source.revision else {
            return None;
        };
        // Git can normalize distinct raw worktree byte streams to one blob.
        // Reuse only when the exact revision captured by the previous scan is
        // still the revision discovered now.
        if revision != cached.revision {
            continue;
        }
        delta.unchanged.insert(
            source.absolute.clone(),
            SourceFrontierFile {
                relative_path: relative.clone(),
                key: cached.key,
                content: cached.content,
                analysis: cached.analysis,
                record_digest: cached.record_digest,
                git_blob: cached.git_blob.clone(),
                revision,
                encoded_record_bytes,
            },
        );
    }
    (delta.version == SOURCE_FRONTIER_VERSION && !delta.unchanged.is_empty()).then_some(delta)
}

fn source_record_digest(record: &ProductSourceRecord) -> Option<([u8; 32], usize)> {
    let mut encoded = Vec::new();
    ProductSourceRelation::encode_value(record, &mut encoded);
    if encoded.len() > ProductSourceRecord::ROW_VALUE_CAPACITY {
        return None;
    }
    Some((*blake3::hash(&encoded).as_bytes(), encoded.len()))
}

fn source_frontier_from_snapshot(
    root: &Path,
    project: [u8; 32],
    snapshot: &IndexSnapshot,
    state: &GitSourceState,
) -> Option<SourceFrontier> {
    if source_policy_changed(state) {
        return None;
    }
    let mut files = Vec::new();
    let mut estimated_bytes = 512usize;
    // Build once; a per-file linear search here made frontier seeding
    // quadratic in the number of indexed source files.
    let revisions = snapshot
        .revision_fence
        .files
        .iter()
        .map(|file| (file.relative_path.clone(), file.metadata))
        .collect::<BTreeMap<_, _>>();
    for (key, record) in &snapshot.files {
        let Some(fields) = record.file_fields() else {
            return None;
        };
        if fields.project != project
            || fields.content_version == [0; 32]
            || state.changed_paths.contains(fields.path)
        {
            continue;
        }
        let Some(indexed) = state.entries.get(fields.path) else {
            continue;
        };
        if indexed.mode != 0o100644 && indexed.mode != 0o100755 {
            continue;
        }
        let Some(revision) = revisions.get(Path::new(fields.path)).copied().flatten() else {
            continue;
        };
        let (record_digest, encoded_record_bytes) = source_record_digest(record)?;
        let entry_bytes = std::mem::size_of::<SourceFrontierFile>()
            .saturating_add(fields.path.len())
            .saturating_add(indexed.blob.len());
        estimated_bytes = estimated_bytes.checked_add(entry_bytes)?;
        if estimated_bytes > MAX_SOURCE_FRONTIER_BYTES {
            return None;
        }
        files.push(SourceFrontierFile {
            relative_path: fields.path.to_owned(),
            key: *key,
            content: fields.content_version,
            analysis: fields.analysis_version,
            record_digest,
            git_blob: indexed.blob.clone(),
            revision,
            encoded_record_bytes,
        });
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let mut policy_bytes = 0usize;
    for (path, blob) in &state.policy_blobs {
        policy_bytes = policy_bytes.checked_add(
            std::mem::size_of::<(String, String)>()
                .saturating_add(path.len())
                .saturating_add(blob.len()),
        )?;
    }
    estimated_bytes = estimated_bytes.checked_add(policy_bytes)?;
    if estimated_bytes > MAX_SOURCE_FRONTIER_BYTES {
        return None;
    }
    Some(SourceFrontier {
        version: SOURCE_FRONTIER_VERSION,
        root: root.to_path_buf(),
        project,
        policy_identity: SOURCE_FRONTIER_POLICY_IDENTITY,
        git_policy_digest: state.git_policy_digest,
        policy_blobs: state.policy_blobs.clone(),
        files,
        estimated_bytes,
    })
}

pub(super) fn source_delta(
    root: &Path,
    project: [u8; 32],
    sources: &[SourcePath],
    configurations: &[PathBuf],
    directories_stable: bool,
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    state: &GitSourceState,
    is_supported_source: impl Fn(&Path) -> bool,
) -> Option<SourceDelta> {
    let frontier = cached_source_frontier(root, project)?;
    stable_source_delta(
        root,
        project,
        sources,
        configurations,
        directories_stable,
        reusable,
        &frontier,
        state,
        is_supported_source,
    )
}

pub(super) fn remember_snapshot_frontier(
    root: &Path,
    project: [u8; 32],
    snapshot: &IndexSnapshot,
    state: &GitSourceState,
) {
    if let Some(frontier) = source_frontier_from_snapshot(root, project, snapshot, state) {
        remember_source_frontier(frontier);
    }
}

pub(super) fn git_states_same_during_scan(before: &GitSourceState, after: &GitSourceState) -> bool {
    before.head == after.head
        && before.index_digest == after.index_digest
        && before.git_policy_digest == after.git_policy_digest
        && before.policy_blobs == after.policy_blobs
        && before.changed_paths == after.changed_paths
        && before.untracked_paths == after.untracked_paths
}
