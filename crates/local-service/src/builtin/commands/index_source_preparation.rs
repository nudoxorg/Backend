//! Completed scans and compiler routing prepared from an immutable admitted
//! snapshot. Every source read, relation walk and candidate SQL runs on the
//! existing owned publication worker, never the listener's control turn.
use super::*;
use backend_engine::builtin::{
    NativeClangCandidatePermit, NativePathDisposition, product_native_frontier_path_key,
    product_native_frontier_profile_key, product_native_frontier_record_key,
    product_native_frontier_relation,
};

pub(in crate::builtin::commands) enum PreparedSourceScan {
    Ready(PreparedProductSelection),
    Capture {
        selection: PreparedProductSelection,
        continuation: SourceCompilePlan,
    },
}

pub(in crate::builtin::commands) struct SourceRoute {
    pub(in crate::builtin::commands) owner:
        Arc<crate::builtin::cluster_dispatch::OwnerCompilerClusterRuntime>,
    pub(in crate::builtin::commands) pending:
        Option<Arc<Mutex<crate::builtin::pending_stored::PendingStoredAckJournal>>>,
}

/// Owned continuation of exactly the structural capture candidate. Its inputs
/// were materialized and hash-checked before asking the owner for a grant.
pub(in crate::builtin::commands) struct SourceCompilePlan {
    native_typescript: Option<Arc<ingest::RevalidatedNativeTsCapture>>,
    mixed_stylo_proof: Option<crate::builtin::stylo_admission::StyloLiveProof>,
    rust_target: Option<ResolvedCargoTarget>,
    csharp_project: Option<Arc<ResolvedCSharpProject>>,
    csharp_shape: Option<CSharpScanShape>,
    expected_project_commitment: [u8; 32],
    pending_csharp_sources: Option<CSharpSourceHandles>,
    csharp_parked: bool,
    package: backend_engine::PackageKey,
    label: String,
    project_key: [u8; 32],
    coordinate: Option<PackageUrl>,
    request_id: u64,
    source_root: PathBuf,
    revision_fence: ingest::CompilerRevisionFence,
    sources: Vec<ingest::CompilerSource>,
    observations: BTreeMap<LanguageProfile, SourceObservationReceipt>,
    pub(in crate::builtin::commands) captures:
        BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    native_frontier: Vec<ProductNativeFrontierRecord>,
    native_candidate_profiles: ingest::NativeCandidateProfiles,
    /// Profiles whose exact selected generation was proven current and will
    /// be carried into this operation's receipt without recompiling it.
    reused_profiles: BTreeSet<LanguageProfile>,
    operation_key: Option<backend_library::IndexOperationKey>,
    reuse_only: bool,
    python_environment: Option<PythonEnvironmentInput>,
    cancellation: Arc<AtomicBool>,
    workspace_snapshot: Option<ingest::CompilerWorkspaceSnapshot>,
    route: Option<SourceRoute>,
    execution_intent: CompileExecutionIntent,
}

/// Shared immutable-base source and profile admission. This plan cannot
/// commit a workspace or publish a view; callers choose their capture timing.
pub(super) struct PreparedScanSource {
    pub(super) rust_target: Option<ResolvedCargoTarget>,
    pub(super) csharp_project: Option<ResolvedCSharpProject>,
    pub(super) csharp_shape: Option<CSharpScanShape>,
    pub(super) package: backend_engine::PackageKey,
    pub(super) label: String,
    pub(super) requested_root: PathBuf,
    pub(super) coordinate: Option<PackageUrl>,
    pub(super) request_id: u64,
    pub(super) execution_intent: CompileExecutionIntent,
    pub(super) project_key: [u8; 32],
    pub(super) before: Option<ProductSourceRecord>,
    pub(super) project: ProductSourceRecord,
    pub(super) file_keys: Vec<[u8; 32]>,
    pub(super) changes: Vec<BuiltinSourceChange>,
    pub(super) source_facts_changes: Vec<BuiltinSourceFactsChange>,
    pub(super) native_frontier_changes: Vec<crate::builtin::profile::BuiltinNativeFrontierChange>,
    pub(super) native_frontier: Vec<ProductNativeFrontierRecord>,
    /// Original private scan witness; raw NSE rows are not a substitute.
    pub(super) native_typescript_capture: Option<ingest::NativeTsCapture>,
    pub(super) revision_fence: ingest::CompilerRevisionFence,
    pub(super) python_environment: Option<PythonEnvironmentInput>,
    pub(super) cancellation: Arc<AtomicBool>,
    pub(super) workspace_snapshot: Option<ingest::CompilerWorkspaceSnapshot>,
    pub(super) profiles: PreparedScanProfiles,
    pub(super) stylo_proof: Option<crate::builtin::stylo_admission::StyloLiveProof>,
    pub(super) sql_proof: Option<Arc<crate::builtin::sql_admission::SqlLiveProof>>,
}

pub(super) enum PreparedScanProfiles {
    Reused {
        captures: BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
        reused_profiles: BTreeSet<LanguageProfile>,
        pending_changes: Vec<BuiltinCaptureChange>,
    },
    Compile(Box<PreparedScanCompilation>),
}

fn reused_profiles_intent(
    package: backend_engine::PackageKey,
    label: &str,
    changes: Vec<BuiltinSourceChange>,
    source_facts_changes: Vec<BuiltinSourceFactsChange>,
) -> Result<BuiltinIntent, BuiltinModelError> {
    // The v7 encoding intentionally requires at least one facts row. Keep
    // source-only updates in the existing v4 format when no facts were made.
    if source_facts_changes.is_empty() {
        BuiltinIntent::index_with_semantics(package, label, changes, Vec::new())
    } else {
        BuiltinIntent::index_with_source_facts(
            package,
            label,
            changes,
            Vec::new(),
            Vec::new(),
            source_facts_changes,
        )
    }
}

/// The exact dirty-profile input handles and private Pending after-images,
/// admitted together against PreparedScanSource's immutable base.
pub(super) struct PreparedScanCompilation {
    pub(super) fresh: Vec<ingest::CompilerSourceHandle>,
    pub(super) reused: Vec<ingest::ReusedCompilerFile>,
    pub(super) source_admission_policy: crate::builtin::source_budget::SourceAdmissionPolicy,
    pub(super) observations: BTreeMap<LanguageProfile, SourceObservationReceipt>,
    pub(super) captures: BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    /// Exact selected generations omitted from this compile because their
    /// current input witness proves reuse.
    pub(super) reused_profiles: BTreeSet<LanguageProfile>,
    pub(super) pending_changes: Vec<BuiltinCaptureChange>,
    pub(super) native_clang_candidates: Vec<ingest::NativeClangCandidateHandle>,
    pub(super) native_csharp_candidates: Vec<ingest::NativeCSharpCandidateHandle>,
    pub(super) native_frontier: Vec<ProductNativeFrontierRecord>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct NativeCandidatePermits {
    pub(super) project: Option<[u8; 32]>,
    pub(super) selected_root: Option<[u8; 32]>,
    pub(super) clang_by_profile: BTreeMap<LanguageProfile, Box<[NativeClangCandidatePermit]>>,
    pub(super) csharp_by_profile:
        BTreeMap<LanguageProfile, Box<[backend_engine::builtin::NativeCSharpCandidatePermit]>>,
}

struct CSharpSourceHandles {
    fresh: Vec<ingest::CompilerSourceHandle>,
    reused: Vec<ingest::ReusedCompilerFile>,
    policy: crate::builtin::source_budget::SourceAdmissionPolicy,
}

impl CSharpSourceHandles {
    fn retained_bytes(&self) -> Option<usize> {
        let mut total = std::mem::size_of::<Self>()
            .checked_add(self.fresh.capacity().checked_mul(std::mem::size_of::<ingest::CompilerSourceHandle>())?)?
            .checked_add(self.reused.capacity().checked_mul(std::mem::size_of::<ingest::ReusedCompilerFile>())?)?
            .checked_add(128)?;
        for source in &self.fresh { total = total.checked_add(source.relative_path.capacity())?.checked_add(64)?; }
        for source in &self.reused { total = total.checked_add(source.relative_path.capacity())?.checked_add(64)?; }
        Some(total)
    }
}

pub(super) struct CSharpScanShape {
    profile: LanguageProfile,
    digest: [u8; 32],
}

fn preflight_csharp_source_shape(
    scan: &ingest::IndexSnapshot,
    workspace: &ingest::CompilerWorkspaceSnapshot,
    source_root: &Path,
    selected: &ResolvedCSharpProject,
) -> Result<CSharpScanShape, BuiltinModelError> {
    if !scan.native_csharp_candidates.is_empty() {
        return Err(BuiltinModelError("C# native candidates need unit-scoped permits".to_owned()));
    }
    let directory = selected.project_path.rsplit_once('/').map_or("", |(parent, _)| parent);
    let in_scope = |path: &str| directory.is_empty() || path.strip_prefix(directory).is_some_and(|suffix| suffix.starts_with('/'));
    let mut observed = Vec::<(&str, u64, LanguageProfile)>::new();
    let mut work = 0usize;
    for (path, profile) in scan.compiler_sources.iter().map(|source| (source.relative_path.as_str(), source.profile))
        .chain(scan.reused_compiler_files.iter().map(|source| (source.relative_path.as_str(), source.profile))) {
        let profile = ingest::compilation_profile(source_root, path, profile);
        if !matches!(profile, LanguageProfile::CSharp(_)) || !in_scope(path) { continue; }
        let entry = workspace.entries().binary_search_by(|entry| entry.path.as_str().cmp(path))
            .ok().and_then(|index| workspace.entries().get(index))
            .filter(|entry| entry.kind == ingest::CompilerWorkspaceEntryKind::File)
            .ok_or_else(|| BuiltinModelError("C# source is outside observed workspace inventory".to_owned()))?;
        let len = entry.byte_length.ok_or_else(|| BuiltinModelError("C# source has no observed length".to_owned()))?;
        let length = usize::try_from(len).map_err(|_| BuiltinModelError("C# source length overflow".to_owned()))?;
        let cost = length.checked_add(path.len().checked_mul(2).ok_or_else(|| BuiltinModelError("C# source budget overflow".to_owned()))?)
            .and_then(|value| value.checked_add(2 * std::mem::size_of::<ingest::CompilerSource>() + 256))
            .ok_or_else(|| BuiltinModelError("C# source budget overflow".to_owned()))?;
        work = work.checked_add(cost).filter(|total| *total <= 8 * 1024 * 1024)
            .ok_or_else(|| BuiltinModelError("C# source witness exceeds preflight bound".to_owned()))?;
        if length > 8 * 1024 * 1024 || observed.len() >= backend_engine::application::MAX_MANIFEST_ENTRIES {
            return Err(BuiltinModelError("C# source witness exceeds preflight bound".to_owned()));
        }
        observed.push((path, len, profile));
    }
    observed.sort_unstable_by(|a, b| a.0.cmp(b.0));
    if observed.is_empty() || observed.windows(2).any(|pair| pair[0].0 == pair[1].0)
        || observed.iter().any(|item| item.2 != observed[0].2) {
        return Err(BuiltinModelError("C# source witness is empty, duplicated or multi-profile".to_owned()));
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"backend.local-service.csharp-source-shape.v1\0");
    for (path, len, _) in &observed {
        hash.update(&(path.len() as u64).to_le_bytes()); hash.update(path.as_bytes()); hash.update(&len.to_le_bytes());
    }
    hash.update(&(observed.len() as u64).to_le_bytes());
    Ok(CSharpScanShape { profile: observed[0].2, digest: *hash.finalize().as_bytes() })
}

// An explicit solution selection identifies a project; it does not prove
// MSBuild evaluation or complete source membership.
#[derive(Clone, Debug)]
enum CSharpProjectSelector {
    Project(String),
    SolutionProject {
        solution_path: String,
        project_path: String,
    },
}

fn solution_contains_project(
    bytes: &[u8],
    solution_path: &str,
    project_path: &str,
) -> Result<bool, BuiltinModelError> {
    use quick_xml::{Reader, events::Event};
    let mut reader = Reader::from_reader(bytes);
    let mut buffer = Vec::new();
    let mut stack = Vec::<Vec<u8>>::new();
    let mut nodes = 0usize;
    let mut found = 0usize;
    let prefix = solution_path
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    loop {
        let event = reader
            .read_event_into(&mut buffer)
            .map_err(|_| BuiltinModelError("invalid .slnx XML".to_owned()))?;
        match event {
            Event::Start(ref node) | Event::Empty(ref node) => {
                nodes = nodes
                    .checked_add(1)
                    .ok_or_else(|| BuiltinModelError(".slnx node bound exceeded".to_owned()))?;
                if nodes > 4096 || stack.len() > 16 {
                    return Err(BuiltinModelError(
                        ".slnx structure exceeds bound".to_owned(),
                    ));
                }
                let name = node.name().as_ref().to_vec();
                let parent = stack.last().map(Vec::as_slice);
                let allowed = match name.as_slice() {
                    b"Solution" => parent.is_none() && nodes == 1,
                    b"Folder" | b"Project" => {
                        parent.is_some_and(|value| value == b"Solution" || value == b"Folder")
                    }
                    _ => false,
                };
                if !allowed {
                    return Err(BuiltinModelError("unsupported .slnx structure".to_owned()));
                }
                let mut path = None;
                let mut folder_name = false;
                for attribute in node.attributes().with_checks(true) {
                    let attribute = attribute
                        .map_err(|_| BuiltinModelError("invalid .slnx attribute".to_owned()))?;
                    match (name.as_slice(), attribute.key.as_ref()) {
                        (b"Project", b"Path") => {
                            let raw =
                                std::str::from_utf8(attribute.value.as_ref()).map_err(|_| {
                                    BuiltinModelError("invalid .slnx path encoding".to_owned())
                                })?;
                            if raw.len() > 4096
                                || prefix.len().saturating_add(raw.len()) > 4096
                                || raw.contains('&')
                                || raw.contains('\\')
                                || raw.contains(':')
                                || raw.starts_with('/')
                                || raw
                                    .split('/')
                                    .any(|part| part.is_empty() || part == "." || part == "..")
                            {
                                return Err(BuiltinModelError(
                                    "unsupported .slnx project path".to_owned(),
                                ));
                            }
                            path = Some(if prefix.is_empty() {
                                raw.to_owned()
                            } else {
                                format!("{prefix}/{raw}")
                            });
                        }
                        (b"Folder", b"Name")
                            if !attribute.value.is_empty()
                                && std::str::from_utf8(attribute.value.as_ref()).is_ok()
                                && !attribute.value.contains(&b'&') =>
                        {
                            folder_name = true
                        }
                        _ => {
                            return Err(BuiltinModelError("unsupported .slnx attribute".to_owned()));
                        }
                    }
                }
                if name == b"Project" {
                    let path = path
                        .ok_or_else(|| BuiltinModelError(".slnx project has no path".to_owned()))?;
                    if path == project_path {
                        found += 1;
                    }
                } else if name == b"Folder" && !folder_name {
                    return Err(BuiltinModelError(".slnx folder has no name".to_owned()));
                }
                if matches!(&event, Event::Start(_)) {
                    stack.push(name);
                }
            }
            Event::End(_) => {
                stack
                    .pop()
                    .ok_or_else(|| BuiltinModelError("invalid .slnx nesting".to_owned()))?;
            }
            Event::Text(ref text) if !text.iter().all(u8::is_ascii_whitespace) => {
                return Err(BuiltinModelError("unsupported .slnx text".to_owned()));
            }
            Event::CData(_) | Event::PI(_) => {
                return Err(BuiltinModelError("unsupported .slnx content".to_owned()));
            }
            Event::DocType(_) => {
                return Err(BuiltinModelError(
                    ".slnx document types are unsupported".to_owned(),
                ));
            }
            Event::Eof => break,
            Event::Text(_) | Event::Comment(_) | Event::Decl(_) => {}
            _ => return Err(BuiltinModelError("unsupported .slnx content".to_owned())),
        }
        buffer.clear();
    }
    if !stack.is_empty() || nodes == 0 || found > 1 {
        return Err(BuiltinModelError("ambiguous .slnx project".to_owned()));
    }
    Ok(found == 1)
}

fn resolve_csharp_selector(
    snapshot: &ingest::CompilerWorkspaceSnapshot,
    selector: CSharpProjectSelector,
) -> Result<ResolvedCSharpProject, BuiltinModelError> {
    let lease = CSharpCaptureLease::acquire()?;
    let (project_path, solution) = match selector {
        CSharpProjectSelector::Project(project_path) => (project_path, None),
        CSharpProjectSelector::SolutionProject {
            solution_path,
            project_path,
        } => {
            if !solution_path.to_ascii_lowercase().ends_with(".slnx") {
                return Err(BuiltinModelError(
                    "C# solution selector is not .slnx".to_owned(),
                ));
            }
            let solution = snapshot
                .capture_confined_file(&solution_path, 1024 * 1024)
                .map_err(BuiltinModelError)?;
            if !solution_contains_project(solution.bytes(), &solution_path, &project_path)? {
                return Err(BuiltinModelError(
                    "selected project is not present in .slnx".to_owned(),
                ));
            }
            (project_path, Some(solution))
        }
    };
    if !snapshot.entries().iter().any(|entry| {
        entry.kind == ingest::CompilerWorkspaceEntryKind::File && entry.path == project_path
    }) {
        return Err(BuiltinModelError(
            "selected project is outside the observed workspace".to_owned(),
        ));
    }
    let graph = capture_csharp_project_graph(snapshot, &[project_path.clone()])?;
    let node = graph.node(&project_path).ok_or_else(|| BuiltinModelError("C# graph root is missing".to_owned()))?;
    let config = node.config.clone();
    let sdk_config = node.sdk_config.clone();
    let selected = ResolvedCSharpProject {
        project_path,
        _capture_lease: Arc::new(lease),
        graph,
        solution,
        config,
        sdk_config,
    };
    if selected.retained_bytes().is_none_or(|bytes| bytes > 4 * 1024 * 1024) {
        return Err(BuiltinModelError("C# capture retained bytes exceed bound".to_owned()));
    }
    Ok(selected)
}

/// Enumerates every observed project as an independent root, sharing one
/// immutable capture and one bounded capture lease. This does not pick a root
/// by file ordering and does not combine project assemblies.
fn discover_observed_csharp_projects(
    snapshot: Option<&ingest::CompilerWorkspaceSnapshot>,
) -> Result<Vec<ResolvedCSharpProject>, BuiltinModelError> {
    let Some(snapshot) = snapshot else { return Ok(Vec::new()); };
    let paths = snapshot.entries().iter().filter(|entry|
        entry.kind == ingest::CompilerWorkspaceEntryKind::File && entry.path.to_ascii_lowercase().ends_with(".csproj"))
        .map(|entry| entry.path.clone()).collect::<Vec<_>>();
    if paths.is_empty() { return Ok(Vec::new()); }
    let lease = Arc::new(CSharpCaptureLease::acquire()?);
    let graph = capture_csharp_project_graph(snapshot, &paths)?;
    paths.into_iter().map(|project_path| {
        let node = graph.node(&project_path).ok_or_else(|| BuiltinModelError("C# graph root is missing".to_owned()))?;
        Ok(ResolvedCSharpProject { project_path, _capture_lease: Arc::clone(&lease), graph: Arc::clone(&graph),
            solution: None, config: node.config.clone(), sdk_config: node.sdk_config.clone() })
    }).collect()
}

fn capture_csharp_project_graph(
    snapshot: &ingest::CompilerWorkspaceSnapshot,
    roots: &[String],
) -> Result<Arc<ResolvedCSharpProjectGraph>, BuiltinModelError> {
    let mut pending = roots.to_vec();
    let mut nodes = BTreeMap::new();
    let mut edges = 0usize;
    let mut retained = 0usize;
    while let Some(project_path) = pending.pop() {
        if nodes.contains_key(&project_path) { continue; }
        if nodes.len() >= 64 || project_path.len() > 4096 || !project_path.to_ascii_lowercase().ends_with(".csproj")
            || project_path.starts_with('/') || project_path.contains(['\\', ':'])
            || project_path.split('/').any(|part| matches!(part, "" | "." | ".."))
            || !snapshot.entries().iter().any(|entry| entry.kind == ingest::CompilerWorkspaceEntryKind::File && entry.path == project_path) {
            return Err(BuiltinModelError("C# graph project is outside observed inventory or exceeds bound".to_owned()));
        }
        let mut paths = vec![project_path.clone()];
        let mut dir = project_path.rsplit_once('/').map_or("", |(parent, _)| parent);
        loop {
            let prefix = if dir.is_empty() { String::new() } else { format!("{dir}/") };
            paths.push(format!("{prefix}Directory.Build.props")); paths.push(format!("{prefix}Directory.Build.targets"));
            if dir.is_empty() { break; }
            if paths.len() >= 17 { return Err(BuiltinModelError("C# graph project path is too deep".to_owned())); }
            dir = dir.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
        let config = snapshot.capture_configuration_paths(&paths, 2 * 1024 * 1024).map_err(BuiltinModelError)?;
        let sdk_config = snapshot.capture_csharp_global_json_paths(&project_path).map_err(BuiltinModelError)?;
        if config.probes().chain(sdk_config.probes()).any(|(_, bytes)| bytes.is_some_and(|bytes| std::str::from_utf8(bytes).is_err())) {
            return Err(BuiltinModelError("C# graph configuration is not UTF-8".to_owned()));
        }
        let project = config.probes().next().and_then(|(_, bytes)| bytes).ok_or_else(|| BuiltinModelError("C# graph project is absent".to_owned()))?;
        let project_references = captured_csharp_project_references(project, &project_path)?;
        edges = edges.checked_add(project_references.len()).filter(|count| *count <= 256)
            .ok_or_else(|| BuiltinModelError("C# graph exceeds edge limit".to_owned()))?;
        retained = retained.checked_add(config.retained_bytes().ok_or_else(|| BuiltinModelError("C# graph size overflow".to_owned()))?)
            .and_then(|bytes| bytes.checked_add(sdk_config.retained_bytes()?)).filter(|bytes| *bytes <= 4 * 1024 * 1024)
            .ok_or_else(|| BuiltinModelError("C# graph configuration exceeds retained-byte bound".to_owned()))?;
        pending.extend(project_references.iter().cloned());
        nodes.insert(project_path.clone(), ResolvedCSharpProjectNode { project_path, config, sdk_config,
            project_references: project_references.into_boxed_slice() });
    }
    let graph = Arc::new(ResolvedCSharpProjectGraph { nodes: nodes.into_values().collect() });
    for root in roots { graph.closure(root)?; }
    if graph.retained_bytes().is_none_or(|bytes| bytes > 4 * 1024 * 1024) {
        return Err(BuiltinModelError("C# graph configuration exceeds retained-byte bound".to_owned()));
    }
    Ok(graph)
}

// Parse only literal ProjectReference edges from captured XML; no imports,
// targets, property substitution, or MSBuild evaluation is performed here.
fn captured_csharp_project_references(bytes: &[u8], project_path: &str) -> Result<Vec<String>, BuiltinModelError> {
    use quick_xml::{Reader, events::Event};
    let fail = || BuiltinModelError("unsupported captured C# project reference".to_owned());
    let mut reader = Reader::from_reader(bytes);
    reader.config_mut().expand_empty_elements = true;
    let mut buffer = Vec::new();
    let mut stack = Vec::<(Vec<u8>, bool)>::new();
    let mut output = Vec::new();
    let mut nodes = 0usize;
    let mut saw_root = false;
    loop {
        match reader.read_event_into(&mut buffer).map_err(|_| fail())? {
            Event::Start(node) => {
                nodes += 1;
                if nodes > 8192 || stack.len() >= 32 { return Err(fail()); }
                let name = node.name().as_ref().to_vec();
                if stack.is_empty() {
                    if name != b"Project" { return Err(fail()); }
                    saw_root = true;
                } else if !saw_root {
                    return Err(fail());
                }
                if stack.last().is_some_and(|(name, _)| name == b"ProjectReference") { return Err(fail()); }
                let is_reference = name == b"ProjectReference";
                let mut conditioned = false;
                let mut include = None;
                for attribute in node.attributes().with_checks(true) {
                    let attribute = attribute.map_err(|_| fail())?;
                    if attribute.key.as_ref() == b"Condition" { conditioned = true; }
                    if is_reference {
                        if attribute.key.as_ref() != b"Include" || include.is_some() { return Err(fail()); }
                        include = Some(std::str::from_utf8(attribute.value.as_ref()).map_err(|_| fail())?.to_owned());
                    }
                }
                if is_reference {
                    if conditioned || stack.iter().any(|(_, condition)| *condition)
                        || stack.len() != 2 || stack[0].0 != b"Project" || stack[1].0 != b"ItemGroup" { return Err(fail()); }
                    let raw = include.ok_or_else(fail)?.replace('\\', "/");
                    if raw.is_empty() || raw.len() > 4096 || raw.starts_with('/') || raw.contains(':')
                        || raw.chars().any(|ch| ch.is_control() || matches!(ch, '$' | '@' | '%' | '*' | '?' | ';' | '&')) { return Err(fail()); }
                    let directory = project_path.rsplit_once('/').map_or("", |(directory, _)| directory);
                    let mut parts = if directory.is_empty() { Vec::new() } else { directory.split('/').collect::<Vec<_>>() };
                    for part in raw.split('/') {
                        match part { "" => return Err(fail()), "." => {}, ".." => { parts.pop().ok_or_else(fail)?; }, _ => parts.push(part) }
                    }
                    let path = parts.join("/");
                    if !path.to_ascii_lowercase().ends_with(".csproj") || path.len() > 4096 || output.contains(&path) || output.len() >= 64 { return Err(fail()); }
                    output.push(path);
                }
                stack.push((name, conditioned));
            }
            Event::End(node) => {
                let (expected, _) = stack.pop().ok_or_else(fail)?;
                if expected.as_slice() != node.name().as_ref() { return Err(fail()); }
            }
            Event::DocType(_) | Event::PI(_) | Event::CData(_) => return Err(fail()),
            Event::Eof => break,
            Event::Text(text) if stack.last().is_some_and(|(name, _)| name == b"ProjectReference")
                && !text.iter().all(u8::is_ascii_whitespace) => return Err(fail()),
            _ => {},
        }
        buffer.clear();
    }
    if nodes == 0 || !saw_root || !stack.is_empty() { return Err(fail()); }
    output.sort();
    Ok(output)
}

// Discovery only supplies a hint from observed paths; ignore-filtered
// inventory does not prove that the observed project is unique on disk.
fn discover_observed_csharp_project(
    snapshot: Option<&ingest::CompilerWorkspaceSnapshot>,
) -> Result<Option<ResolvedCSharpProject>, BuiltinModelError> {
    let Some(snapshot) = snapshot else {
        return Ok(None);
    };
    let mut projects = snapshot.entries().iter().filter(|entry| {
        entry.kind == ingest::CompilerWorkspaceEntryKind::File
            && entry.path.to_ascii_lowercase().ends_with(".csproj")
    });
    let Some(project) = projects.next() else {
        return Ok(None);
    };
    if projects.next().is_some() {
        return Ok(None);
    }
    resolve_csharp_selector(
        snapshot,
        CSharpProjectSelector::Project(project.path.clone()),
    )
    .map(Some)
}

fn resolve_requested_cargo_target(
    compiler: &LocalCompilerClient,
    context: &SemanticCompilationContext<'_>,
    scan: &ingest::IndexSnapshot,
    live: &BTreeSet<LanguageProfile>,
    selector: Option<&backend_library::CargoTargetSelector>,
    cancellation: Arc<AtomicBool>,
) -> Result<Option<ResolvedCargoTarget>, BuiltinModelError> {
    let Some(selector) = selector else {
        return Ok(None);
    };
    let mut profiles = live
        .iter()
        .copied()
        .filter(|profile| matches!(profile, LanguageProfile::Rust(_)));
    let profile = profiles.next().ok_or_else(|| {
        BuiltinModelError("Cargo target selection requires a Rust package profile".to_owned())
    })?;
    if profiles.next().is_some() {
        return Err(BuiltinModelError(
            "Cargo target selection has multiple Rust profiles".to_owned(),
        ));
    }
    let fresh = scan
        .compiler_sources
        .iter()
        .filter(|source| matches!(source.profile, LanguageProfile::Rust(_)))
        .cloned()
        .collect();
    let reused = scan
        .reused_compiler_files
        .iter()
        .filter(|source| matches!(source.profile, LanguageProfile::Rust(_)))
        .cloned()
        .collect();
    let sources = ingest::admit_compiler_sources_for_scan(
        context.source_root,
        &scan.revision_fence,
        fresh,
        reused,
        scan.source_admission_policy,
        Some(cancellation.as_ref()),
    )
    .map_err(BuiltinModelError)?;
    let sources = sources
        .into_iter()
        .map(|source| {
            OwnedPackageSource::from_bytes(&source.relative_path, source.source)
                .map_err(|error| BuiltinModelError(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let coordinate = crate::builtin::compiler_scope::semantic_coordinate(
        context.package,
        profile,
        context.coordinate,
    )?;
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: context.correlation,
            profile,
            stage: backend_semantic::vocabulary::Stage::LowerIr,
        },
        coordinate,
    )
    .map_err(|error| BuiltinModelError(format!("Cargo target profile: {error:?}")))?;
    let request = OwnedPackageSourceSet::new(
        request,
        context.source_root.to_path_buf(),
        sources.into_boxed_slice(),
    )
    .and_then(|request| request.with_rust_cargo_target_name(selector.as_str()))
    .map_err(|error| BuiltinModelError(error.to_string()))?;
    let (compiler_target, closure) = compiler
        .resolve_rust_cargo_target_cancellable(&request, cancellation)
        .map_err(|error| BuiltinModelError(format!("resolve selected Cargo target: {error}")))?;
    let scope = backend_library::CargoTargetScope {
        name: backend_library::CargoTargetSelector::new(closure.target_name().to_owned())
            .map_err(|error| BuiltinModelError(error.to_owned()))?,
        root: closure.target_root().to_owned(),
        kinds: closure
            .target_kinds()
            .iter()
            .map(|kind| kind.to_string())
            .collect(),
        source_closure: closure.identity(),
        source_count: u64::try_from(closure.source_count())
            .map_err(|_| BuiltinModelError("Cargo source closure exceeds u64".to_owned()))?,
    };
    scope
        .validate()
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
    if scope.name != *selector {
        return Err(BuiltinModelError(
            "Cargo resolver returned another target name".to_owned(),
        ));
    }
    Ok(Some(ResolvedCargoTarget {
        compiler_target,
        scope,
    }))
}

fn prepare_native_frontier_changes_for_snapshot(
    snapshot: &WorkspaceSnapshot,
    project: [u8; 32],
    current: &[ProductNativeFrontierRecord],
    old_file_keys: &[[u8; 32]],
    source_relation: &backend_engine::WorkspaceRelationHandle<BuiltinWorkspaceRelation>,
) -> Result<Vec<crate::builtin::profile::BuiltinNativeFrontierChange>, BuiltinModelError> {
    let prior = product_native_frontier_relation(snapshot).map_err(|error| {
        BuiltinModelError(format!("open prior native source frontier: {error}"))
    })?;
    let mut after = current
        .iter()
        .cloned()
        .map(|record| {
            let key = product_native_frontier_record_key(&record)
                .map_err(|error| BuiltinModelError(error.to_owned()))?;
            Ok((key, record))
        })
        .collect::<Result<BTreeMap<_, _>, BuiltinModelError>>()?;
    let current_profiles = after
        .values()
        .filter_map(|record| match record {
            ProductNativeFrontierRecord::Profile {
                project: owner,
                profile,
                ..
            } if owner == &project => Some(*profile),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut previous_native_keys = BTreeMap::<[u8; 32], ProductNativeFrontierRecord>::new();
    if let Some(prior) = &prior {
        for source_key in old_file_keys {
            let Some(source) = source_relation.lookup(source_key).map_err(|error| {
                BuiltinModelError(format!("read prior native source row: {error}"))
            })?
            else {
                continue;
            };
            let Some(file) = source.file_fields() else {
                continue;
            };
            if file.project != project {
                continue;
            }
            let profile = match file.language {
                backend_engine::SourceLanguage::Clang => {
                    ingest::native_clang_profile_for_relative(file.path)
                }
                backend_engine::SourceLanguage::CSharp => {
                    ingest::native_csharp_profile_for_relative(file.path)
                }
                _ => None,
            };
            let Some(profile) = profile else {
                continue;
            };
            let path_key = product_native_frontier_path_key(project, profile, file.path)
                .map_err(BuiltinModelError)?;
            let path_record = prior.lookup(&path_key).map_err(|error| {
                BuiltinModelError(format!("read prior native frontier path: {error}"))
            })?;
            let mut keys = vec![
                path_key,
                product_native_frontier_profile_key(project, profile),
            ];
            if let Some(ProductNativeFrontierRecord::Path {
                disposition:
                    NativePathDisposition::ClangMalformed { chunk_count, .. }
                    | NativePathDisposition::CSharpMalformed { chunk_count, .. },
                ..
            }) = &path_record
            {
                for ordinal in 0..*chunk_count {
                    keys.push(
                        backend_engine::builtin::product_native_frontier_chunk_key(
                            project, profile, file.path, ordinal,
                        )
                        .map_err(BuiltinModelError)?,
                    );
                }
            }
            for key in keys {
                if let Some(record) = prior.lookup(&key).map_err(|error| {
                    BuiltinModelError(format!("read prior native frontier row: {error}"))
                })? {
                    previous_native_keys.insert(key, record);
                }
            }
        }
    }
    let old_profiles = previous_native_keys
        .values()
        .filter_map(|record| match record {
            ProductNativeFrontierRecord::Profile {
                project: owner,
                profile,
                ..
            } if owner == &project => Some(*profile),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    for profile in old_profiles.difference(&current_profiles) {
        after.remove(&product_native_frontier_profile_key(project, *profile));
    }
    let mut keys = after.keys().copied().collect::<BTreeSet<_>>();
    keys.extend(previous_native_keys.keys().copied());
    let mut changes = Vec::new();
    for key in keys {
        let expected = prior
            .as_ref()
            .map(|relation| relation.lookup(&key))
            .transpose()
            .map_err(|error| {
                BuiltinModelError(format!("read native frontier before row: {error}"))
            })?
            .flatten();
        let next = after.remove(&key);
        if expected != next {
            changes.push(crate::builtin::profile::BuiltinNativeFrontierChange {
                key,
                expected,
                after: next,
            });
        }
    }
    Ok(changes)
}

pub(super) fn native_candidate_permits(
    snapshot: &WorkspaceSnapshot,
    project: [u8; 32],
    records: &[ProductNativeFrontierRecord],
    candidate_profiles: &ingest::NativeCandidateProfiles,
) -> Result<NativeCandidatePermits, BuiltinModelError> {
    if candidate_profiles.clang.is_empty() && candidate_profiles.csharp.is_empty() {
        return Ok(NativeCandidatePermits::default());
    }
    let relation = product_native_frontier_relation(snapshot)
        .map_err(|error| BuiltinModelError(format!("open selected native frontier: {error}")))?
        .ok_or_else(|| BuiltinModelError("selected native frontier is missing".to_owned()))?;
    let selected_root = *relation.root().as_bytes();
    let mut clang_permits = BTreeMap::new();
    for profile in &candidate_profiles.clang {
        let paths = records
            .iter()
            .filter_map(|record| match record {
                ProductNativeFrontierRecord::Path {
                    project: owner,
                    profile: row_profile,
                    path,
                    ..
                } if owner == &project && row_profile == profile => Some(path.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let checked =
            backend_engine::builtin::admit_native_frontier(&relation, project, *profile, &paths)
                .map_err(|error| {
                    BuiltinModelError(format!("admit selected native frontier: {error}"))
                })?;
        if !checked.is_compilable() || checked.selected_root() != selected_root {
            return Err(BuiltinModelError(
                "checked native frontier is refused or has a different selected root".to_owned(),
            ));
        }
        let profile_permits = checked
            .dispositions()
            .iter()
            .filter_map(|(path, disposition)| {
                matches!(disposition, NativePathDisposition::ClangMalformed { .. })
                    .then(|| checked.candidate_permit(path))
                    .flatten()
            })
            .collect::<Vec<_>>();
        if profile_permits.is_empty() {
            return Err(BuiltinModelError(
                "native candidate profile has no malformed-source permits".to_owned(),
            ));
        }
        clang_permits.insert(*profile, profile_permits.into_boxed_slice());
    }
    let mut csharp_permits = BTreeMap::new();
    for profile in &candidate_profiles.csharp {
        if !matches!(profile, LanguageProfile::CSharp(_)) {
            return Err(BuiltinModelError(
                "native C# candidate profile is not C#".to_owned(),
            ));
        }
        let paths = records
            .iter()
            .filter_map(|record| match record {
                ProductNativeFrontierRecord::Path {
                    project: owner,
                    profile: row_profile,
                    path,
                    ..
                } if owner == &project && row_profile == profile => Some(path.clone()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let checked =
            backend_engine::builtin::admit_native_frontier(&relation, project, *profile, &paths)
                .map_err(|error| {
                    BuiltinModelError(format!("admit selected native C# frontier: {error}"))
                })?;
        if !checked.is_compilable() || checked.selected_root() != selected_root {
            return Err(BuiltinModelError(
                "checked native C# frontier is refused or has a different selected root".to_owned(),
            ));
        }
        let profile_permits = checked
            .dispositions()
            .iter()
            .filter_map(|(path, disposition)| {
                matches!(disposition, NativePathDisposition::CSharpMalformed { .. })
                    .then(|| checked.csharp_candidate_permit(path))
                    .flatten()
            })
            .collect::<Vec<_>>();
        if profile_permits.is_empty() {
            return Err(BuiltinModelError(
                "native C# candidate profile has no malformed-source permits".to_owned(),
            ));
        }
        csharp_permits.insert(*profile, profile_permits.into_boxed_slice());
    }
    Ok(NativeCandidatePermits {
        project: Some(project),
        selected_root: Some(selected_root),
        clang_by_profile: clang_permits,
        csharp_by_profile: csharp_permits,
    })
}

pub(super) fn prepare_scan_source(
    base_snapshot: &WorkspaceSnapshot,
    result: IndexScanResult,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut crate::builtin::semantic_authority::SemanticAuthority,
    operation_key: Option<backend_library::IndexOperationKey>,
) -> Result<PreparedScanSource, BuiltinModelError> {
    let current_root = base_snapshot.root();
    if current_root != result.work.workspace_root {
        return Err(BuiltinModelError(
            "workspace selection changed while the project scan was running; retry indexing"
                .to_owned(),
        ));
    }
    let native_root = crate::builtin::stylo_admission::stylo_basis_for_snapshot(base_snapshot)
        .map_err(BuiltinModelError)?
        .analysis_root;
    if native_root != result.work.native_root {
        return Err(BuiltinModelError(
            "native Stylo selection changed while the project scan was running; retry indexing"
                .into(),
        ));
    }
    let IndexScanResult {
        work,
        scan,
        workspace_snapshot,
    } = result;
    // A native-only walk does not witness global or ancestor ignore inputs.
    // Its repeated digest alone cannot authorize an old TS program or capture.
    let native_typescript_discovery_incomplete = scan.native_typescript_discovery_incomplete();
    let mut final_revision_fence = scan.revision_fence.clone();
    let cancellation = work.cancellation.clone();
    let relation = base_snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(|error| BuiltinModelError(format!("open product source: {error}")))?;
    let facts_relation = product_source_file_facts_relation(&base_snapshot)
        .map_err(|error| BuiltinModelError(format!("open product source facts: {error}")))?;
    let IndexScanWork {
        cargo_target,
        package,
        label,
        source_root: source_root_owned,
        coordinate: coordinate_owned,
        request_id,
        execution_intent,
        project_key,
        before,
        old_files,
        reusable,
        stylo_worker,
        stylo_selection_failure,
        sql_workers,
        sql_prior,
        capture_sql,
        ..
    } = work;

    let coordinate = coordinate_owned.as_ref();
    let revision_fence = &scan.revision_fence;
    if cancellation.load(Ordering::Acquire) {
        return Err(BuiltinModelError(ingest::INDEX_SCAN_CANCELLED.to_owned()));
    }
    if !ingest::compiler_revision_is_current_with_cancellation(
        revision_fence,
        Some(cancellation.as_ref()),
    )
    .map_err(BuiltinModelError)?
    {
        return Err(BuiltinModelError(
            "project root or files changed after source admission; retry indexing".to_owned(),
        ));
    }
    // Use the root pinned by the scan for configuration, compiler-input, and
    // profile reads. Keep the original spelling only for alias-retarget checks
    // at later admission fences.
    let source_root = revision_fence.canonical_root();
    let inputs = compiler_input_admissions(source_root, &scan, compiler);
    let mut semantic_context = SemanticCompilationContext::admit(
        package,
        &label,
        source_root,
        coordinate,
        request_id,
        compiler,
        scan.python_environment.as_ref(),
        cancellation.as_ref(),
    )?;
    let mut live = ingest::live_compiler_profiles(
        source_root,
        &scan.compiler_sources,
        &scan.reused_compiler_files,
    );
    live.extend(scan.native_clang_candidates.iter().map(|source| {
        ingest::compilation_profile(source_root, &source.relative_path, source.profile)
    }));
    live.extend(scan.native_csharp_candidates.iter().map(|source| {
        ingest::compilation_profile(source_root, &source.relative_path, source.profile)
    }));
    let rust_target = resolve_requested_cargo_target(
        compiler,
        &semantic_context,
        &scan,
        &live,
        cargo_target.as_ref(),
        Arc::clone(&cancellation),
    )?;
    let csharp_project = if live.iter().any(|profile| matches!(profile, LanguageProfile::CSharp(_))) {
        discover_observed_csharp_project(workspace_snapshot.as_ref())?
    } else { None };
    let csharp_shape = match (&csharp_project, workspace_snapshot.as_ref()) {
        (Some(selected), Some(workspace)) => Some(preflight_csharp_source_shape(&scan, workspace, source_root, selected)?),
        (Some(_), None) => return Err(BuiltinModelError("C# workspace snapshot is missing".to_owned())),
        _ => None,
    };
    semantic_context = semantic_context.with_rust_target(rust_target.as_ref()).with_csharp_project(csharp_project.as_ref());
    // Package observations and capture markers cannot stand in for a unit
    // selector. Refuse before creating or refreshing any product authority.
    if let Some(selected) = &csharp_project {
        let retained = selected.retained_bytes().ok_or_else(|| BuiltinModelError("C# capture accounting overflow".to_owned()))?;
        if retained > 4 * 1024 * 1024 || !selected.revalidate(revision_fence, cancellation.as_ref()).map_err(BuiltinModelError)? {
            return Err(BuiltinModelError("selected C# capture exceeds its bound or changed".to_owned()));
        }
        if operation_key.is_none() || live.len() != 1 || !scan.native_clang_candidates.is_empty()
            || !scan.native_csharp_candidates.is_empty() {
            return Err(BuiltinModelError("selected C# project awaits unit-scoped authority".to_owned()));
        }
    }
    let mut present =
        ingest::present_compiler_paths(&scan.compiler_sources, &scan.reused_compiler_files);
    present.extend(
        scan.native_clang_candidates
            .iter()
            .map(|source| source.relative_path.clone()),
    );
    present.extend(
        scan.native_csharp_candidates
            .iter()
            .map(|source| source.relative_path.clone()),
    );
    let lost = ingest::lost_compiler_profiles(source_root, &reusable, &present)
        .map_err(BuiltinModelError)?;
    // Each profile needs its own selected native producer fence. A matching
    // scan digest only identifies the candidate; replay proves its full read
    // closure, including dependencies and negative filesystem observations.
    let mut typescript_reused_profiles = BTreeSet::new();
    for &profile in &live {
        if native_typescript_discovery_incomplete
            || !matches!(profile, LanguageProfile::TypeScript(_))
            || lost.contains(&profile)
        {
            continue;
        }
        let binding = selected_semantic_binding_for_snapshot(
            base_snapshot,
            package,
            &label,
            coordinate,
            profile,
        )?;
        let input = semantic_input_digest(&scan, profile);
        let proof = binding.and_then(|binding| {
            compiler
                .typescript_snapshot_reuse(
                    source_root,
                    profile,
                    input,
                    binding,
                    cancellation.as_ref(),
                )
                .or_else(|| {
                    let package_reference =
                        backend_engine::PackageReference::parse(label.clone()).ok()?;
                    let semantic_coordinate = crate::builtin::compiler_scope::semantic_coordinate(
                        package, profile, coordinate,
                    )
                    .ok()?;
                    let key = ProductSemanticPublicationKey::new(
                        package_reference,
                        semantic_coordinate,
                        profile,
                    )
                    .ok()?;
                    let bytes = semantic_authority
                        .selected_typescript_snapshot_certificate(&key, binding, input)?;
                    compiler.restore_typescript_snapshot_certificate(
                        source_root,
                        profile,
                        input,
                        binding,
                        &bytes,
                        cancellation.as_ref(),
                    )
                })
        });
        if let Some(proof) = proof {
            typescript_reused_profiles.insert(profile);
            final_revision_fence.typescript_reuse.push(proof);
        }
    }
    // Persist candidate observations first. They fence compilation but remain
    // private to freshness and query paths until the combined product intent
    // commits.
    let mut observed_profiles = live.clone();
    observed_profiles.extend(lost.iter().copied());
    let mut observations = BTreeMap::new();
    for profile in observed_profiles {
        let coordinate = semantic_context.compiler_coordinate(profile)?;
        let key = ProductSemanticPublicationKey::new(
            semantic_context.package_reference.clone(),
            coordinate,
            profile,
        )
        .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let input_digest = semantic_target_input_digest(&scan, profile, rust_target.as_ref());
        let count = scan
            .compiler_sources
            .iter()
            .filter(|source| source.profile == profile)
            .count()
            .saturating_add(
                scan.reused_compiler_files
                    .iter()
                    .filter(|source| source.profile == profile)
                    .count(),
            )
            .saturating_add(
                scan.native_clang_candidates
                    .iter()
                    .filter(|source| {
                        ingest::compilation_profile(
                            source_root,
                            &source.relative_path,
                            source.profile,
                        ) == profile
                    })
                    .count(),
            )
            .saturating_add(
                scan.native_csharp_candidates
                    .iter()
                    .filter(|source| {
                        ingest::compilation_profile(
                            source_root,
                            &source.relative_path,
                            source.profile,
                        ) == profile
                    })
                    .count(),
            );
        let count = rust_target
            .as_ref()
            .filter(|_| matches!(profile, LanguageProfile::Rust(_)))
            .map_or(count, |target| target.scope.source_count as usize);
        let count = u64::try_from(count)
            .map_err(|_| BuiltinModelError("semantic source count exceeds u64".to_owned()))?;
        observations.insert(
            profile,
            semantic_authority.observe(&key, input_digest, count)?,
        );
    }
    let file_keys = scan.files.iter().map(|(key, _)| *key).collect::<Vec<_>>();
    let mut stylo_capture = if let Some(permit) = stylo_worker.as_deref() {
        match ingest::capture_stylo_project(
            &scan,
            permit,
            cancellation.as_ref(),
            std::time::Duration::from_secs(30),
            None,
        ) {
            Ok(capture) => Some(capture),
            Err(ingest::StyloProjectCaptureFailure::Unavailable) => None,
            Err(error) => {
                return Err(BuiltinModelError(format!(
                    "native Stylo source capture: {error:?}"
                )));
            }
        }
    } else {
        None
    };
    let mut source_selection = scan.source_selection.clone();
    if stylo_capture.is_some() {
        let css = source_selection
            .unsupported_extensions
            .iter_mut()
            .find(|row| row.extension == "css")
            .ok_or_else(|| {
                BuiltinModelError("same-walk CSS scope observation is missing".into())
            })?;
        // Claimed by native Stylo; parse and inventory coverage remain separate.
        css.files = 0;
    }
    let sql_projection = if !capture_sql { None } else { crate::builtin::sql_index::sql_projection(
        &scan.sql_walk,
        revision_fence,
        source_root,
        sql_prior,
        &sql_workers,
        &cancellation,
    )? };
    if let Some(projection) = &sql_projection {
        if let Some(sql) = source_selection
            .unsupported_extensions
            .iter_mut()
            .find(|row| row.extension == "sql")
        {
            // Hosts selected for SQL analysis are claimed; only files beyond the
            // selection bound remain disclosed as unsupported.
            sql.files = sql.files.saturating_sub(projection.host_count() as u64);
        }
    }
    let project_update = ProductSourceRecord::project_with_membership_pages_and_source_selection(
        &label,
        scan.source_version,
        file_keys.clone(),
        None,
        Some(source_selection),
    )
    .map_err(BuiltinModelError)?;
    let project = project_update.project_record().clone();
    // Capture a qualified latest attempt even when no native worker is
    // available; retained facts remain explicitly last-served.
    let selected_native =
        backend_engine::builtin::stylo_analysis_relation(base_snapshot).map_err(|error| {
            BuiltinModelError(format!("open selected native Stylo relation: {error}"))
        })?;
    let retained_selection = selected_native
        .as_ref()
        .map(|relation| relation.lookup(&backend_engine::builtin::stylo_selection_key(project_key)))
        .transpose()
        .map_err(|error| {
            BuiltinModelError(format!("read retained native Stylo selection: {error}"))
        })?
        .flatten();
    let prior_attempt = selected_native
        .as_ref()
        .map(|relation| relation.lookup(&backend_engine::builtin::stylo_attempt_key(project_key)))
        .transpose()
        .map_err(|error| BuiltinModelError(format!("read prior native Stylo attempt: {error}")))?
        .flatten();
    let css_may_exist = scan
        .stylo_walk
        .inventory()
        .is_none_or(|(count, _)| count != 0);
    let needs_attempt = stylo_capture.is_some()
        || css_may_exist
        || retained_selection.is_some()
        || prior_attempt.is_some();
    let stylo_proof = if needs_attempt {
        let attempt = ingest::capture_stylo_attempt(
            &scan,
            project.clone(),
            stylo_worker.as_deref(),
            stylo_selection_failure,
            stylo_capture.as_ref(),
            retained_selection.as_ref(),
            cancellation.as_ref(),
            std::time::Duration::from_secs(30),
            None,
        )
        .map_err(|error| BuiltinModelError(format!("native Stylo attempt: {error:?}")))?;
        if let Some(mut capture) = stylo_capture.take() {
            capture.bind_attempt(attempt);
            Some(crate::builtin::stylo_admission::StyloLiveProof::Captured {
                capture: Arc::new(capture),
                project: project.clone(),
                cancellation: Arc::clone(&cancellation),
            })
        } else {
            Some(
                crate::builtin::stylo_admission::StyloLiveProof::Unavailable {
                    attempt: Arc::new(attempt),
                    cancellation: Arc::clone(&cancellation),
                },
            )
        }
    } else {
        None
    };
    let sql_proof = sql_projection
        .map(|projection| crate::builtin::sql_index::sql_proof(projection, &project, &cancellation))
        .transpose()?;
    crate::builtin::sql_index::trace(format_args!("proof captured: {}", sql_proof.is_some()));
    let mut changes = Vec::new();
    if before.as_ref() != Some(&project) {
        changes.push(BuiltinSourceChange {
            key: project_key,
            after: Some(project.clone()),
        });
    }
    let new_page_keys = project_update
        .membership_pages()
        .iter()
        .map(|(key, _)| *key)
        .collect::<BTreeSet<_>>();
    for (key, page) in project_update.membership_pages() {
        let current = relation
            .lookup(key)
            .map_err(|error| BuiltinModelError(format!("read project membership page: {error}")))?;
        if current.as_ref() != Some(page) {
            changes.push(BuiltinSourceChange {
                key: *key,
                after: Some(page.clone()),
            });
        }
    }
    if let Some(previous) = before
        .as_ref()
        .and_then(ProductSourceRecord::project_fields)
    {
        changes.extend(
            previous
                .files
                .page_keys()
                .iter()
                .copied()
                .filter(|key| !new_page_keys.contains(key))
                .map(|key| BuiltinSourceChange { key, after: None }),
        );
    }
    for (key, record) in &scan.files {
        let current = relation
            .lookup(&key)
            .map_err(|error| BuiltinModelError(format!("read indexed source file: {error}")))?;
        if current.as_ref() != Some(record) {
            changes.push(BuiltinSourceChange {
                key: *key,
                after: Some(record.clone()),
            });
        }
    }
    let selected = file_keys.iter().copied().collect::<BTreeSet<_>>();
    changes.extend(
        old_files
            .iter()
            .copied()
            .filter(|key| !selected.contains(key))
            .map(|key| BuiltinSourceChange { key, after: None }),
    );
    let source_facts_changes = prepare_source_facts_changes(
        &relation,
        facts_relation.as_ref(),
        scan.source_facts,
        &scan.files,
        &old_files,
    )?;
    if package.to_bytes() != project_key {
        return Err(BuiltinModelError(
            "native frontier owner differs from the selected source project".to_owned(),
        ));
    }
    let mut native_frontier = scan.native_frontier.clone();
    for record in &mut native_frontier {
        if let ProductNativeFrontierRecord::Profile {
            project: owner,
            profile,
            package_target,
            ..
        } = record
            && *owner == project_key
        {
            *package_target = Some(crate::builtin::compiler_scope::semantic_coordinate(
                package, *profile, coordinate,
            )?);
        }
    }
    let native_frontier_changes = prepare_native_frontier_changes_for_snapshot(
        base_snapshot,
        project_key,
        &native_frontier,
        &old_files,
        &relation,
    )?;
    // Retain only checked after-images and selected compiler handles. Decoded
    // scan rows and configuration/accounting payloads are not compiler authority.
    drop(scan.files);
    drop(scan.encoded_fact_bytes);
    drop(scan.compiler_configuration);
    // Keep these source rows private until every semantic profile has been
    // admitted. The eventual BuiltinIntent carries source and semantic roots
    // in one workspace transition.
    // Profiles without a replayed compiler-owned witness still rebuild; the
    // source/configuration digest alone cannot authorize reuse.

    let fresh_profiles = scan
        .compiler_sources
        .iter()
        .map(|source| {
            ingest::compilation_profile(source_root, &source.relative_path, source.profile)
        })
        .chain(scan.native_clang_candidates.iter().map(|source| {
            ingest::compilation_profile(source_root, &source.relative_path, source.profile)
        }))
        .chain(scan.native_csharp_candidates.iter().map(|source| {
            ingest::compilation_profile(source_root, &source.relative_path, source.profile)
        }))
        .collect::<BTreeSet<_>>();
    let mut reusable_inputs = BTreeSet::new();
    for (profile, admitted) in &inputs {
        if native_typescript_discovery_incomplete
            && matches!(profile, LanguageProfile::TypeScript(_))
        {
            continue;
        }
        if compiler_exact_witness_is_current_for_snapshot(
            base_snapshot,
            package,
            &label,
            coordinate,
            compiler,
            source_root,
            *profile,
            *admitted,
            cancellation.as_ref(),
        )? {
            reusable_inputs.insert(*profile);
        }
    }
    if csharp_project.is_some() {
        reusable_inputs.retain(|profile| !matches!(profile, LanguageProfile::CSharp(_)));
    }
    let mut dirty =
        profiles_requiring_compilation(&live, &lost, &fresh_profiles, &inputs, &reusable_inputs);
    if csharp_project.is_some() {
        dirty.extend(live.iter().copied().filter(|profile| matches!(profile, LanguageProfile::CSharp(_))));
    }
    dirty.retain(|profile| !typescript_reused_profiles.contains(profile));
    let mut reused_profiles = live
        .intersection(&reusable_inputs)
        .filter(|profile| !dirty.contains(profile))
        .copied()
        .collect::<BTreeSet<_>>();
    // The native TSZ snapshot proof is independent of the compiler-input
    // replay cache above, but it authorizes the same exact selected profile
    // to be carried into this operation's refreshed capture.
    reused_profiles.extend(typescript_reused_profiles.iter().copied());
    let profiles = if dirty.is_empty() {
        let (captures, pending_changes, _) = prepare_source_capture_changes_for_snapshot(
            base_snapshot,
            package,
            &semantic_context,
            &reused_profiles,
            &observations,
            scan.source_version,
            operation_key,
        )?;
        PreparedScanProfiles::Reused {
            captures,
            reused_profiles,
            pending_changes,
        }
    } else {
        let mut captured_profiles = dirty.clone();
        captured_profiles.extend(reused_profiles.iter().copied());
        let (captures, pending_changes, _) = prepare_source_capture_changes_for_snapshot(
            base_snapshot,
            package,
            &semantic_context,
            &captured_profiles,
            &observations,
            scan.source_version,
            operation_key,
        )?;
        let (fresh, reused) = ingest::select_compiler_inputs(
            source_root,
            scan.compiler_sources,
            scan.reused_compiler_files,
            &dirty,
        );
        let native_clang_candidates = scan
            .native_clang_candidates
            .iter()
            .filter(|source| {
                dirty.contains(&ingest::compilation_profile(
                    source_root,
                    &source.relative_path,
                    source.profile,
                ))
            })
            .cloned()
            .collect();
        let native_csharp_candidates = scan
            .native_csharp_candidates
            .iter()
            .filter(|source| {
                dirty.contains(&ingest::compilation_profile(
                    source_root,
                    &source.relative_path,
                    source.profile,
                ))
            })
            .cloned()
            .collect();
        PreparedScanProfiles::Compile(Box::new(PreparedScanCompilation {
            fresh,
            reused,
            source_admission_policy: scan.source_admission_policy,
            observations,
            captures,
            reused_profiles,
            pending_changes,
            native_clang_candidates,
            native_csharp_candidates,
            native_frontier: scan.native_frontier.clone(),
        }))
    };
    Ok(PreparedScanSource {
        rust_target,
        csharp_project,
        csharp_shape,
        package,
        label,
        requested_root: source_root_owned,
        coordinate: coordinate_owned,
        request_id,
        execution_intent,
        project_key,
        before,
        project,
        file_keys,
        changes,
        source_facts_changes,
        native_frontier_changes,
        native_frontier,
        native_typescript_capture: scan.native_typescript_capture,
        revision_fence: final_revision_fence,
        python_environment: scan.python_environment,
        cancellation,
        workspace_snapshot,
        profiles,
        stylo_proof,
        sql_proof,
    })
}

pub(in crate::builtin::commands) fn prepare_source_scan(
    base_snapshot: &WorkspaceSnapshot,
    result: IndexScanResult,
    compiler: &LocalCompilerClient,
    semantic_authority: &mut crate::builtin::semantic_authority::SemanticAuthority,
    operation_key: Option<backend_library::IndexOperationKey>,
    route: Option<SourceRoute>,
    #[cfg(test)] before_materialization: &mut dyn FnMut(),
) -> Result<PreparedSourceScan, BuiltinModelError> {
    let PreparedScanSource {
        rust_target,
        csharp_project,
        csharp_shape,
        package,
        label,
        requested_root,
        coordinate,
        request_id,
        execution_intent,
        project_key,
        project,
        changes,
        mut source_facts_changes,
        mut native_frontier_changes,
        native_frontier: _,
        native_typescript_capture,
        revision_fence,
        python_environment,
        cancellation,
        workspace_snapshot,
        profiles,
        stylo_proof,
        sql_proof,
        ..
    } = prepare_scan_source(
        base_snapshot,
        result,
        compiler,
        semantic_authority,
        operation_key,
    )?;
    if native_typescript_capture.is_some() && route.is_some() {
        return Err(BuiltinModelError("native TypeScript with remote compilation is not available".into()));
    }
    let native_typescript = native_typescript_capture.map(|capture| {
        let mut bytes_read = 0usize;
        let verified = capture.into_revalidated(&mut bytes_read).map_err(BuiltinModelError)?;
        if !verified.matches_cancellation(&cancellation) {
            return Err(BuiltinModelError("native capture cancellation changed".into()));
        }
        let retained = verified.checked_retained_bytes()
            .and_then(|bytes| bytes.checked_add(2 * std::mem::size_of::<usize>()))
            .ok_or_else(|| BuiltinModelError("native capture retained size overflow".into()))?;
        if retained > backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES {
            return Err(BuiltinModelError("native capture retained size exceeds its bound".into()));
        }
        Ok(Arc::new(verified))
    }).transpose()?;
    let expected_project_commitment = selected_project_commitment(project_key, &project)?;
    let native_changed = stylo_proof
        .as_ref()
        .map(|proof| crate::builtin::stylo_admission::plan_stylo_proof(base_snapshot, proof))
        .transpose()
        .map_err(BuiltinModelError)?
        .is_some_and(|changes| !changes.is_empty());
    let sql_changed = sql_proof
        .as_ref()
        .map(|proof| crate::builtin::sql_admission::plan_sql_update(base_snapshot, proof))
        .transpose()
        .map_err(BuiltinModelError)?
        .is_some_and(|changes| !changes.is_empty());
    crate::builtin::sql_index::trace(format_args!("rows changed against the base: {sql_changed}"));
    let mixed_stylo_noop = if native_typescript.is_some() && !native_changed {
        stylo_proof.clone()
    } else {
        None
    };
    // The source/Pending transaction selects this exact capture first. Keep
    // its original proof through compiler work for the later TS grant too.
    let mixed_stylo_final = if native_typescript.is_some() { stylo_proof.clone() } else { None };
    let native_noop_root = crate::builtin::stylo_admission::stylo_basis_for_snapshot(base_snapshot)
        .map_err(BuiltinModelError)?
        .analysis_root;
    let compilation = match profiles {
        PreparedScanProfiles::Reused {
            captures,
            reused_profiles,
            pending_changes,
        } => {
            if native_typescript.is_some() {
                return Err(BuiltinModelError("native TypeScript requires a fresh compiler attempt".into()));
            }
            if captures.is_empty() {
                let intent = if changes.is_empty()
                    && source_facts_changes.is_empty()
                    && native_frontier_changes.is_empty()
                    && !native_changed
                    && !sql_changed
                {
                    None
                } else if changes.is_empty()
                    && source_facts_changes.is_empty()
                    && native_frontier_changes.is_empty()
                    && !native_changed
                {
                    Some(BuiltinIntent::sql_only(
                        package,
                        &label,
                        base_snapshot,
                        sql_proof
                            .clone()
                            .ok_or_else(|| BuiltinModelError("missing SQL proof".into()))?,
                    )?)
                } else if changes.is_empty()
                    && source_facts_changes.is_empty()
                    && native_frontier_changes.is_empty()
                {
                    let intent = BuiltinIntent::stylo_only(
                        package,
                        &label,
                        base_snapshot,
                        stylo_proof
                            .clone()
                            .ok_or_else(|| BuiltinModelError("missing Stylo proof".into()))?,
                    )?;
                    Some(match sql_proof.clone() {
                        Some(proof) => intent.with_sql_capture(base_snapshot, proof)?,
                        None => intent,
                    })
                } else {
                    let mut intent = reused_profiles_intent(
                        package,
                        &label,
                        changes,
                        std::mem::take(&mut source_facts_changes),
                    )?;
                    if !native_frontier_changes.is_empty() {
                        intent = intent
                            .with_native_frontier(std::mem::take(&mut native_frontier_changes))?;
                    }
                    if let Some(proof) = stylo_proof.clone() {
                        intent = intent.with_stylo_capture(base_snapshot, proof)?;
                    }
                    if let Some(proof) = sql_proof.clone() {
                        intent = intent.with_sql_capture(base_snapshot, proof)?;
                    }
                    Some(intent)
                };
                let native_noop_expectation = (intent.is_none()
                    && (stylo_proof.is_some() || native_noop_root.is_some()))
                .then_some(native_noop_root);
                let native_noop_proof = if intent.is_none() {
                    stylo_proof.clone().map(crate::builtin::stylo_admission::StyloGrantProof::Live)
                } else {
                    None
                };
                return Ok(PreparedSourceScan::Ready(PreparedProductSelection {
                    native_noop_expectation,
                    native_noop_proof,
                    intent,
                    selected: Vec::new(),
                    revision_fence: Some(revision_fence),
                    profile_refusals: Box::new([]),
                    partial_plan: None,
                }));
            }
            let mut intent = BuiltinIntent::index_with_capture(
                package,
                &label,
                changes,
                Vec::new(),
                pending_changes,
            )?;
            if !source_facts_changes.is_empty() {
                intent = intent.with_source_facts(std::mem::take(&mut source_facts_changes))?;
            }
            if !native_frontier_changes.is_empty() {
                if intent.has_capture_changes() && intent.capture_basis().is_none() {
                    intent = intent.with_capture_basis(
                        crate::builtin::capture_basis_for_snapshot(base_snapshot)?,
                    )?;
                }
                intent =
                    intent.with_native_frontier(std::mem::take(&mut native_frontier_changes))?;
            }
            if let Some(operation_key) = operation_key {
                intent = intent.with_operation_key(operation_key)?;
            }
            if let Some(proof) = stylo_proof.clone() {
                if intent.has_capture_changes() && intent.capture_basis().is_none() {
                    intent = intent.with_capture_basis(
                        crate::builtin::capture_basis_for_snapshot(base_snapshot)?,
                    )?;
                }
                intent = intent.with_stylo_capture(base_snapshot, proof)?;
            }
            if let Some(proof) = sql_proof.clone() {
                if intent.has_capture_changes() && intent.capture_basis().is_none() {
                    intent = intent.with_capture_basis(
                        crate::builtin::capture_basis_for_snapshot(base_snapshot)?,
                    )?;
                }
                intent = intent.with_sql_capture(base_snapshot, proof)?;
            }
            let selection = PreparedProductSelection {
                native_noop_expectation: None,
                native_noop_proof: None,
                intent: Some(intent),
                selected: Vec::new(),
                revision_fence: Some(revision_fence.clone()),
                profile_refusals: Box::new([]),
                partial_plan: None,
            };
            return Ok(PreparedSourceScan::Capture {
                selection,
                continuation: SourceCompilePlan {
                    native_typescript: None,
                    mixed_stylo_proof: None,
                    rust_target,
                    csharp_project: csharp_project.map(Arc::new),
                    csharp_shape,
                    expected_project_commitment,
                    pending_csharp_sources: None,
                    csharp_parked: false,
                    package,
                    label,
                    project_key,
                    coordinate,
                    request_id,
                    source_root: revision_fence.canonical_root().to_owned(),
                    revision_fence,
                    sources: Vec::new(),
                    observations: BTreeMap::new(),
                    captures,
                    reused_profiles,
                    operation_key,
                    reuse_only: true,
                    native_frontier: Vec::new(),
                    native_candidate_profiles: ingest::NativeCandidateProfiles::default(),
                    python_environment,
                    cancellation,
                    workspace_snapshot,
                    route,
                    execution_intent,
                },
            });
        }
        PreparedScanProfiles::Compile(compilation) => compilation,
    };
    let PreparedScanCompilation {
        fresh,
        reused,
        source_admission_policy,
        observations,
        captures,
        reused_profiles,
        pending_changes,
        native_clang_candidates,
        native_csharp_candidates,
        native_frontier: compilation_native_frontier,
    } = *compilation;
    // A single keyed C# profile can retain bounded handles until its source
    // selection is installed. Other paths retain the established materialization.
    let park_csharp = csharp_project.is_some() && csharp_shape.is_some()
        && operation_key.is_some() && route.is_none() && rust_target.is_none()
        && python_environment.is_none() && native_typescript.is_none()
        && native_clang_candidates.is_empty() && native_csharp_candidates.is_empty()
        && reused_profiles.is_empty() && observations.len() == 1 && captures.len() == 1;
    #[cfg(test)]
    if !park_csharp { before_materialization(); }
    let (sources, native_candidate_profiles, pending_csharp_sources) = if park_csharp {
        (Vec::new(), ingest::NativeCandidateProfiles::default(), Some(CSharpSourceHandles {
            fresh, reused, policy: source_admission_policy,
        }))
    } else {
        let (sources, native) = ingest::admit_compiler_sources_with_native_candidates_for_scan(
            &requested_root, &revision_fence, fresh, reused, native_clang_candidates,
            native_csharp_candidates, source_admission_policy, Some(cancellation.as_ref()),
        ).map_err(BuiltinModelError)?;
        (sources, native, None)
    };
    if native_typescript.is_some() && (sources.is_empty() || captures.is_empty()) {
        return Err(BuiltinModelError("native TypeScript empty scope requires explicit retirement".into()));
    }
    let mut intent =
        BuiltinIntent::index_with_capture(package, &label, changes, Vec::new(), pending_changes)?;
    if !source_facts_changes.is_empty() {
        intent = intent.with_source_facts(std::mem::take(&mut source_facts_changes))?;
    }
    if !native_frontier_changes.is_empty() {
        if intent.has_capture_changes() && intent.capture_basis().is_none() {
            intent = intent
                .with_capture_basis(crate::builtin::capture_basis_for_snapshot(base_snapshot)?)?;
        }
        intent = intent.with_native_frontier(std::mem::take(&mut native_frontier_changes))?;
    }
    if let Some(proof) = stylo_proof {
        if intent.has_capture_changes() && intent.capture_basis().is_none() {
            intent = intent
                .with_capture_basis(crate::builtin::capture_basis_for_snapshot(base_snapshot)?)?;
        }
        intent = intent.with_stylo_capture(base_snapshot, proof)?;
    }
    if let Some(proof) = sql_proof {
        if intent.has_capture_changes() && intent.capture_basis().is_none() {
            intent = intent
                .with_capture_basis(crate::builtin::capture_basis_for_snapshot(base_snapshot)?)?;
        }
        intent = intent.with_sql_capture(base_snapshot, proof)?;
    }
    let selection = PreparedProductSelection {
        native_noop_expectation: None,
        native_noop_proof: mixed_stylo_noop.map(crate::builtin::stylo_admission::StyloGrantProof::Live),
        intent: Some(intent),
        selected: Vec::new(),
        revision_fence: Some(revision_fence.clone()),
        profile_refusals: Box::new([]),
        partial_plan: None,
    };
    Ok(PreparedSourceScan::Capture {
        selection,
        continuation: SourceCompilePlan {
            native_typescript,
            mixed_stylo_proof: mixed_stylo_final,
            rust_target,
            csharp_project: csharp_project.map(Arc::new),
            csharp_shape,
            expected_project_commitment,
            pending_csharp_sources,
            csharp_parked: false,
            package,
            label,
            project_key,
            coordinate,
            request_id,
            source_root: revision_fence.canonical_root().to_owned(),
            revision_fence,
            sources,
            observations,
            captures,
            reused_profiles,
            operation_key,
            reuse_only: false,
            native_frontier: compilation_native_frontier,
            native_candidate_profiles,
            python_environment,
            cancellation,
            workspace_snapshot,
            route,
            execution_intent,
        },
    })
}

#[cfg(test)]
mod csharp_project_discovery_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static SERIAL: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn selected_source_witness_requires_same_bytes_and_original_cancellation() -> Result<(), String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        let source_root = temporary.path().join("source");
        let workspace = temporary.path().join("workspace");
        std::fs::create_dir_all(&source_root).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&workspace).map_err(|error| error.to_string())?;
        std::fs::write(source_root.join("App.csproj"), "<Project/>\n").map_err(|error| error.to_string())?;
        let bytes = b"class App {}\n".to_vec();
        std::fs::write(source_root.join("App.cs"), &bytes).map_err(|error| error.to_string())?;
        let captured_workspace = ingest::CompilerWorkspaceSnapshot::open(&source_root)?;
        let selected = resolve_csharp_selector(&captured_workspace, CSharpProjectSelector::Project("App.csproj".to_owned())).map_err(|error| error.to_string())?;
        let fence = ingest::csharp_test_revision_fence(&captured_workspace);
        let profile = LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp14);
        let content = backend_compile::typed_of::<backend_compile::InputContentSchema>(&bytes).to_bytes();
        let fact = ingest::SourceFactIdentity::from_canonical_bytes(&bytes);
        let source = ingest::CompilerSource { relative_path: "App.cs".to_owned(), profile, source: bytes.clone(), content, source_fact_identity: fact };
        let mut shape_hash = blake3::Hasher::new();
        shape_hash.update(b"backend.local-service.csharp-source-shape.v1\0");
        shape_hash.update(&("App.cs".len() as u64).to_le_bytes());
        shape_hash.update(b"App.cs"); shape_hash.update(&(bytes.len() as u64).to_le_bytes()); shape_hash.update(&1u64.to_le_bytes());
        let shape = CSharpScanShape { profile, digest: *shape_hash.finalize().as_bytes() };
        let label = "pkg:nuget/csharp-witness@1.0.0";
        let package = backend_engine::package_key(label);
        let project_key = package.to_bytes();
        let source_key = backend_engine::product_source_file_key(project_key, "App.cs");
        let project = ProductSourceRecord::project(label, [0x31; 32], vec![source_key])?;
        let file = ProductSourceRecord::file(project_key, "App.cs", backend_engine::SourceLanguage::CSharp,
            content, [0x21; 32], Vec::<backend_engine::SourceDeclaration>::new())?.with_source_identity(fact)?;
        let commitment = selected_project_commitment(project_key, &project).map_err(|e| e.to_string())?;
        crate::test_support::make_private(&workspace).map_err(|error| error.to_string())?;
        let descriptor = crate::builtin::profile_descriptor(crate::builtin::BuiltinProfile::Product).map_err(|e| e.to_string())?;
        let dispatcher = crate::builtin::builtin_dispatcher(Some(crate::builtin::ECHO_AUTHORITY_SECRET), Arc::clone(&descriptor), 60_000).map_err(|e| e.to_string())?;
        let mut daemon = crate::Locald::open_with_dispatcher_and_registry(&workspace, BuiltinModel, crate::builtin::genesis().map_err(|e| e.to_string())?, dispatcher,
            backend_engine::DaemonConfig::default(), crate::builtin::product_relation_registry().map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let operation = backend_library::IndexOperationKey::from_bytes([0x41; 32]).map_err(|e| e.to_string())?;
        let intent = BuiltinIntent::index_with_semantics(package, label, vec![
            BuiltinSourceChange { key: project_key, after: Some(project) },
            BuiltinSourceChange { key: source_key, after: Some(file) },
        ], Vec::new()).map_err(|e| e.to_string())?.with_operation_key(operation).map_err(|e| e.to_string())?;
        super::super::super::adapter::commit_builtin_intent(&mut daemon, 1, &intent).map_err(|e| e.to_string())?;
        let snapshot = daemon.engine().daemon().owner().snapshot();
        let receipt = super::super::super::adapter::SelectedCSharpSourceCommit::from_selected_test(&snapshot, Some(operation)).map_err(|e| e.to_string())?;
        let cancellation = Arc::new(AtomicBool::new(false));
        let mut plan = SourceCompilePlan {
            native_typescript: None, mixed_stylo_proof: None,
            rust_target: None, csharp_project: Some(Arc::new(selected)), csharp_shape: Some(shape), expected_project_commitment: commitment,
            pending_csharp_sources: Some(CSharpSourceHandles {
                fresh: vec![ingest::CompilerSourceHandle { relative_path: source.relative_path.clone(), profile: source.profile, content: source.content, source_fact_identity: source.source_fact_identity, expected_byte_length: None }],
                reused: Vec::new(),
                policy: crate::builtin::source_budget::SourceAdmissionPolicy::new(crate::builtin::source_budget::SourceAdmissionLimits::default()).map_err(|error| error.to_string())?,
            }),
            csharp_parked: false,
            package, label: label.to_owned(), project_key, coordinate: Some(PackageUrl::parse(label).map_err(|e| format!("{e:?}"))?), request_id: 1,
            source_root: source_root.clone(), revision_fence: fence, sources: Vec::new(), observations: BTreeMap::new(), captures: BTreeMap::new(),
            native_frontier: Vec::new(), native_candidate_profiles: ingest::NativeCandidateProfiles::default(), reused_profiles: BTreeSet::new(),
            operation_key: Some(operation), reuse_only: false, python_environment: None, cancellation: Arc::clone(&cancellation),
            workspace_snapshot: None, route: None, execution_intent: CompileExecutionIntent::Interactive,
        };
        plan.prepare_csharp_source_only_for_park().map_err(|e| e.to_string())?;
        let (_plan, witness) = plan.capture_csharp_admission(&snapshot, receipt).map_err(|e| e.to_string())?;
        assert!(witness.revalidate(&snapshot, &cancellation).map_err(|e| e.to_string())?);
        assert!(!witness.revalidate(&snapshot, &Arc::new(AtomicBool::new(false))).map_err(|e| e.to_string())?);
        std::fs::write(source_root.join("App.cs"), "class Else {}\n").map_err(|error| error.to_string())?;
        assert!(!witness.revalidate(&snapshot, &cancellation).map_err(|error| error.to_string())?);
        Ok(())
    }

    #[test]
    fn capture_credit_is_bounded_and_released_on_drop() {
        static TEST_USED: AtomicUsize = AtomicUsize::new(0);
        let mut leases = (0..4).map(|_| CSharpCaptureLease::acquire_from(&TEST_USED).unwrap()).collect::<Vec<_>>();
        assert!(CSharpCaptureLease::acquire_from(&TEST_USED).is_err());
        drop(leases.pop());
        let replacement = CSharpCaptureLease::acquire_from(&TEST_USED).unwrap();
        drop(replacement);
        drop(leases);
        assert_eq!(TEST_USED.load(Ordering::Acquire), 0);
        let _ = std::panic::catch_unwind(|| {
            let _lease = CSharpCaptureLease::acquire_from(&TEST_USED).unwrap();
            panic!("synthetic interrupted capture");
        });
        assert_eq!(TEST_USED.load(Ordering::Acquire), 0);
    }

    #[test]
    fn case_aliases_never_mint_two_project_units() -> Result<(), String> {
        let path = std::env::temp_dir().join(format!("csharp-case-{}-{}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
        let result = (|| {
            std::fs::write(path.join("App.csproj"), "upper").map_err(|error| error.to_string())?;
            std::fs::write(path.join("app.csproj"), "lower").map_err(|error| error.to_string())?;
            if std::fs::read(path.join("App.csproj")).map_err(|error| error.to_string())? == b"upper" {
                assert!(ingest::CompilerWorkspaceSnapshot::open(&path).is_err());
            } else {
                let snapshot = ingest::CompilerWorkspaceSnapshot::open(&path)?;
                let upper = resolve_csharp_selector(&snapshot, CSharpProjectSelector::Project("App.csproj".to_owned())).is_ok();
                let lower = resolve_csharp_selector(&snapshot, CSharpProjectSelector::Project("app.csproj".to_owned())).is_ok();
                assert_ne!(upper, lower);
            }
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(path);
        result
    }

    #[test]
    fn selected_unicode_project_uses_exact_captured_spelling() -> Result<(), String> {
        let path = std::env::temp_dir().join(format!("csharp-unicode-{}-{}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
        let result = (|| {
            std::fs::write(path.join("é.csproj"), "<Project/>\n").map_err(|error| error.to_string())?;
            let actual = std::fs::read_dir(&path).map_err(|error| error.to_string())?.next().ok_or("missing project")?.map_err(|error| error.to_string())?.file_name().into_string().map_err(|_| "non UTF-8 path")?;
            if actual == "é.csproj" {
                let snapshot = ingest::CompilerWorkspaceSnapshot::open(&path)?;
                let selected = resolve_csharp_selector(&snapshot, CSharpProjectSelector::Project(actual.clone())).map_err(|error| error.to_string())?;
                assert_eq!(selected.project_path, actual);
                assert!(resolve_csharp_selector(&snapshot, CSharpProjectSelector::Project("e\u{301}.csproj".to_owned())).is_err());
            } else {
                // The workspace policy refuses non-NFC on-disk spellings;
                // do not guess that an alias names the admitted project.
                assert!(ingest::CompilerWorkspaceSnapshot::open(&path).is_err());
            }
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(path);
        result
    }

    #[test]
    fn observed_single_project_is_captured_and_multiple_are_ambiguous() -> Result<(), String> {
        let path = std::env::temp_dir().join(format!("csharp-select-{}-{}", std::process::id(), SERIAL.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
        let result = (|| {
            std::fs::write(path.join("App.csproj"), "<Project/>\n").map_err(|error| error.to_string())?;
            let snapshot = ingest::CompilerWorkspaceSnapshot::open(&path)?;
            let selected = discover_observed_csharp_project(Some(&snapshot)).map_err(|error| error.to_string())?.ok_or("project not selected")?;
            assert_eq!(selected.project_path, "App.csproj");
            assert_eq!(selected.config.probes().count(), 3);
            let bytes = b"class App {}".to_vec();
            let source = ingest::CompilerSource {
                relative_path: "App.cs".to_owned(),
                profile: LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp14),
                content: backend_compile::typed_of::<backend_compile::InputContentSchema>(&bytes).to_bytes(),
                source_fact_identity: ingest::SourceFactIdentity::from_canonical_bytes(&bytes),
                source: bytes,
            };
            let (_, claim) = observed_csharp_source_claim(&selected, &source).map_err(|e| e.to_string())?.ok_or("source omitted")?;
            let request = PackageCompileRequest::new(
                GenerateTarget { correlation: CorrelationId(1), profile: source.profile, stage: backend_semantic::vocabulary::Stage::LowerIr },
                crate::builtin::compiler_scope::csharp_unit_coordinate(&PackageUrl::parse("pkg:nuget/csharp-selection@1.0.0").map_err(|e| format!("{e:?}"))?, &selected.project_path).map_err(|e| e.to_string())?,
            ).map_err(|e| format!("{e:?}"))?;
            let owned = OwnedPackageSource::from_bytes(&source.relative_path, source.source).map_err(|e| e.to_string())?;
            admitted_native_candidate_source_set(
                request, path.clone(), vec![owned].into_boxed_slice(), source.profile,
                &ingest::NativeCandidateProfiles::default(), &NativeCandidatePermits::default(), None,
                Some(&selected), Some(vec![claim].into_boxed_slice()),
            ).map_err(|e| e.to_string())?;
            std::fs::write(path.join("Other.csproj"), "<Project/>\n").map_err(|error| error.to_string())?;
            std::fs::write(path.join("App.slnx"), "<Solution><Project Path=\"App.csproj\"/><Project Path=\"Other.csproj\"/></Solution>").map_err(|error| error.to_string())?;
            let snapshot = ingest::CompilerWorkspaceSnapshot::open(&path)?;
            assert!(discover_observed_csharp_project(Some(&snapshot)).map_err(|error| error.to_string())?.is_none());
            let selected = resolve_csharp_selector(&snapshot, CSharpProjectSelector::SolutionProject { solution_path: "App.slnx".to_owned(), project_path: "Other.csproj".to_owned() }).map_err(|e| e.to_string())?;
            assert_eq!(selected.project_path, "Other.csproj");
            assert_eq!(selected.solution.as_ref().map(|solution| solution.path()), Some("App.slnx"));
            assert!(resolve_csharp_selector(&snapshot, CSharpProjectSelector::SolutionProject { solution_path: "App.slnx".to_owned(), project_path: "Missing.csproj".to_owned() }).is_err());
            assert!(solution_contains_project(b"<Solution><Unrelated><Project Path=\"App.csproj\"/></Unrelated></Solution>", "App.slnx", "App.csproj").is_err());
            assert!(solution_contains_project(b"<Solution><Project Path=\"App.csproj\"/><Project Path=\"App.csproj\"/></Solution>", "App.slnx", "App.csproj").is_err());
            assert!(solution_contains_project(b"<Solution><Project Path=\"App.csproj\"></Solution>", "App.slnx", "App.csproj").is_err());
            assert!(solution_contains_project("<Solution><Project Path=\"é.csproj\"/></Solution>".as_bytes(), "App.slnx", "é.csproj").map_err(|e| e.to_string())?);
            let deep = format!("<Solution>{}<Project Path=\"App.csproj\"/>{}</Solution>", "<Folder Name=\"x\">".repeat(17), "</Folder>".repeat(17));
            assert!(solution_contains_project(deep.as_bytes(), "App.slnx", "App.csproj").is_err());
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(path);
        result
    }

    #[test]
    fn captured_project_references_require_a_well_formed_project_root() {
        assert!(captured_csharp_project_references(
            br#"<NotProject><ItemGroup><ProjectReference Include="Api/Api.csproj"/></ItemGroup></NotProject>"#,
            "Tests/Tests.csproj",
        )
        .is_err());
        assert!(captured_csharp_project_references(
            br#"<Project><ItemGroup><ProjectReference Include="Api/Api.csproj"></ItemGroup></Project>"#,
            "Tests/Tests.csproj",
        )
        .is_err());
        assert!(captured_csharp_project_references(
            br#"<Project><ItemGroup><ProjectReference Include="Api/Api.csproj"/></ItemGroup></Project>"#,
            "Tests/Tests.csproj",
        )
        .is_ok());
    }
}

#[cfg(test)]
mod reused_profiles_intent_tests {
    use super::*;

    #[test]
    fn source_only_reused_change_uses_intent_without_empty_facts_section() {
        let label = "tinyioc fixture";
        let package = backend_engine::PackageKey::from_value(label);
        let project = ProductSourceRecord::new(label.to_owned()).expect("project record");
        let intent = reused_profiles_intent(
            package,
            label,
            vec![BuiltinSourceChange {
                key: package.to_bytes(),
                after: Some(project),
            }],
            Vec::new(),
        )
        .expect("source-only reused intent");

        let encoded = intent.encode();
        assert_eq!(&encoded[..4], b"BPI4");
        assert_eq!(encoded[4], 4);
        assert!(intent.source_facts_changes().is_empty());
    }

    #[test]
    fn reused_intent_keeps_facts_encoding_when_fact_rows_exist() {
        let label = "tinyioc fixture";
        let package = backend_engine::PackageKey::from_value(label);
        let update = backend_engine::build_product_source_file_facts(
            [1; 32],
            "Content/TinyIoC.cs",
            backend_engine::SourceLanguage::CSharp,
            [2; 32],
            [3; 32],
            backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                b"TinyIoC source facts",
            ),
            &[],
        )
        .expect("facts manifest");
        let (key, record) = update.into_rows().next().expect("manifest row");
        let intent = reused_profiles_intent(
            package,
            label,
            Vec::new(),
            vec![BuiltinSourceFactsChange {
                key,
                expected: None,
                after: Some(record),
            }],
        )
        .expect("facts-bearing reused intent");

        let encoded = intent.encode();
        assert_eq!(&encoded[..4], b"BPI7");
        assert_eq!(encoded[4], 7);
        assert_eq!(intent.source_facts_changes().len(), 1);
    }
}

impl SourceCompilePlan {
    pub(in crate::builtin::commands) fn requires_selected_csharp_admission(&self) -> bool {
        self.csharp_project.is_some()
    }

    pub(super) fn csharp_project(&self) -> &ResolvedCSharpProject { self.csharp_project.as_deref().expect("sealed C# capture owns selection") }
    pub(super) fn csharp_project_path(&self) -> &str { &self.csharp_project().project_path }
    pub(super) fn csharp_sources(&self) -> &[ingest::CompilerSource] { &self.sources }
    pub(super) fn csharp_revision_fence(&self) -> &ingest::CompilerRevisionFence { &self.revision_fence }
    pub(super) fn csharp_cancellation(&self) -> &Arc<AtomicBool> { &self.cancellation }
    pub(super) fn csharp_retained_bytes(&self) -> Option<usize> {
        let mut total = std::mem::size_of::<Self>()
            .checked_add(self.label.capacity())?.checked_add(self.source_root.capacity().checked_mul(4)?)?
            .checked_add(self.coordinate.as_ref().map_or(0, |coordinate| coordinate.as_str().len()))?
            .checked_add(self.csharp_project.as_ref()?.retained_bytes()?)?
            .checked_add(self.revision_fence.csharp_retained_bytes()?)?
            .checked_add(self.sources.capacity().checked_mul(std::mem::size_of::<ingest::CompilerSource>())?)?;
        for source in &self.sources {
            total = total.checked_add(source.relative_path.capacity())?.checked_add(source.source.capacity())?.checked_add(128)?;
        }
        if let Some(handles) = &self.pending_csharp_sources { total = total.checked_add(handles.retained_bytes()?)?; }
        // At most one selected C# observation/capture is parked; account for
        // their tree nodes, owned keys and bounded package/coordinate strings.
        if self.observations.len() > 1 || self.captures.len() > 1 { return None; }
        total = total.checked_add(self.observations.len().checked_mul(4096)?)?
            .checked_add(self.captures.len().checked_mul(ProductSourceRecord::MAX_LABEL_BYTES.checked_mul(4)?.checked_add(4096)?)?)?;
        // Arc counters plus atomic cancellation state; bounded overcharge.
        total.checked_add(512)
    }

    /// Drops the inventory and confirms that only bounded same-scan handles
    /// will be parked while the source-only commit is selected.
    pub(in crate::builtin::commands) fn prepare_csharp_source_only_for_park(&mut self) -> Result<(), BuiltinModelError> {
        if self.csharp_project.is_none() || self.csharp_shape.is_none() || self.pending_csharp_sources.is_none()
            || self.operation_key.is_none() || self.expected_project_commitment == [0; 32]
            || !self.sources.is_empty() || self.observations.len() > 1 || self.captures.len() > 1
            || !self.reused_profiles.is_empty() || self.rust_target.is_some() || self.python_environment.is_some()
            || self.route.is_some() || !self.native_candidate_profiles.clang.is_empty() || !self.native_candidate_profiles.csharp.is_empty() {
            return Err(BuiltinModelError("C# source-only plan is not isolated".to_owned()));
        }
        self.workspace_snapshot = None;
        self.native_frontier = Vec::new();
        if self.csharp_retained_bytes().is_none_or(|bytes| bytes > 12 * 1024 * 1024) {
            return Err(BuiltinModelError("C# source-only plan exceeds retained-byte bound".to_owned()));
        }
        self.csharp_parked = true;
        Ok(())
    }

    /// Consumes the exact source-only continuation only after the adapter has
    /// installed the matching source commit. The returned custody is not a
    /// compiler or unit-authority grant.
    pub(in crate::builtin::commands) fn capture_csharp_admission(
        mut self,
        snapshot: &WorkspaceSnapshot,
        receipt: super::super::adapter::SelectedCSharpSourceCommit,
    ) -> Result<(Self, CapturedCSharpUnitAdmission), BuiltinModelError> {
        if !self.csharp_parked || self.cancellation.load(Ordering::Acquire) || self.operation_key.is_none()
            || self.operation_key != receipt.operation_key()
            || *snapshot.root().as_bytes() != receipt.root() || snapshot.sequence() != receipt.sequence()
            || *snapshot.closure().binding().closure().as_bytes() != receipt.closure()
            || snapshot.owner_epoch() != receipt.owner_epoch() {
            return Err(BuiltinModelError("C# source selection receipt does not match the continuation".to_owned()));
        }
        let selected_request = snapshot.with_persisted_transition(|transition, _| transition.request())
            .map_err(|error| BuiltinModelError(format!("C# source snapshot is not selected: {error}")))?;
        if selected_request != receipt.request_identity() {
            return Err(BuiltinModelError("C# source request identity changed".to_owned()));
        }
        // This finite source-only path does not take custody of other profile
        // attempts, pending rows, or a full workspace inventory.
        if self.reuse_only || self.rust_target.is_some() || self.captures.len() > 1
            || self.observations.len() > 1 || !self.reused_profiles.is_empty()
            || !self.native_candidate_profiles.clang.is_empty() || !self.native_candidate_profiles.csharp.is_empty()
            || self.python_environment.is_some() || self.route.is_some() || self.workspace_snapshot.is_some() {
            return Err(BuiltinModelError("C# source continuation is not isolated".to_owned()));
        }
        self.native_frontier.clear();
        self.native_frontier.shrink_to_fit();
        if !self.sources.is_empty() { return Err(BuiltinModelError("C# sources were materialized before selection".to_owned())); }
        let handles = self.pending_csharp_sources.take().ok_or_else(|| BuiltinModelError("C# source handles are missing".to_owned()))?;
        self.sources = ingest::admit_compiler_sources_for_scan(
            self.revision_fence.requested_root(), &self.revision_fence, handles.fresh, handles.reused,
            handles.policy, Some(self.cancellation.as_ref()),
        ).map_err(BuiltinModelError)?;
        let selected = self.csharp_project.as_ref().ok_or_else(|| BuiltinModelError("C# selected project is missing".to_owned()))?;
        let shape = self.csharp_shape.as_ref().ok_or_else(|| BuiltinModelError("C# source preflight is missing".to_owned()))?;
        if self.sources.is_empty() || self.csharp_retained_bytes().is_none_or(|bytes| bytes > 12 * 1024 * 1024) {
            return Err(BuiltinModelError("C# witness retained bytes exceed bound".to_owned()));
        }
        let directory = selected.project_path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let mut hash = blake3::Hasher::new();
        hash.update(b"backend.local-service.csharp-source-shape.v1\0");
        let mut previous: Option<&str> = None;
        for source in &self.sources {
            if source.profile != shape.profile || (!directory.is_empty() && !source.relative_path.strip_prefix(directory).is_some_and(|suffix| suffix.starts_with('/')))
                || previous.is_some_and(|path| path >= source.relative_path.as_str()) {
                return Err(BuiltinModelError("C# materialized source set differs from selected scope".to_owned()));
            }
            previous = Some(&source.relative_path);
            hash.update(&(source.relative_path.len() as u64).to_le_bytes());
            hash.update(source.relative_path.as_bytes());
            hash.update(&(source.source.len() as u64).to_le_bytes());
        }
        hash.update(&(self.sources.len() as u64).to_le_bytes());
        if *hash.finalize().as_bytes() != shape.digest {
            return Err(BuiltinModelError("C# materialized sources differ from preflight".to_owned()));
        }
        let package_reference = backend_engine::PackageReference::parse(self.label.clone())
            .map_err(|error| BuiltinModelError(format!("C# package reference: {error:?}")))?;
        if backend_engine::package_key(package_reference.as_str()) != self.package || self.package.to_bytes() != self.project_key {
            return Err(BuiltinModelError("C# project package identity mismatch".to_owned()));
        }
        let coordinate = selected_project_coordinate(self.package, shape.profile, self.coordinate.as_ref(), Some(&selected.project_path))?;
        let key = ProductSemanticPublicationKey::new(package_reference, coordinate.clone(), shape.profile)
            .map_err(|error| BuiltinModelError(error.to_owned()))?;
        let target = CompilerPackageTargetV2::new(coordinate, backend_engine::application::CompilationUnitKeyV2::CSharpProject {
            project_path: selected.project_path.clone().into(),
        }).map_err(|error| BuiltinModelError(format!("C# unit target: {error:?}")))?;
        // A production plan may retain exactly one profile's original Pending
        // receipt. Bind it to this selected key and operation before carrying it
        // into the deferred compiler; the empty pair is used by source fixtures.
        match (self.captures.get(&key), self.observations.get(&shape.profile)) {
            (None, None) if self.captures.is_empty() && self.observations.is_empty() => {},
            (Some(capture), Some(observation)) if self.captures.len() == 1 && self.observations.len() == 1
                && capture.operation_key().copied() == self.operation_key.map(|key| key.to_bytes())
                && capture.observation_sequence() == observation.sequence()
                && observation.observation().revision().as_ref() == Some(capture.input_digest()) => {},
            _ => return Err(BuiltinModelError("C# pending capture differs from selected profile".to_owned())),
        }
        let (project, files, frontier) = bounded_csharp_project_frontier(snapshot, self.project_key, self.cancellation.as_ref())?;
        let project_commitment = selected_project_commitment(self.project_key, &project)?;
        if project.project_fields().is_none_or(|fields| fields.label != self.label)
            || project_commitment != self.expected_project_commitment {
            return Err(BuiltinModelError("selected C# Project differs from same-scan Project".to_owned()));
        }
        if !selected.revalidate(&self.revision_fence, self.cancellation.as_ref()).map_err(BuiltinModelError)?
            || !ingest::rematerialize_csharp_source_buffers(&self.revision_fence, &mut self.sources, &selected.project_path, shape.profile, self.cancellation.as_ref()).map_err(BuiltinModelError)?
            || !selected.revalidate(&self.revision_fence, self.cancellation.as_ref()).map_err(BuiltinModelError)?
            || self.csharp_retained_bytes().is_none_or(|bytes| bytes > 12 * 1024 * 1024) {
            return Err(BuiltinModelError("C# fresh materialization changed or exceeded its bound".to_owned()));
        }
        let source_commitment = checked_csharp_selected_sources(snapshot, self.project_key, &files, &self.sources, shape.profile, &selected.project_path, self.cancellation.as_ref())?;
        let cancellation = Arc::clone(&self.cancellation);
        let witness = CapturedCSharpUnitAdmission {
            project: Arc::clone(self.csharp_project.as_ref().expect("selected project checked")),
            revision_fence: self.revision_fence.clone(), sources: self.sources.clone(),
            cancellation: Arc::clone(&self.cancellation), receipt,
            pending_capture: self.captures.get(&key).cloned(),
            snapshot_root: *snapshot.root().as_bytes(), snapshot_sequence: snapshot.sequence(),
            snapshot_closure: *snapshot.closure().binding().closure().as_bytes(), owner_epoch: snapshot.owner_epoch(),
            project_key: key.package_key().to_bytes(), project_commitment, project_frontier: frontier,
            source_commitment, key, target,
        };
        if witness.retained_bytes().and_then(|bytes| bytes.checked_add(self.csharp_retained_bytes()?))
            .is_none_or(|bytes| bytes > 16 * 1024 * 1024) {
            return Err(BuiltinModelError("C# sealed witness exceeds retained-byte bound".to_owned()));
        }
        if !witness.revalidate(snapshot, &cancellation)? {
            return Err(BuiltinModelError("C# capture changed during selected-source admission".to_owned()));
        }
        Ok((self, witness))
    }

    /// Consumes the materialized plan while retaining its independent selected
    /// source custody through deferred compilation. This proof does not admit
    /// SDK, helper, runtime or project-graph closure.
    pub(in crate::builtin::commands) fn finish_with_csharp_admission(
        self,
        witness: CapturedCSharpUnitAdmission,
        snapshot: &WorkspaceSnapshot,
        compiler: &LocalCompilerClient,
        semantic: &mut crate::builtin::semantic_authority::SemanticAuthority,
        retained_attempts: &mut Vec<backend_extension_turso::CandidateAttempt>,
    ) -> Result<PreparedIndex, BuiltinModelError> {
        if !witness.revalidate(snapshot, &self.cancellation)?
            || self.csharp_project.as_ref().is_none_or(|project| !Arc::ptr_eq(project, &witness.project))
            || self.operation_key != witness.receipt.operation_key()
            || self.project_key != witness.project_key
            || self.captures.get(&witness.key) != witness.pending_capture.as_ref()
            || self.sources.len() != witness.sources.len()
            || self.sources.iter().zip(&witness.sources).any(|(a, b)| {
                a.relative_path != b.relative_path || a.profile != b.profile || a.source != b.source
                    || a.content != b.content || a.source_fact_identity != b.source_fact_identity
            }) {
            return Err(BuiltinModelError("C# compilation inputs differ from selected custody".to_owned()));
        }
        self.finish_inner(snapshot, compiler, semantic, retained_attempts, Some(Arc::new(witness)))
    }

    pub(in crate::builtin::commands) fn finish(
        self,
        snapshot: &WorkspaceSnapshot,
        compiler: &LocalCompilerClient,
        semantic: &mut crate::builtin::semantic_authority::SemanticAuthority,
        retained_attempts: &mut Vec<backend_extension_turso::CandidateAttempt>,
    ) -> Result<PreparedIndex, BuiltinModelError> {
        self.finish_inner(snapshot, compiler, semantic, retained_attempts, None)
    }

    fn finish_inner(
        self,
        snapshot: &WorkspaceSnapshot,
        compiler: &LocalCompilerClient,
        semantic: &mut crate::builtin::semantic_authority::SemanticAuthority,
        retained_attempts: &mut Vec<backend_extension_turso::CandidateAttempt>,
        witness: Option<Arc<CapturedCSharpUnitAdmission>>,
    ) -> Result<PreparedIndex, BuiltinModelError> {
        if self.reuse_only {
            return Ok(PreparedIndex::Ready(self.reused_selection(snapshot)?));
        }
        if self.route.is_some() {
            return Ok(PreparedIndex::Cluster(self));
        }
        let (project, files, frontier) =
            current_project_frontier_for_snapshot(snapshot, self.project_key)?;
        let context = SemanticCompilationContext::admit(
            self.package,
            &self.label,
            &self.source_root,
            self.coordinate.as_ref(),
            self.request_id,
            compiler,
            self.python_environment.as_ref(),
            self.cancellation.as_ref(),
        )?
        .with_rust_target(self.rust_target.as_ref())
        .with_csharp_project(self.csharp_project.as_deref());
        let mixed_stylo_proof = self.mixed_stylo_proof.as_ref().map(|proof| {
            crate::builtin::stylo_admission::SelectedStyloFence::from_selected(snapshot, proof, &self.cancellation)
                .map(|fence| crate::builtin::stylo_admission::StyloGrantProof::Selected(Arc::new(fence)))
                .map_err(BuiltinModelError)
        }).transpose()?;
        let native_typescript = self.native_typescript;
        let mut deferred = prepare_deferred_compile(
            snapshot,
            self.package,
            &self.label,
            self.project_key,
            project.clone(),
            Some(project),
            files,
            frontier,
            Vec::new(),
            &context,
            self.sources,
            self.native_candidate_profiles.clone(),
            native_candidate_permits(
                snapshot,
                self.project_key,
                &self.native_frontier,
                &self.native_candidate_profiles,
            )?,
            self.revision_fence,
            &self.observations,
            self.captures,
            self.reused_profiles,
            self.operation_key,
            semantic,
            witness.as_deref(),
            Some(retained_attempts),
        )?;
        deferred.native_typescript = native_typescript;
        deferred.mixed_stylo_proof = mixed_stylo_proof;
        deferred.csharp_capture = self.csharp_project;
        deferred.csharp_selected_source = witness;
        Ok(PreparedIndex::Compile(deferred))
    }

    /// Runs the configured cluster route and every local fallback on the
    /// same tracked worker. No remote decision can reenter the listener loop.
    pub(in crate::builtin::commands) fn compile_cluster(
        mut self,
        snapshot: &WorkspaceSnapshot,
        compiler: &LocalCompilerClient,
        semantic: &mut crate::builtin::semantic_authority::SemanticAuthority,
        retained_attempts: &mut Vec<backend_extension_turso::CandidateAttempt>,
        #[cfg(test)] before_local_compile: &mut dyn FnMut(),
    ) -> Result<PreparedProductSelection, BuiltinModelError> {
        if self.reuse_only {
            return self.reused_selection(snapshot);
        }
        let (project, files, frontier) =
            current_project_frontier_for_snapshot(snapshot, self.project_key)?;
        let context = SemanticCompilationContext::admit(
            self.package,
            &self.label,
            &self.source_root,
            self.coordinate.as_ref(),
            self.request_id,
            compiler,
            self.python_environment.as_ref(),
            self.cancellation.as_ref(),
        )?
        .with_rust_target(self.rust_target.as_ref())
        .with_csharp_project(self.csharp_project.as_deref());
        let route = self.route.as_ref().expect("configured route continuation");
        let mut compiled = compile_semantic_publications_for_snapshot(
            snapshot,
            &context,
            std::mem::take(&mut self.sources),
            &self.native_candidate_profiles,
            &native_candidate_permits(
                snapshot,
                self.project_key,
                &self.native_frontier,
                &self.native_candidate_profiles,
            )?,
            &self.revision_fence,
            &self.observations,
            semantic,
            self.workspace_snapshot.as_ref(),
            Some(route.owner.as_ref()),
            route.pending.as_ref(),
            self.execution_intent,
            Some(Arc::clone(&self.cancellation)),
            Some(retained_attempts),
            #[cfg(test)]
            before_local_compile,
        )?;
        // Reuse remains separate from compiler progress: this carries the
        // exact retained generation into the receipt without emitting a new
        // compiler-completed milestone.
        compiled
            .admitted
            .extend(super::reused_capture_publications_for_snapshot(
                snapshot,
                &self.captures,
                &self.reused_profiles,
                self.operation_key,
            )?);
        let count = compiled
            .admitted
            .len()
            .saturating_add(compiled.refused_profiles.len());
        let aliases = std::mem::take(&mut compiled.cargo_alias_observations);
        let mut source_changes = Vec::new();
        let relation = snapshot
            .relation::<BuiltinWorkspaceRelation>()
            .map_err(|e| BuiltinModelError(format!("open cluster project source: {e}")))?;
        replace_project_cargo_aliases(
            &mut source_changes,
            self.project_key,
            project.clone(),
            files.clone(),
            Some(&project),
            |key| {
                relation
                    .lookup(key)
                    .map_err(|e| BuiltinModelError(format!("cluster membership: {e}")))
            },
            aliases,
        )?;
        let job = DeferredIndex {
            native_typescript: None,
            mixed_stylo_proof: self.mixed_stylo_proof.as_ref().map(|proof| {
                crate::builtin::stylo_admission::SelectedStyloFence::from_selected(snapshot, proof, &self.cancellation)
                    .map(|fence| crate::builtin::stylo_admission::StyloGrantProof::Selected(Arc::new(fence)))
                    .map_err(BuiltinModelError)
            }).transpose()?,
            operation_key: self.operation_key,
            compiler: compiler.clone(),
            package: self.package,
            label: self.label,
            project_key: self.project_key,
            project_record: project.clone(),
            prior_project: Some(project),
            file_keys: files,
            prior_file_frontier: frontier,
            source_root: self.source_root,
            source_changes,
            revision_fence: self.revision_fence,
            csharp_capture: self.csharp_project,
            csharp_selected_source: None,
            profiles: VecDeque::new(),
            expected_profiles: count,
            completed_profiles: count,
            captures: self.captures,
            semantic_changes: compiled.changes,
            capture_publications: compiled.admitted,
            cargo_alias_observations: BTreeMap::new(),
            refused_profiles: compiled.refused_profiles,
        };
        finish_deferred_index_for_snapshot(snapshot, job, Some(semantic))
    }

    fn reused_selection(
        &self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<PreparedProductSelection, BuiltinModelError> {
        if !ingest::compiler_revision_is_current_with_cancellation(
            &self.revision_fence,
            Some(self.cancellation.as_ref()),
        )
        .map_err(BuiltinModelError)?
        {
            return Err(BuiltinModelError(
                "compiler source or configuration revision changed before reused capture terminal; retry indexing"
                    .to_owned(),
            ));
        }
        let admitted = super::reused_capture_publications_for_snapshot(
            snapshot,
            &self.captures,
            &self.reused_profiles,
            self.operation_key,
        )?;
        let completed = super::completed_capture_changes_for_snapshot(
            snapshot,
            &self.captures,
            &[],
            &admitted,
        )?;
        let mut intent = BuiltinIntent::index_with_capture(
            self.package,
            &self.label,
            Vec::new(),
            completed.retained_publications,
            completed.captures,
        )?;
        if let Some(operation_key) = self.operation_key {
            intent = intent.with_operation_key(operation_key)?;
        }
        Ok(PreparedProductSelection {
            native_noop_expectation: None,
            native_noop_proof: None,
            intent: Some(intent),
            selected: admitted
                .iter()
                .map(super::AdmittedCapturePublication::selected_claim)
                .collect(),
            revision_fence: Some(self.revision_fence.clone()),
            profile_refusals: Box::new([]),
            partial_plan: None,
        })
    }
}

/// Frozen terminalization request for the exact already-selected capture.
/// Its cancellation decision is final; recording that decision is separate
/// from authorizing any new compiler publication.
pub(in crate::builtin::commands) struct CaptureFailurePlan {
    pub(in crate::builtin::commands) attempts: Vec<backend_extension_turso::CandidateAttempt>,
    pub(in crate::builtin::commands) attempt_reason:
        Option<backend_extension_turso::CandidateAttemptRetirementReason>,
    pub(in crate::builtin::commands) package: backend_engine::PackageKey,
    pub(in crate::builtin::commands) label: String,
    pub(in crate::builtin::commands) captures:
        BTreeMap<ProductSemanticPublicationKey, SemanticSourceCapture>,
    pub(in crate::builtin::commands) reason: backend_engine::builtin::SemanticUnavailableReason,
    pub(in crate::builtin::commands) compiler_failure:
        Option<(LanguageProfile, backend_library::PackageCompilerFailure)>,
}

impl CaptureFailurePlan {
    pub(in crate::builtin::commands) fn prepare(
        &mut self,
        snapshot: &WorkspaceSnapshot,
        semantic: &mut crate::builtin::semantic_authority::SemanticAuthority,
    ) -> Result<PreparedProductSelection, BuiltinModelError> {
        if let Some(reason) = self.attempt_reason {
            for attempt in &self.attempts {
                semantic.retire_candidate_attempt(attempt, reason)?;
            }
        }
        let changes = terminal_capture_changes_for_snapshot(
            snapshot,
            &self.captures,
            self.reason,
            self.compiler_failure.clone(),
        )?;
        let intent = BuiltinIntent::index_with_capture(
            self.package,
            &self.label,
            Vec::new(),
            Vec::new(),
            changes,
        )?;
        Ok(PreparedProductSelection {
            native_noop_expectation: None,
            native_noop_proof: None,
            intent: Some(intent),
            selected: Vec::new(),
            revision_fence: None,
            profile_refusals: Box::new([]),
            partial_plan: None,
        })
    }
}
