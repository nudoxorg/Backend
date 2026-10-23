//! Versioned preservation of the complete native resolver ledger. Only the
//! authenticated selected publication may supply these bytes; filesystem replay
//! remains mandatory before a decoded ledger becomes an opaque witness.

use super::*;
use std::collections::BTreeMap;

const MAX_BYTES: usize = backend_semantic::ir::MAX_TYPESCRIPT_SNAPSHOT_WITNESS_BYTES;

/// Bounds allocation while encoding, rather than allocating an oversized JSON
/// buffer before deciding that preservation is ineligible.
pub(super) fn encode_json_with_limit(
    value: &impl serde::Serialize,
    max_bytes: usize,
) -> Option<Vec<u8>> {
    if max_bytes > MAX_BYTES {
        return None;
    }
    let mut output = Output(Vec::new(), max_bytes);
    serde_json::to_writer(&mut output, value).ok()?;
    Some(output.0)
}

struct Output(Vec<u8>, usize);
impl io::Write for Output {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self
            .0
            .len()
            .checked_add(bytes.len())
            .is_none_or(|length| length > self.1)
        {
            return Err(io::Error::other(
                "TypeScript snapshot JSON exceeds its byte bound",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResolverDto {
    version: u32,
    project_fingerprint: [u8; 32],
    digest: [u8; 32],
    loaded_sources:
        BoundedSnapshotVec<(String, NativeSourceSnapshotDto), MAX_RESOLVED_SOURCE_FILES>,
    missing_paths: BoundedSnapshotVec<(String, SnapshotOption<String>), MAX_RESOLVER_OBSERVATIONS>,
    directories: BoundedSnapshotVec<(String, DirectoryDto), MAX_RESOLVER_DIRECTORIES>,
    native_root_selection: SnapshotOption<NativeRootSelectionDto>,
    external_negative_parents: BoundedSnapshotVec<(String, RealpathDto), MAX_RESOLVER_OBSERVATIONS>,
    external_absent_directories:
        BoundedSnapshotVec<(String, RealpathDto), MAX_RESOLVER_OBSERVATIONS>,
    captured_peer_manifests: BoundedSnapshotVec<(String, PeerDto), MAX_RESOLVER_OBSERVATIONS>,
    peer_directory_candidates: BoundedSnapshotVec<(String, String), MAX_RESOLVER_OBSERVATIONS>,
    realpath_queries_by_target: BoundedSnapshotVec<(String, String), MAX_RESOLVER_OBSERVATIONS>,
    realpaths: BoundedSnapshotVec<(String, RealpathDto), MAX_RESOLVER_OBSERVATIONS>,
    closure_replay_work_units: u64,
    loaded_source_bytes: u64,
    directory_entries: usize,
    retained_metadata_bytes: usize,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RealpathDto {
    path: String,
    canonical_path: SnapshotOption<String>,
    identity: SnapshotOption<(u64, u128)>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectoryDto {
    path: String,
    identity: (u64, u128),
    entries: BoundedSnapshotVec<EntryDto, MAX_RESOLVER_DIRECTORY_ENTRIES>,
    root_enumeration: bool,
    module_enumeration: bool,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeRootSelectionDto {
    config_path: String,
    root_names: BoundedSnapshotVec<String, MAX_RESOLVED_SOURCE_FILES>,
    directories: BoundedSnapshotVec<String, MAX_RESOLVER_DIRECTORIES>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryDto {
    path: String,
    canonical_path: SnapshotOption<String>,
    kind: u8,
    effective_kind: SnapshotOption<u8>,
    identity: (u64, u128),
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PeerDto {
    source_path: String,
    content_id: [u8; 32],
    candidates: BoundedSnapshotVec<String, MAX_RESOLVER_OBSERVATIONS>,
    activated: bool,
}

fn decode_path(path: String) -> Option<PathBuf> {
    let path = snapshot_decode_path(path)?;
    (normalize_absolute_path(&path).as_ref() == Some(&path)
        && !path.as_os_str().as_encoded_bytes().contains(&0))
    .then_some(path)
}

fn encode_path_option(path: Option<&Path>) -> Option<SnapshotOption<String>> {
    Some(match path {
        Some(path) => SnapshotOption::Present(snapshot_path(path)?),
        None => SnapshotOption::Absent,
    })
}

fn decode_path_option(path: SnapshotOption<String>) -> Option<Option<PathBuf>> {
    Some(match path {
        SnapshotOption::Present(path) => Some(decode_path(path)?),
        SnapshotOption::Absent => None,
    })
}

fn encode_kind(kind: TypeScriptDirectoryEntryKind) -> u8 {
    match kind {
        TypeScriptDirectoryEntryKind::RegularFile => 1,
        TypeScriptDirectoryEntryKind::Directory => 2,
        TypeScriptDirectoryEntryKind::Symlink => 3,
        TypeScriptDirectoryEntryKind::Other => 4,
    }
}

fn decode_kind(kind: u8) -> Option<TypeScriptDirectoryEntryKind> {
    Some(match kind {
        1 => TypeScriptDirectoryEntryKind::RegularFile,
        2 => TypeScriptDirectoryEntryKind::Directory,
        3 => TypeScriptDirectoryEntryKind::Symlink,
        4 => TypeScriptDirectoryEntryKind::Other,
        _ => return None,
    })
}

fn encode_realpath(value: &RealpathSnapshot) -> Option<RealpathDto> {
    Some(RealpathDto {
        path: snapshot_path(&value.path)?,
        canonical_path: encode_path_option(value.canonical_path.as_deref())?,
        identity: match value.identity {
            Some(identity) => SnapshotOption::Present(snapshot_identity(identity)),
            None => SnapshotOption::Absent,
        },
    })
}

fn decode_realpath(value: RealpathDto) -> Option<RealpathSnapshot> {
    Some(RealpathSnapshot {
        path: decode_path(value.path)?.into_boxed_path(),
        canonical_path: decode_path_option(value.canonical_path)?.map(PathBuf::into_boxed_path),
        identity: match value.identity {
            SnapshotOption::Present((first, second)) => {
                Some(snapshot_decode_identity(first, second)?)
            }
            SnapshotOption::Absent => None,
        },
    })
}

fn encode_directory(value: &DirectorySnapshot) -> Option<DirectoryDto> {
    if value.entries.len() > MAX_RESOLVER_DIRECTORY_ENTRIES {
        return None;
    }
    Some(DirectoryDto {
        path: snapshot_path(&value.path)?,
        identity: snapshot_identity(value.identity),
        root_enumeration: value.root_enumeration,
        module_enumeration: value.module_enumeration,
        entries: BoundedSnapshotVec(
            value
                .entries
                .iter()
                .map(|entry| {
                    Some(EntryDto {
                        path: snapshot_path(&entry.path)?,
                        canonical_path: encode_path_option(entry.canonical_path.as_deref())?,
                        kind: encode_kind(entry.kind),
                        effective_kind: match entry.effective_kind {
                            Some(kind) => SnapshotOption::Present(encode_kind(kind)),
                            None => SnapshotOption::Absent,
                        },
                        identity: snapshot_identity(entry.identity),
                    })
                })
                .collect::<Option<_>>()?,
        ),
    })
}

fn decode_directory(value: DirectoryDto) -> Option<DirectorySnapshot> {
    let path = decode_path(value.path)?;
    let entries = value
        .entries
        .0
        .into_iter()
        .map(|entry| {
            Some(TypeScriptDirectoryEntry {
                path: decode_path(entry.path)?.into_boxed_path(),
                canonical_path: decode_path_option(entry.canonical_path)?
                    .map(PathBuf::into_boxed_path),
                kind: decode_kind(entry.kind)?,
                effective_kind: match entry.effective_kind {
                    SnapshotOption::Present(kind) => Some(decode_kind(kind)?),
                    SnapshotOption::Absent => None,
                },
                identity: snapshot_decode_identity(entry.identity.0, entry.identity.1)?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    if entries.windows(2).any(|pair| pair[0].path >= pair[1].path)
        || entries
            .iter()
            .any(|entry| entry.path.parent() != Some(path.as_path()))
    {
        return None;
    }
    Some(DirectorySnapshot {
        path: path.into_boxed_path(),
        identity: snapshot_decode_identity(value.identity.0, value.identity.1)?,
        entries: entries.into_boxed_slice(),
        root_enumeration: value.root_enumeration,
        module_enumeration: value.module_enumeration,
    })
}

fn encode_map<T, U>(
    map: &BTreeMap<PathBuf, T>,
    encode: impl Fn(&T) -> Option<U>,
) -> Option<Vec<(String, U)>> {
    map.iter()
        .map(|(path, value)| Some((snapshot_path(path)?, encode(value)?)))
        .collect()
}

fn decode_map<T, U>(
    rows: Vec<(String, T)>,
    decode: impl Fn(T) -> Option<U>,
    control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
) -> Option<BTreeMap<PathBuf, U>> {
    let mut map = BTreeMap::new();
    let mut previous: Option<PathBuf> = None;
    for (path, value) in rows {
        control().ok()?;
        let path = decode_path(path)?;
        if previous.as_ref().is_some_and(|previous| previous >= &path) {
            return None;
        }
        previous = Some(path.clone());
        map.insert(path, decode(value)?);
    }
    Some(map)
}

impl TypeScriptResolverWitness {
    /// Preserves all nine observation maps, their counters and exact producer digest.
    /// No ambient read or reconstruction supplies missing rows during encoding.
    #[cfg(test)]
    pub(crate) fn encode_native_snapshot(
        &self,
        host: &TypeScriptProjectWitness,
    ) -> Option<Vec<u8>> {
        self.encode_native_snapshot_with_limit(host, MAX_BYTES)
    }

    pub(crate) fn encode_native_snapshot_with_limit(
        &self,
        host: &TypeScriptProjectWitness,
        max_bytes: usize,
    ) -> Option<Vec<u8>> {
        if host.retained_native_bytes().is_none()
            || !host.is_native_for_root(&host.project_root)
            || self.project_fingerprint != host.fingerprint
            || self.retained_bytes() > MAX_BYTES
        {
            return None;
        }
        let observations = &self.observations;
        let counters = checked_counters(observations, host)?;
        if counters
            != (
                observations.loaded_source_bytes,
                observations.directory_entries,
                observations.retained_metadata_bytes,
            )
        {
            return None;
        }
        let dto = ResolverDto {
            version: 3,
            project_fingerprint: self.project_fingerprint,
            digest: self.digest,
            loaded_sources: BoundedSnapshotVec(encode_map(
                &observations.loaded_sources,
                snapshot_source,
            )?),
            missing_paths: BoundedSnapshotVec(encode_map(&observations.missing_paths, |parent| {
                encode_path_option(parent.as_deref())
            })?),
            directories: BoundedSnapshotVec(encode_map(
                &observations.directories,
                encode_directory,
            )?),
            native_root_selection: match observations.native_root_selection.as_ref() {
                Some(query) => SnapshotOption::Present(NativeRootSelectionDto {
                    config_path: snapshot_path(&query.config_path)?,
                    root_names: BoundedSnapshotVec(
                        query
                            .root_names
                            .iter()
                            .map(|path| snapshot_path(path))
                            .collect::<Option<_>>()?,
                    ),
                    directories: BoundedSnapshotVec(
                        query
                            .directories
                            .iter()
                            .map(|path| snapshot_path(path))
                            .collect::<Option<_>>()?,
                    ),
                }),
                None => SnapshotOption::Absent,
            },
            external_negative_parents: BoundedSnapshotVec(encode_map(
                &observations.external_negative_parents,
                encode_realpath,
            )?),
            external_absent_directories: BoundedSnapshotVec(encode_map(
                &observations.external_absent_directories,
                encode_realpath,
            )?),
            captured_peer_manifests: BoundedSnapshotVec(encode_map(
                &observations.captured_peer_manifests,
                |manifest| {
                    Some(PeerDto {
                        source_path: snapshot_path(&manifest.source_path)?,
                        content_id: *manifest.content_id.as_ref(),
                        candidates: BoundedSnapshotVec(
                            manifest
                                .candidates
                                .iter()
                                .map(|path| snapshot_path(path))
                                .collect::<Option<_>>()?,
                        ),
                        activated: manifest.activated,
                    })
                },
            )?),
            peer_directory_candidates: BoundedSnapshotVec(encode_map(
                &observations.peer_directory_candidates,
                |path| snapshot_path(path),
            )?),
            realpath_queries_by_target: BoundedSnapshotVec(encode_map(
                &observations.realpath_queries_by_target,
                |path| snapshot_path(path),
            )?),
            realpaths: BoundedSnapshotVec(encode_map(&observations.realpaths, encode_realpath)?),
            closure_replay_work_units: observations.closure_replay_work_units,
            loaded_source_bytes: counters.0,
            directory_entries: counters.1,
            retained_metadata_bytes: counters.2,
        };
        encode_json_with_limit(&dto, max_bytes)
    }

    /// Authenticated selected-generation bytes are mandatory. Structural and digest
    /// checks precede controlled current-host and complete filesystem revalidation.
    #[cfg(test)]
    pub(crate) fn decode_native_snapshot(
        bytes: &[u8],
        host: &TypeScriptProjectWitness,
        control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
    ) -> Option<Self> {
        control().ok()?;
        if bytes.is_empty() || bytes.len() > MAX_BYTES {
            return None;
        }
        let current_host = host.validated_controlled(control).ok()?;
        Self::decode_native_snapshot_with_validated_host(bytes, &current_host, control)
    }

    /// Shares one freshly replayed host fence across the forest, while every
    /// leaf still checks its complete independent observation ledger.
    pub(crate) fn decode_native_snapshot_with_validated_host(
        bytes: &[u8],
        current_host: &ValidatedTypeScriptHost<'_>,
        control: &mut dyn FnMut() -> Result<(), TypeScriptProjectHostError>,
    ) -> Option<Self> {
        let host = current_host.witness;
        control().ok()?;
        if bytes.is_empty()
            || bytes.len() > MAX_BYTES
            || !host.is_native_for_root(&host.project_root)
        {
            return None;
        }
        let dto: ResolverDto = serde_json::from_slice(bytes).ok()?;
        if dto.version != 3 || dto.project_fingerprint != host.fingerprint {
            return None;
        }
        let observations = ResolverObservationLedger {
            loaded_sources: decode_map(dto.loaded_sources.0, snapshot_decode_source, control)?,
            missing_paths: decode_map(dto.missing_paths.0, decode_path_option, control)?,
            directories: decode_map(dto.directories.0, decode_directory, control)?,
            native_root_selection: match dto.native_root_selection {
                SnapshotOption::Absent => None,
                SnapshotOption::Present(query) => Some(NativeRootSelectionObservation {
                    config_path: decode_path(query.config_path)?.into_boxed_path(),
                    root_names: query
                        .root_names
                        .0
                        .into_iter()
                        .map(decode_path)
                        .collect::<Option<Vec<_>>>()?
                        .into_iter()
                        .map(PathBuf::into_boxed_path)
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                    directories: query
                        .directories
                        .0
                        .into_iter()
                        .map(decode_path)
                        .collect::<Option<Vec<_>>>()?
                        .into_iter()
                        .map(PathBuf::into_boxed_path)
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                }),
            },
            external_negative_parents: decode_map(
                dto.external_negative_parents.0,
                decode_realpath,
                control,
            )?,
            external_absent_directories: decode_map(
                dto.external_absent_directories.0,
                decode_realpath,
                control,
            )?,
            captured_peer_manifests: decode_map(
                dto.captured_peer_manifests.0,
                |manifest| {
                    Some(CapturedPeerManifest {
                        source_path: decode_path(manifest.source_path)?.into_boxed_path(),
                        content_id: ContentId::<SourceFactDomain>::try_from(manifest.content_id)
                            .ok()?,
                        candidates: manifest
                            .candidates
                            .0
                            .into_iter()
                            .map(decode_path)
                            .collect::<Option<Vec<_>>>()?
                            .into_iter()
                            .map(PathBuf::into_boxed_path)
                            .collect(),
                        activated: manifest.activated,
                    })
                },
                control,
            )?,
            peer_directory_candidates: decode_map(
                dto.peer_directory_candidates.0,
                decode_path,
                control,
            )?,
            realpath_queries_by_target: decode_map(
                dto.realpath_queries_by_target.0,
                decode_path,
                control,
            )?,
            realpaths: decode_map(dto.realpaths.0, decode_realpath, control)?,
            closure_replay_work_units: dto.closure_replay_work_units,
            loaded_source_bytes: dto.loaded_source_bytes,
            directory_entries: dto.directory_entries,
            retained_metadata_bytes: dto.retained_metadata_bytes,
        };
        let counters = checked_counters(&observations, host)?;
        if counters
            != (
                dto.loaded_source_bytes,
                dto.directory_entries,
                dto.retained_metadata_bytes,
            )
        {
            return None;
        }
        let mut capability = TypeScriptResolverCapability {
            witness: host,
            observations,
        };
        if capability.resolver_witness().ok()? != dto.digest {
            return None;
        }
        let restored = Self {
            project_fingerprint: host.fingerprint,
            digest: capability.resolver_witness().ok()?,
            observations: std::mem::take(&mut capability.observations),
        };
        restored
            .validate_with_host_controlled(current_host, control)
            .ok()?;
        control().ok()?;
        (restored.retained_bytes() <= MAX_BYTES).then_some(restored)
    }
}

fn path_bytes(path: &Path) -> usize {
    path.as_os_str().as_encoded_bytes().len()
}
fn realpath_cost(snapshot: &RealpathSnapshot) -> Option<usize> {
    path_bytes(&snapshot.path).checked_add(snapshot.canonical_path.as_deref().map_or(0, path_bytes))
}

/// Derives exact accounting from the deduplicated retained maps. Peer indexes
/// are checked against their manifest bytes and underlying canonical queries.
fn checked_counters(
    ledger: &ResolverObservationLedger,
    host: &TypeScriptProjectWitness,
) -> Option<(u64, usize, usize)> {
    if ledger.observation_count() > MAX_RESOLVER_OBSERVATIONS
        || ledger.loaded_sources.len() > MAX_RESOLVED_SOURCE_FILES
        || ledger.directories.len() > MAX_RESOLVER_DIRECTORIES
        || ledger.closure_replay_work_units
            > super::super::typescript_program::MAX_CLOSURE_REPLAY_WORK_UNITS
    {
        return None;
    }
    let mut source_bytes = 0u64;
    let mut entries = 0usize;
    let mut metadata = 0usize;
    for (path, source) in &ledger.loaded_sources {
        if path.as_path() != source.path.as_ref()
            || !host.path_is_admitted(path)
            || source.bytes.len() as u64 > MAX_SOURCE_FILE_BYTES
            || ContentId::<SourceFactDomain>::from_canonical_bytes(&source.bytes)
                != source.content_id
        {
            return None;
        }
        source_bytes = source_bytes.checked_add(source.bytes.len() as u64)?;
        metadata = metadata.checked_add(path_bytes(path))?;
    }
    for (path, parent) in &ledger.missing_paths {
        if ledger.loaded_sources.contains_key(path)
            || ledger
                .realpaths
                .get(path)
                .is_some_and(|snapshot| snapshot.canonical_path.is_some())
            || parent
                .as_ref()
                .is_some_and(|parent| !ledger.directories.contains_key(parent))
        {
            return None;
        }
        if ledger.external_negative_parents.contains_key(path)
            || ledger.external_absent_directories.contains_key(path)
        {
            if parent.is_some() {
                return None;
            }
        } else {
            if !host.path_is_admitted(path) {
                return None;
            }
            metadata = metadata
                .checked_add(path_bytes(path))?
                .checked_add(parent.as_deref().map_or(0, path_bytes))?;
        }
    }
    for (path, directory) in &ledger.directories {
        if path.as_path() != directory.path.as_ref()
            || !host.path_is_admitted(path)
            || directory.root_enumeration
                != ledger.native_root_selection.as_ref().is_some_and(|query| {
                    query
                        .directories
                        .iter()
                        .any(|entry| entry.as_ref() == path.as_path())
                })
            || directory.entries.len() > MAX_RESOLVER_DIRECTORY_ENTRIES
            || directory
                .entries
                .windows(2)
                .any(|pair| pair[0].path >= pair[1].path)
            || directory
                .entries
                .iter()
                .any(|entry| entry.path.parent() != Some(path.as_path()))
        {
            return None;
        }
        entries = entries.checked_add(directory.entries.len())?;
        metadata = metadata.checked_add(directory_snapshot_metadata_cost(directory))?;
    }
    if let Some(query) = &ledger.native_root_selection {
        if captured_root_source_target(&query.config_path, ledger, host).is_none()
            || query.root_names.len() > MAX_RESOLVED_SOURCE_FILES
            || query.directories.len() > MAX_RESOLVER_DIRECTORIES
            || query.root_names.windows(2).any(|pair| pair[0] >= pair[1])
            || query.directories.windows(2).any(|pair| pair[0] >= pair[1])
            || query
                .root_names
                .iter()
                .any(|path| captured_root_source_target(path, ledger, host).is_none())
            || query.directories.iter().any(|path| {
                !host.path_is_admitted(path) || !ledger.directories.contains_key(path.as_ref())
            })
        {
            return None;
        }
        metadata = metadata
            .checked_add(path_bytes(&query.config_path))?
            .checked_add(
                query
                    .root_names
                    .iter()
                    .map(|path| path_bytes(path))
                    .try_fold(0usize, usize::checked_add)?,
            )?
            .checked_add(
                query
                    .directories
                    .iter()
                    .map(|path| path_bytes(path))
                    .try_fold(0usize, usize::checked_add)?,
            )?;
    }
    for (path, snapshot) in &ledger.realpaths {
        if path.as_path() != snapshot.path.as_ref() {
            return None;
        }
        metadata = metadata.checked_add(realpath_cost(snapshot)?)?;
    }
    for (path, snapshot) in &ledger.external_negative_parents {
        if !host.external_negative_module_candidate(path)
            || !matches!(ledger.missing_paths.get(path), Some(None))
            || ledger.external_absent_directories.contains_key(path)
        {
            return None;
        }
        metadata = metadata
            .checked_add(path_bytes(path))?
            .checked_add(realpath_cost(snapshot)?)?;
    }
    for (path, snapshot) in &ledger.external_absent_directories {
        if !matches!(ledger.missing_paths.get(path), Some(None))
            || !captured_peer_directory_candidate(path, host, ledger)
        {
            return None;
        }
        metadata = metadata
            .checked_add(path_bytes(path).checked_mul(2)?)?
            .checked_add(realpath_cost(snapshot)?)?;
    }
    for (target, lexical) in &ledger.realpath_queries_by_target {
        if !ledger
            .realpaths
            .get(lexical)
            .is_some_and(|snapshot| snapshot.canonical_path.as_deref() == Some(target.as_path()))
        {
            return None;
        }
        metadata = metadata
            .checked_add(path_bytes(target))?
            .checked_add(path_bytes(lexical))?;
    }
    for (directory, manifest) in &ledger.captured_peer_manifests {
        let source = ledger.loaded_sources.get(manifest.source_path.as_ref())?;
        if manifest.source_path.parent() != Some(directory.as_path())
            || manifest
                .source_path
                .file_name()
                .and_then(|name| name.to_str())
                != Some("package.json")
            || source.content_id != manifest.content_id
            || !host.path_is_admitted(directory)
            || manifest.candidates.len() > MAX_RESOLVER_OBSERVATIONS
            || manifest.activated != captured_canonical_realpath(directory, ledger)
        {
            return None;
        }
        let prefix = directory
            .to_str()
            .and_then(typescript_peer_directory_prefix)
            .and_then(|prefix| normalize_absolute_path(Path::new(prefix)))?;
        if host.path_is_admitted(&prefix) {
            return None;
        }
        let mut expected = Vec::new();
        if source.bytes.len() <= MAX_PACKAGE_MANIFEST_BYTES {
            if let Ok(parsed) = parse_typescript_config(&manifest.source_path, &source.bytes)
                && let Some(peers) = parsed
                    .get("peerDependencies")
                    .and_then(serde_json::Value::as_object)
            {
                for key in peers.keys().filter(|key| valid_peer_package_key(key)) {
                    expected.push(prefix.join(key).into_boxed_path());
                }
            }
        }
        if expected.as_slice() != manifest.candidates.as_ref() {
            return None;
        }
        metadata = metadata
            .checked_add(path_bytes(directory))?
            .checked_add(path_bytes(&manifest.source_path))?
            .checked_add(33)?;
        for candidate in &manifest.candidates {
            metadata = metadata.checked_add(path_bytes(candidate))?;
            if manifest.activated {
                let owner = ledger.peer_directory_candidates.get(candidate.as_ref())?;
                if !ledger
                    .captured_peer_manifests
                    .get(owner)
                    .is_some_and(|owner_manifest| {
                        owner_manifest.activated && owner_manifest.candidates.contains(candidate)
                    })
                {
                    return None;
                }
            }
        }
    }
    for (candidate, directory) in &ledger.peer_directory_candidates {
        let manifest = ledger.captured_peer_manifests.get(directory)?;
        if !manifest.activated
            || !manifest
                .candidates
                .iter()
                .any(|path| path.as_ref() == candidate.as_path())
        {
            return None;
        }
        metadata = metadata
            .checked_add(path_bytes(candidate))?
            .checked_add(path_bytes(directory))?;
    }
    if source_bytes > MAX_RESOLVED_SOURCE_BYTES
        || entries > MAX_RESOLVER_TOTAL_DIRECTORY_ENTRIES
        || metadata > MAX_RESOLVER_METADATA_BYTES
    {
        return None;
    }
    Some((source_bytes, entries, metadata))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture {
        directory: PathBuf,
        root: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let directory = std::env::temp_dir().join(format!(
                "tsz-resolver-certificate-{}-{stamp}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let root = directory.join("node_modules/package");
            fs::create_dir_all(&root).unwrap();
            fs::write(
                root.join("package.json"),
                r#"{"name":"fixture","peerDependencies":{"cold-peer":"*"}}"#,
            )
            .unwrap();
            fs::write(root.join("source.ts"), "export const value = 1;").unwrap();
            Self {
                directory,
                root: root.canonicalize().unwrap(),
            }
        }

        fn capture(&self) -> (TypeScriptProjectWitness, TypeScriptResolverWitness) {
            let host = TypeScriptProjectWitness::capture_native(&self.root, None).unwrap();
            let mut resolver = TypeScriptResolverCapability {
                witness: &host,
                observations: ResolverObservationLedger::default(),
            };
            resolver.realpath(&self.root).unwrap().unwrap();
            resolver
                .try_load_source(&self.root.join("package.json"))
                .unwrap()
                .unwrap();
            resolver
                .try_load_source(&self.root.join("source.ts"))
                .unwrap()
                .unwrap();
            assert!(
                resolver
                    .try_load_source(&self.root.join("missing.ts"))
                    .unwrap()
                    .is_none()
            );
            let external_modules = self.root.parent().unwrap().join("node_modules");
            assert!(!resolver.directory_exists(&external_modules).unwrap());
            resolver
                .observe_absent_directory(&self.root.parent().unwrap().join("cold-peer"))
                .unwrap();
            let witness = resolver.seal().unwrap();
            (host, witness)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn complete_native_resolver_round_trip_preserves_all_nine_maps() {
        let fixture = Fixture::new();
        let (host, original) = fixture.capture();
        let ledger = &original.observations;
        assert!(!ledger.loaded_sources.is_empty());
        assert!(!ledger.missing_paths.is_empty());
        assert!(!ledger.directories.is_empty());
        assert!(!ledger.external_negative_parents.is_empty());
        assert!(!ledger.external_absent_directories.is_empty());
        assert!(!ledger.captured_peer_manifests.is_empty());
        assert!(!ledger.peer_directory_candidates.is_empty());
        assert!(!ledger.realpath_queries_by_target.is_empty());
        assert!(!ledger.realpaths.is_empty());
        let bytes = original
            .encode_native_snapshot(&host)
            .expect("complete bounded certificate");
        let restored =
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                .expect("current complete transcript");
        assert_eq!(restored.digest(), original.digest());
        assert_eq!(restored.encode_native_snapshot(&host).unwrap(), bytes);
        fs::write(fixture.root.join("source.ts"), "export const value = 2;").unwrap();
        assert!(
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                .is_none()
        );
    }

    #[test]
    fn malformed_incomplete_or_cancelled_resolver_records_cannot_mint_a_witness() {
        let fixture = Fixture::new();
        let (host, witness) = fixture.capture();
        let bytes = witness.encode_native_snapshot(&host).unwrap();
        let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        for key in [
            "loaded_sources",
            "missing_paths",
            "directories",
            "native_root_selection",
            "external_negative_parents",
            "external_absent_directories",
            "captured_peer_manifests",
            "peer_directory_candidates",
            "realpath_queries_by_target",
            "realpaths",
        ] {
            let mut changed = original.clone();
            changed.as_object_mut().unwrap().remove(key);
            assert!(
                TypeScriptResolverWitness::decode_native_snapshot(
                    &serde_json::to_vec(&changed).unwrap(),
                    &host,
                    &mut || Ok(())
                )
                .is_none(),
                "omitted {key}"
            );
        }
        let mut duplicate = original.clone();
        let rows = duplicate["loaded_sources"].as_array_mut().unwrap();
        rows.push(rows[0].clone());
        let mut bad_counter = original.clone();
        bad_counter["retained_metadata_bytes"] = serde_json::json!(0);
        let mut unknown = original.clone();
        unknown["unknown"] = serde_json::json!(true);
        let mut unknown_query = original.clone();
        unknown_query["native_root_selection"] =
            serde_json::json!({"state":"absent", "unexpected":true});
        let mut future_version = original.clone();
        future_version["version"] = serde_json::json!(4);
        let mut bad_digest = original;
        bad_digest["digest"][0] = serde_json::json!(bad_digest["digest"][0].as_u64().unwrap() ^ 1);
        for changed in [
            duplicate,
            bad_counter,
            unknown,
            unknown_query,
            future_version,
            bad_digest,
        ] {
            assert!(
                TypeScriptResolverWitness::decode_native_snapshot(
                    &serde_json::to_vec(&changed).unwrap(),
                    &host,
                    &mut || Ok(())
                )
                .is_none()
            );
        }
        assert!(
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Err(
                TypeScriptProjectHostError::CompilerProgramCancelled
            ))
            .is_none()
        );
        fs::write(fixture.root.join("missing.ts"), "export {};").unwrap();
        assert!(
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                .is_none()
        );
    }

    #[test]
    fn configured_root_transcript_vectors_are_bounded_during_decode() {
        let oversized = serde_json::json!({
            "config_path": "/workspace/tsconfig.json",
            "root_names": [],
            "directories": vec!["/workspace/src"; MAX_RESOLVER_DIRECTORIES + 1],
        });
        assert!(serde_json::from_value::<NativeRootSelectionDto>(oversized).is_err());
    }

    #[test]
    fn existence_parent_regular_delta_replays_but_exact_missing_candidate_does_not() {
        let fixture = Fixture::new();
        let (host, witness) = fixture.capture();
        assert!(
            witness
                .observations
                .directories
                .values()
                .all(|directory| !directory.root_enumeration && !directory.module_enumeration)
        );
        let bytes = witness.encode_native_snapshot(&host).unwrap();
        fs::write(fixture.root.join("unrelated.txt"), "").unwrap();
        assert!(
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                .is_some()
        );
        let (_, fresh) = fixture.capture();
        assert_eq!(
            fresh.digest(),
            witness.digest(),
            "fresh and warm existence-only views bind the same effective inputs"
        );
        fs::remove_file(fixture.root.join("unrelated.txt")).unwrap();
        assert!(
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                .is_some()
        );
        fs::write(fixture.root.join("missing.ts"), "export {};").unwrap();
        assert!(
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                .is_none()
        );
    }

    #[test]
    fn root_only_enumeration_replays_exact_roots_and_ignores_unselected_regular_files() {
        let fixture = Fixture::new();
        fs::write(
            fixture.root.join("tsconfig.json"),
            r#"{"compilerOptions":{"noLib":true,"types":[]},"include":["*.ts"]}"#,
        )
        .unwrap();
        let (host, witness) = fixture.capture();
        let mut resolver = TypeScriptResolverCapability {
            witness: &host,
            observations: witness.observations,
        };
        resolver
            .try_load_source(&fixture.root.join("tsconfig.json"))
            .unwrap()
            .unwrap();
        resolver
            .observe_root_program_directory(&fixture.root)
            .unwrap();
        resolver
            .record_native_root_selection(
                &fixture.root.join("tsconfig.json"),
                &[fixture.root.join("source.ts")],
                &[fixture.root.clone()],
            )
            .unwrap();
        let witness = resolver.seal().unwrap();
        let bytes = witness.encode_native_snapshot(&host).unwrap();
        assert!(
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                .is_some()
        );
        fs::write(fixture.root.join("unrelated.txt"), "").unwrap();
        assert!(
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                .is_some()
        );
        fs::write(fixture.root.join("new-root.ts"), "export {};").unwrap();
        assert!(
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                .is_none()
        );
    }

    #[test]
    fn module_enumeration_regular_delta_is_exact_including_shared_root_directory() {
        for shared_root in [false, true] {
            let fixture = Fixture::new();
            fs::write(
                fixture.root.join("tsconfig.json"),
                r#"{"compilerOptions":{"noLib":true,"types":[]},"include":["*.ts"]}"#,
            )
            .unwrap();
            let (host, witness) = fixture.capture();
            let mut resolver = TypeScriptResolverCapability {
                witness: &host,
                observations: witness.observations,
            };
            resolver
                .try_load_source(&fixture.root.join("tsconfig.json"))
                .unwrap()
                .unwrap();
            if shared_root {
                resolver
                    .observe_root_program_directory(&fixture.root)
                    .unwrap();
                resolver
                    .record_native_root_selection(
                        &fixture.root.join("tsconfig.json"),
                        &[fixture.root.join("source.ts")],
                        &[fixture.root.clone()],
                    )
                    .unwrap();
                assert!(!resolver.observations.directories[&fixture.root].module_enumeration);
                // A later module enumeration must restore strictness even though
                // root selection enumerated the same physical directory first.
                resolver.observe_program_directory(&fixture.root).unwrap();
            } else {
                resolver.observe_program_directory(&fixture.root).unwrap();
            }
            let witness = resolver.seal().unwrap();
            assert!(witness.observations.directories[&fixture.root].module_enumeration);
            let bytes = witness.encode_native_snapshot(&host).unwrap();
            assert!(
                TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                    .is_some()
            );
            fs::write(fixture.root.join("unrelated.txt"), "").unwrap();
            assert!(
                TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                    .is_none(),
                "module enumeration must remain exact, shared_root={shared_root}"
            );
            fs::remove_file(fixture.root.join("unrelated.txt")).unwrap();
            assert!(
                TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                    .is_some()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_retarget_and_new_directory_children_invalidate_decoded_closure() {
        let fixture = Fixture::new();
        fs::write(fixture.root.join("other.ts"), "export const other = 1;").unwrap();
        std::os::unix::fs::symlink("source.ts", fixture.root.join("link.ts")).unwrap();
        let (host, mut witness) = fixture.capture();
        let mut resolver = TypeScriptResolverCapability {
            witness: &host,
            observations: witness.observations,
        };
        resolver.observe_program_directory(&fixture.root).unwrap();
        witness = resolver.seal().unwrap();
        let bytes = witness.encode_native_snapshot(&host).unwrap();
        fs::write(fixture.root.join("new-child.ts"), "export {};").unwrap();
        assert!(
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                .is_none()
        );
        fs::remove_file(fixture.root.join("new-child.ts")).unwrap();
        fs::remove_file(fixture.root.join("link.ts")).unwrap();
        std::os::unix::fs::symlink("other.ts", fixture.root.join("link.ts")).unwrap();
        assert!(
            TypeScriptResolverWitness::decode_native_snapshot(&bytes, &host, &mut || Ok(()))
                .is_none()
        );
    }
}
