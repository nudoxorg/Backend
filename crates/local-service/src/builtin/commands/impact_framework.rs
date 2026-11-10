//! Owner-side framework for the cross-language `impact` question.
//!
//! A language plugs in by implementing [`ImpactAdapter`]: it reads the selected
//! analysis for one project and returns a [`DomainLoad`], a directed graph in
//! the "affects" direction (a change to `from` can change `to`), the blind
//! spots of that graph, and the files whose bytes it read. The framework merges
//! the graphs of every registered adapter, resolves the seed, traverses with
//! the shared bounded primitive in `backend_semantic::impact`, and renders one
//! reply with strengths, gaps and freshness. Adapters never traverse, page,
//! seal handles, or word a reply; they only describe facts.
//!
//! Node keys are deterministic functions of `(domain, kind, scope, name)` so one
//! adapter can name a node another adapter owns without loading it. The CSS
//! adapter owns `class`, `id`, `custom-property`, `rule`, `at-rule`, `file`; the
//! code adapter links to those keys from `component` and `module` nodes. A SQL
//! adapter owns `table`, `column`, `view`, and can link to `component` keys.
use backend_library::impact::{
    IMPACT_MAX_DEPTH, ImpactCursor, ImpactCursorClaims, ImpactDirection, ImpactDomain,
    ImpactDomainBasis, ImpactDomainState, ImpactFreshness, ImpactGap, ImpactHandle,
    ImpactHandleClaims, ImpactNodeView, ImpactReport, ImpactRequest, ImpactRow, ImpactSeed,
    ImpactSeedSpec, ImpactSeedState, ImpactStrength, ImpactStrengthFilter, ImpactTerminal,
    ImpactTotals,
};
use backend_semantic::impact as traversal;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Stable identity of one node inside one project's graph.
pub(super) type Key = [u8; 32];

/// Computation bounds of one traversal. Independent of the output page.
const MAX_NODES: usize = 50_000;
const MAX_EDGES: usize = 400_000;
const MAX_CANDIDATES: usize = 50_000;
const MAX_ROWS: usize = 20_000;
const MAX_WITNESS_BYTES: usize = 32 * 1024 * 1024;
/// Files compared for freshness per answer.
const MAX_FRESHNESS_FILES: usize = 256;
/// Rows which carry a follow-up handle; later rows are re-asked by name.
const HANDLE_ROWS: u32 = 12;

/// Deterministic node identity. `scope` separates same-named entities, for
/// example a CSS-module class from a global class of the same name.
pub(super) fn node_key(domain: ImpactDomain, kind: &str, scope: &str, name: &str) -> Key {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.impact.node.v1\0");
    for part in [domain.as_str(), kind, scope, name] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    *hasher.finalize().as_bytes()
}

/// One node an adapter can describe.
#[derive(Clone, Debug)]
pub(super) struct Node {
    pub domain: ImpactDomain,
    /// Domain-scoped kind.
    pub kind: &'static str,
    /// Disambiguating scope shown only when names collide.
    pub scope: String,
    /// Human-readable name.
    pub name: String,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub within: Option<String>,
    /// Executable source-read follow-up, when the adapter has one.
    pub read: Option<String>,
    /// Further nodes which a seed on this node also changes (for example the
    /// rules which mention a class). They are followed with the seed itself.
    pub companions: Vec<Key>,
}
impl Node {
    pub(super) fn new(domain: ImpactDomain, kind: &'static str, scope: &str, name: &str) -> Self {
        Self {
            domain,
            kind,
            scope: scope.to_owned(),
            name: name.to_owned(),
            path: None,
            line: None,
            within: None,
            read: None,
            companions: Vec::new(),
        }
    }
}

/// One causal edge in the "affects" direction.
#[derive(Clone, Debug)]
pub(super) struct Edge {
    pub from: Key,
    pub to: Key,
    pub strength: ImpactStrength,
    /// Namespaced relation, for example `css.reads-property`.
    pub relation: &'static str,
    /// Short statement of the evidence on this edge.
    pub why: String,
}

/// A file whose bytes an adapter read, with the identity it recorded.
#[derive(Clone, Debug)]
pub(super) struct FileWitness {
    pub root: PathBuf,
    pub path: String,
    pub length: u64,
    /// Canonical source digest recorded at index time.
    pub source: Key,
}

/// Everything one adapter knows about one project.
pub(super) struct DomainLoad {
    pub nodes: BTreeMap<Key, Node>,
    pub edges: Vec<Edge>,
    pub gaps: Vec<ImpactGap>,
    pub basis: ImpactDomainBasis,
    pub files: Vec<FileWitness>,
    /// Every project path the index knows for this domain, parsed or not. Files
    /// on disk with a watched extension which are absent here are untracked.
    pub known_paths: BTreeSet<String>,
}
impl DomainLoad {
    pub(super) fn empty(domain: ImpactDomain, state: ImpactDomainState) -> Self {
        Self {
            nodes: BTreeMap::new(),
            edges: Vec::new(),
            gaps: Vec::new(),
            basis: ImpactDomainBasis { domain, state, files: 0 },
            files: Vec::new(),
            known_paths: BTreeSet::new(),
        }
    }
}

/// Inputs shared by every adapter in one request.
pub(super) struct ImpactContext<'a> {
    pub snapshot: &'a backend_engine::WorkspaceSnapshot,
    pub secret: &'a [u8; 32],
    pub package: &'a backend_library::PackageReference,
    pub project: Key,
    /// Canonical project directory, when the owner can name one.
    pub project_root: Option<PathBuf>,
    pub cancelled: &'a dyn Fn() -> bool,
}

/// The one trait a language implements to answer impact questions.
pub(super) trait ImpactAdapter: Sync {
    /// Domain this adapter owns.
    fn domain(&self) -> ImpactDomain;
    /// Read the selected analysis for the project. Missing worker, unselected
    /// analysis, or unsupported syntax must come back as state and gaps, never
    /// as an empty successful graph.
    fn load(&self, cx: &ImpactContext<'_>) -> DomainLoad;
    /// Whether `node` is what a caller means by `spec`. Called only for nodes of
    /// this adapter's domain.
    fn seed_matches(&self, spec: &ImpactSeedSpec, node: &Node) -> bool;
}

/// Adapters installed in this build, in load order. The SQL lane appends its
/// adapter here; its absence is reported as `NotInstalled`, never as no impact.
pub(super) fn adapters() -> &'static [&'static dyn ImpactAdapter] {
    static ADAPTERS: [&dyn ImpactAdapter; 3] = [&super::impact_css::CssAdapter, &super::impact_sql::SqlAdapter, &super::impact_code::CodeAdapter];
    &ADAPTERS
}

fn short(root: &[u8; 32]) -> String {
    root.iter().take(6).map(|byte| format!("{byte:02x}")).collect()
}

fn view(key: &Key, node: &Node, claims_root: &[u8; 32], project: &Key, secret: &[u8; 32]) -> ImpactNodeView {
    let handle = ImpactHandle::issue(
        ImpactHandleClaims {
            manifest_root: *claims_root,
            project: *project,
            domain: node.domain,
            node: *key,
        },
        secret,
    )
    .ok();
    ImpactNodeView {
        domain: node.domain,
        kind: node.kind.to_owned(),
        name: node.name.clone(),
        path: node.path.clone(),
        line: node.line,
        within: node.within.clone(),
        handle,
    }
}

/// Re-reads the files an answer depends on and compares their bytes with the
/// identity recorded at index time.
fn freshness(files: &BTreeMap<&str, &FileWitness>, wanted: &BTreeSet<String>) -> ImpactFreshness {
    use backend_version::{ContentId, SourceFactDomain};
    if files.is_empty() {
        return ImpactFreshness::Unknown { why: "no analyzed file backs this answer".to_owned() };
    }
    let mut checked = 0u32;
    let mut changed = Vec::new();
    let mut unchecked = 0u32;
    for path in wanted.iter().take(MAX_FRESHNESS_FILES) {
        let Some(witness) = files.get(path.as_str()) else {
            unchecked += 1;
            continue;
        };
        checked += 1;
        let current = super::super::ingest::read_stylo_selected_source(
            &witness.root,
            &witness.path,
            usize::try_from(witness.length).unwrap_or(usize::MAX).min(8 * 1024 * 1024),
        )
        .ok()
        .filter(|bytes| bytes.len() as u64 == witness.length)
        .map(|bytes| *ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes).as_ref());
        if current != Some(witness.source) {
            changed.push(witness.path.clone());
        }
    }
    let more = wanted.len().saturating_sub(MAX_FRESHNESS_FILES) as u32;
    if checked == 0 {
        return ImpactFreshness::Unknown { why: "the files behind this answer have no recorded identity".to_owned() };
    }
    if !changed.is_empty() {
        changed.truncate(8);
        return ImpactFreshness::Stale {
            checked,
            changed,
            action: "re-index the project, then ask again".to_owned(),
        };
    }
    if unchecked + more > 0 {
        return ImpactFreshness::Unknown {
            why: format!("{checked} files match the index; {} could not be compared", unchecked + more),
        };
    }
    ImpactFreshness::Current { checked }
}

/// Stylesheets and scripts on disk which no domain knows. A bounded walk which
/// skips dot-directories and common build output, so a hit is a likely new
/// source, never proof of one.
fn untracked_sources(root: &std::path::Path, known: &BTreeSet<String>) -> Vec<String> {
    const WATCHED: &[&str] = &["css", "tsx", "ts", "jsx", "js", "mjs", "cjs", "mts", "cts"];
    const SKIPPED: &[&str] = &["node_modules", "target", "dist", "build", "out", "coverage", "vendor", "__pycache__"];
    const MAX_ENTRIES: usize = 20_000;
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            seen += 1;
            if seen > MAX_ENTRIES {
                return found;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if depth < 16 && !name.starts_with('.') && !SKIPPED.contains(&name.as_str()) {
                    stack.push((entry.path(), depth + 1));
                }
            } else if kind.is_file()
                && !name.ends_with(".d.ts")
                && entry.path().extension().and_then(|e| e.to_str()).is_some_and(|e| WATCHED.contains(&e))
            {
                if let Ok(relative) = entry.path().strip_prefix(root) {
                    let relative = relative.to_string_lossy().replace('\\', "/");
                    if !known.contains(&relative) {
                        found.push(relative);
                    }
                }
            }
        }
    }
    found.sort();
    found
}

fn terminal_gap(stop: traversal::Stop) -> Option<&'static str> {
    match stop {
        traversal::Stop::Complete | traversal::Stop::Depth => None,
        traversal::Stop::Nodes => Some("node"),
        traversal::Stop::Edges => Some("edge"),
        traversal::Stop::Candidates => Some("candidate"),
        traversal::Stop::Rows => Some("row"),
        traversal::Stop::Cancelled => Some("cancellation"),
        traversal::Stop::WitnessBytes => Some("witness-byte"),
        traversal::Stop::InvalidAdjacency => Some("graph-consistency"),
    }
}

fn witness_of(index: usize) -> traversal::Witness {
    let mut out = [0u8; 32];
    out[..8].copy_from_slice(&(index as u64).to_le_bytes());
    out
}
fn index_of(witness: &traversal::Witness) -> usize {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&witness[..8]);
    u64::from_le_bytes(bytes) as usize
}

fn unavailable(
    root: &[u8; 32],
    domains: Vec<ImpactDomainBasis>,
    request: &ImpactRequest,
    state: ImpactSeedState,
    gaps: Vec<ImpactGap>,
    next: Vec<String>,
) -> ImpactReport {
    ImpactReport {
        basis: short(root),
        domains,
        seed_state: state,
        seeds: Vec::new(),
        direction: request.direction,
        depth: request.depth,
        strength_filter: request.strength,
        totals: ImpactTotals { exact: 0, candidate: 0, files: 0, by_kind: Vec::new() },
        rows: Vec::new(),
        gaps,
        freshness: ImpactFreshness::Unknown { why: "no analysis was read".to_owned() },
        terminal: ImpactTerminal::Complete,
        next,
    }
}

/// Entry used by the command adapter: checks that the project is part of the
/// selected workspace, names its directory, and answers.
pub(super) fn execute(
    snapshot: &backend_engine::WorkspaceSnapshot,
    secret: &[u8; 32],
    request: &ImpactRequest,
    cancelled: &dyn Fn() -> bool,
) -> Result<ImpactReport, String> {
    request.admit().map_err(|error| format!("impact request: {error}"))?;
    let project = backend_library::package_key(request.package.as_str()).to_bytes();
    let selected = snapshot
        .relation::<super::super::BuiltinWorkspaceRelation>()
        .map_err(|error| format!("workspace relation: {error}"))?
        .lookup(&project)
        .map_err(|error| format!("workspace lookup: {error}"))?
        .as_ref()
        .and_then(|row| row.project_fields())
        .is_some_and(|fields| fields.label == request.package.as_str());
    let root = *snapshot.root().as_bytes();
    if !selected {
        let why = "this project is not part of the indexed workspace; index it first".to_owned();
        let domains = [ImpactDomain::Css, ImpactDomain::Code]
            .into_iter()
            .map(|domain| ImpactDomainBasis {
                domain,
                state: ImpactDomainState::Unavailable { reason: why.clone() },
                files: 0,
            })
            .collect();
        return Ok(unavailable(&root, domains, request, ImpactSeedState::Unavailable { why }, Vec::new(), Vec::new()));
    }
    let label = std::path::Path::new(request.package.as_str());
    let project_root = label
        .is_absolute()
        .then(|| label.canonicalize().ok())
        .flatten()
        .filter(|canonical| canonical.as_path() == label);
    let cx = ImpactContext {
        snapshot,
        secret,
        package: &request.package,
        project,
        project_root,
        cancelled,
    };
    run(&cx, request)
}

/// Answer one request. This is the only entry the command adapter calls.
pub(super) fn run(
    cx: &ImpactContext<'_>,
    request: &ImpactRequest,
) -> Result<ImpactReport, String> {
    request.admit().map_err(|error| format!("impact request: {error}"))?;
    let mut loads = Vec::new();
    for adapter in adapters() {
        if (cx.cancelled)() {
            return Err("impact cancelled".to_owned());
        }
        loads.push((adapter.domain(), adapter.load(cx)));
    }
    answer(
        *cx.snapshot.root().as_bytes(),
        cx.project,
        cx.secret,
        loads,
        &|spec, node| {
            adapters()
                .iter()
                .find(|adapter| adapter.domain() == node.domain)
                .is_some_and(|adapter| adapter.seed_matches(spec, node))
        },
        request,
        cx.cancelled,
    )
}

/// Merge loaded domains and answer. Separate from [`run`] so tests can supply
/// hand-made graphs without an owner snapshot.
pub(super) fn answer(
    root: [u8; 32],
    project: Key,
    secret: &[u8; 32],
    loads: Vec<(ImpactDomain, DomainLoad)>,
    seed_matches: &dyn Fn(&ImpactSeedSpec, &Node) -> bool,
    request: &ImpactRequest,
    cancelled: &dyn Fn() -> bool,
) -> Result<ImpactReport, String> {
    let recipe = request.recipe();
    let mut offset = 0usize;
    if let Some(cursor) = &request.continuation {
        let claims = cursor.verify(secret).map_err(|_| "impact continuation is not valid for this owner".to_owned())?;
        if claims.manifest_root != root {
            return Err("impact continuation expired: the workspace changed since the first page; ask again without --cursor".to_owned());
        }
        if claims.recipe != recipe {
            return Err("impact continuation belongs to a different question; repeat the original arguments".to_owned());
        }
        offset = claims.offset as usize;
    }

    let mut nodes: BTreeMap<Key, Node> = BTreeMap::new();
    let mut edges: Vec<Edge> = Vec::new();
    let mut domains = Vec::new();
    let mut gaps: Vec<ImpactGap> = Vec::new();
    let mut files: Vec<FileWitness> = Vec::new();
    let mut known: BTreeSet<String> = BTreeSet::new();
    let mut loaded_domains = BTreeSet::new();
    for (domain, load) in loads {
        loaded_domains.insert(domain);
        known.extend(load.known_paths);
        domains.push(load.basis);
        gaps.extend(load.gaps);
        files.extend(load.files);
        nodes.extend(load.nodes);
        edges.extend(load.edges);
    }
    for domain in [ImpactDomain::Css, ImpactDomain::Code, ImpactDomain::Sql] {
        if !loaded_domains.contains(&domain) {
            domains.push(ImpactDomainBasis { domain, state: ImpactDomainState::NotInstalled, files: 0 });
        }
    }
    domains.sort_by_key(|basis| basis.domain);

    // Resolve the seed.
    let seed_keys: Vec<Key>;
    let mut ambiguous = false;
    match &request.seed {
        ImpactSeed::Handle { handle } => {
            let claims = handle.verify(secret).map_err(|_| "impact handle is not valid for this owner".to_owned())?;
            if claims.manifest_root != root || claims.project != project {
                return Err("impact handle expired: it was issued at another workspace root; ask again by name".to_owned());
            }
            if !nodes.contains_key(&claims.node) {
                return Ok(unavailable(&root, domains, request,
                    ImpactSeedState::NotFound { why: "the handle's node is not in the current analysis".to_owned() },
                    gaps, vec!["ask again by name".to_owned()]));
            }
            seed_keys = vec![claims.node];
        }
        ImpactSeed::Named(spec) => {
            let Some(basis) = domains.iter().find(|basis| basis.domain == spec.domain) else {
                return Err("unknown impact domain".to_owned());
            };
            match &basis.state {
                ImpactDomainState::NotInstalled => {
                    return Ok(unavailable(&root, domains.clone(), request,
                        ImpactSeedState::Unavailable { why: format!("{} impact is not installed in this build", spec.domain.as_str()) },
                        gaps, Vec::new()));
                }
                ImpactDomainState::Unavailable { reason } => {
                    let why = reason.clone();
                    return Ok(unavailable(&root, domains.clone(), request,
                        ImpactSeedState::Unavailable { why: why.clone() },
                        gaps, vec![format!("the {} analysis must be available before impact can be answered; this is not a result of 'no impact'", spec.domain.as_str())]));
                }
                _ => {}
            }
            let matches: Vec<Key> = nodes
                .iter()
                .filter(|(_, node)| node.domain == spec.domain && seed_matches(spec, node))
                .map(|(key, _)| *key)
                .collect();
            if matches.is_empty() {
                let why = format!(
                    "no {} named {:?} in the {} analysis of {} files; this does not cover sources the analysis cannot see (see gaps)",
                    spec.kind, spec.name, spec.domain.as_str(),
                    domains.iter().find(|b| b.domain == spec.domain).map_or(0, |b| b.files)
                );
                return Ok(unavailable(&root, domains, request, ImpactSeedState::NotFound { why }, gaps,
                    vec![format!("check the spelling, or list names with stylo-query for CSS, or pass --in PATH")]));
            }
            // Same-named entities in distinct scopes are distinct things.
            let scopes: BTreeSet<(&str, &str)> = matches
                .iter()
                .filter_map(|key| nodes.get(key))
                .filter(|node| !matches!(node.kind, "rule" | "at-rule" | "file"))
                .map(|node| (node.kind, node.scope.as_str()))
                .collect();
            if scopes.len() > 1 {
                ambiguous = true;
            }
            seed_keys = matches;
        }
    }
    let seed_views: Vec<ImpactNodeView> = seed_keys
        .iter()
        .take(16)
        .filter_map(|key| nodes.get(key).map(|node| view(key, node, &root, &project, secret)))
        .collect();
    if ambiguous {
        let mut report = unavailable(&root, domains, request, ImpactSeedState::Ambiguous, gaps,
            vec!["repeat with --in PATH to choose the defining file, or pass --handle from a seed above".to_owned()]);
        report.seeds = seed_views;
        return Ok(report);
    }

    // Seeds plus their companions (for example, the rules mentioning a class).
    let mut seeds: BTreeSet<Key> = BTreeSet::new();
    for key in &seed_keys {
        seeds.insert(*key);
        if let Some(node) = nodes.get(key) {
            seeds.extend(node.companions.iter().copied());
        }
    }
    let seed_nodes: Vec<Key> = seeds.iter().copied().collect();

    // Adjacency in the requested direction. The witness is the edge index.
    let mut adjacency: BTreeMap<traversal::Node, Vec<traversal::Edge>> = BTreeMap::new();
    for (index, edge) in edges.iter().enumerate() {
        if !nodes.contains_key(&edge.from) || !nodes.contains_key(&edge.to) {
            continue;
        }
        let (from, to) = match request.direction {
            ImpactDirection::Dependents => (edge.from, edge.to),
            ImpactDirection::Dependencies => (edge.to, edge.from),
        };
        adjacency.entry(from).or_default().push(traversal::Edge {
            from,
            to,
            strength: match edge.strength {
                ImpactStrength::Exact => traversal::Strength::Exact,
                ImpactStrength::Candidate => traversal::Strength::Candidate,
            },
            witness: witness_of(index),
        });
    }
    // Deterministic order independent of adapter iteration.
    for list in adjacency.values_mut() {
        list.sort_by(|a, b| (a.to, a.witness).cmp(&(b.to, b.witness)));
    }
    let requested_depth = usize::from(request.depth);
    let outcome = traversal::traverse(
        &adjacency,
        &seed_nodes,
        traversal::Limits {
            nodes: MAX_NODES,
            edges: MAX_EDGES,
            candidates: MAX_CANDIDATES,
            // One extra level detects, without reporting, work beyond the request.
            depth: (requested_depth + 1).min(usize::from(IMPACT_MAX_DEPTH) + 1),
            rows: MAX_ROWS,
            witness_bytes: MAX_WITNESS_BYTES,
        },
        Vec::new(),
        || cancelled(),
    );

    // Materialize rows: drop seeds, keep rows within the requested depth.
    let seed_set: BTreeSet<Key> = seeds;
    let mut beyond = BTreeSet::new();
    struct Pending {
        node: Key,
        strength: ImpactStrength,
        depth: u8,
        edge: usize,
        parent: Key,
        parent_strength: ImpactStrength,
    }
    let mut pending: Vec<Pending> = Vec::new();
    for row in &outcome.rows {
        let depth = row.witnesses.len();
        if seed_set.contains(&row.node) {
            continue;
        }
        if depth > requested_depth {
            beyond.insert(row.node);
            continue;
        }
        let Some(last) = row.witnesses.last() else { continue };
        let edge_index = index_of(last);
        let Some(edge) = edges.get(edge_index) else { continue };
        let parent = match request.direction {
            ImpactDirection::Dependents => edge.from,
            ImpactDirection::Dependencies => edge.to,
        };
        let prefix_strength = row.witnesses[..depth - 1]
            .iter()
            .filter_map(|w| edges.get(index_of(w)))
            .map(|e| e.strength)
            .max()
            .unwrap_or(ImpactStrength::Exact);
        pending.push(Pending {
            node: row.node,
            strength: match row.strength {
                traversal::Strength::Exact => ImpactStrength::Exact,
                traversal::Strength::Candidate => ImpactStrength::Candidate,
            },
            depth: u8::try_from(depth).unwrap_or(u8::MAX),
            edge: edge_index,
            parent,
            parent_strength: prefix_strength,
        });
    }
    pending.sort_by(|a, b| {
        let key = |p: &Pending| {
            let node = nodes.get(&p.node);
            (
                p.depth,
                p.strength,
                node.map(|n| n.kind),
                node.and_then(|n| n.path.clone()),
                node.and_then(|n| n.line),
                node.map(|n| n.name.clone()),
                p.node,
            )
        };
        key(a).cmp(&key(b))
    });

    // Totals are over distinct affected nodes, before filtering and paging.
    let mut exact_nodes: BTreeSet<Key> = BTreeSet::new();
    let mut all_nodes: BTreeSet<Key> = BTreeSet::new();
    for p in &pending {
        all_nodes.insert(p.node);
        if p.strength == ImpactStrength::Exact {
            exact_nodes.insert(p.node);
        }
    }
    let mut kinds: BTreeMap<String, u32> = BTreeMap::new();
    let mut paths: BTreeSet<String> = BTreeSet::new();
    for key in &all_nodes {
        if let Some(node) = nodes.get(key) {
            *kinds.entry(format!("{}.{}", node.domain.as_str(), node.kind)).or_default() += 1;
            if let Some(path) = &node.path {
                paths.insert(path.clone());
            }
        }
    }
    let totals = ImpactTotals {
        exact: exact_nodes.len() as u32,
        candidate: (all_nodes.len() - exact_nodes.len()) as u32,
        files: paths.len() as u32,
        by_kind: kinds.into_iter().collect(),
    };

    // Number the full ordered list, then filter and page.
    let mut number_of: BTreeMap<(Key, ImpactStrength), u32> = BTreeMap::new();
    let mut ordered: Vec<(u32, &Pending)> = Vec::new();
    for (index, p) in pending.iter().enumerate() {
        let n = u32::try_from(index + 1).map_err(|_| "impact row count overflow".to_owned())?;
        number_of.insert((p.node, p.strength), n);
        ordered.push((n, p));
    }
    let wanted = ordered.iter().filter(|(_, p)| request.strength == ImpactStrengthFilter::Any || p.strength == ImpactStrength::Exact);
    let selected: Vec<(u32, &Pending)> = wanted.copied().collect();
    let page_end = selected.len().min(offset.saturating_add(usize::from(request.page_rows)));
    let page = selected.get(offset.min(selected.len())..page_end).unwrap_or(&[]);
    let mut rows = Vec::with_capacity(page.len());
    let mut touched: BTreeSet<String> = BTreeSet::new();
    for (n, p) in page {
        let (Some(node), Some(edge)) = (nodes.get(&p.node), edges.get(p.edge)) else { continue };
        let via = if seed_set.contains(&p.parent) {
            0
        } else {
            number_of.get(&(p.parent, p.parent_strength)).copied().unwrap_or(0)
        };
        // A parent hidden by the filter or an earlier page is still a valid number.
        let via = if via >= *n { 0 } else { via };
        if let Some(path) = &node.path {
            touched.insert(path.clone());
        }
        rows.push(ImpactRow {
            n: *n,
            depth: p.depth,
            strength: p.strength,
            node: {
                let mut shown = view(&p.node, node, &root, &project, secret);
                // Handles cost tokens; the first rows carry one for follow-up questions.
                if *n > HANDLE_ROWS {
                    shown.handle = None;
                }
                shown
            },
            relation: edge.relation.to_owned(),
            why: edge.why.clone(),
            via,
            read: node.read.clone(),
        });
    }
    let terminal = if let Some(budget) = terminal_gap(outcome.stop) {
        ImpactTerminal::BudgetLimited { budget: budget.to_owned() }
    } else if page_end < selected.len() {
        let cursor = ImpactCursor::issue(
            ImpactCursorClaims {
                manifest_root: root,
                recipe,
                offset: u32::try_from(page_end).map_err(|_| "impact offset overflow".to_owned())?,
            },
            secret,
        )
        .map_err(|error| format!("impact cursor: {error}"))?;
        ImpactTerminal::More { cursor }
    } else {
        ImpactTerminal::Complete
    };
    if !beyond.is_empty() || matches!(outcome.stop, traversal::Stop::Depth) {
        gaps.push(ImpactGap {
            facet: "impact.depth".to_owned(),
            note: format!(
                "more nodes are reachable beyond {} steps; raise --depth (up to {IMPACT_MAX_DEPTH}) to follow them",
                request.depth
            ),
            count: (!beyond.is_empty()).then(|| beyond.len() as u32),
        });
    }
    if request.strength == ImpactStrengthFilter::Exact && totals.candidate > 0 {
        gaps.push(ImpactGap {
            facet: "impact.hidden-candidates".to_owned(),
            note: "the exact-only filter hides possible impacts; they are counted in the totals".to_owned(),
            count: Some(totals.candidate),
        });
    }
    // Seeds' own files and every shown row's file are compared with the disk.
    for key in &seed_keys {
        if let Some(path) = nodes.get(key).and_then(|node| node.path.clone()) {
            touched.insert(path);
        }
    }
    for p in pending.iter().take(MAX_FRESHNESS_FILES * 4) {
        if let Some(path) = nodes.get(&p.node).and_then(|node| node.path.clone()) {
            if touched.len() < MAX_FRESHNESS_FILES {
                touched.insert(path);
            }
        }
    }
    let by_path: BTreeMap<&str, &FileWitness> = files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut freshness = freshness(&by_path, &touched);
    if let Some(root_dir) = files.first().map(|f| f.root.clone()) {
        let untracked = untracked_sources(&root_dir, &known);
        if !untracked.is_empty() {
            let mut changed = match &freshness {
                ImpactFreshness::Stale { changed, .. } => changed.clone(),
                _ => Vec::new(),
            };
            let checked = match &freshness {
                ImpactFreshness::Current { checked } | ImpactFreshness::Stale { checked, .. } => *checked,
                ImpactFreshness::Unknown { .. } => 0,
            };
            changed.extend(untracked.iter().map(|path| format!("{path} (new, not indexed)")));
            changed.truncate(8);
            freshness = ImpactFreshness::Stale {
                checked,
                changed,
                action: "re-index the project, then ask again".to_owned(),
            };
        }
    }
    let mut next = Vec::new();
    if let Some(first) = rows.iter().find_map(|row| row.read.as_ref()) {
        next.push(format!("read the exact source: {first}"));
    }
    if rows.iter().any(|row| row.node.handle.is_some()) {
        next.push("follow a row further with --handle from its operand".to_owned());
    }
    Ok(ImpactReport {
        basis: short(&root),
        domains,
        seed_state: ImpactSeedState::Resolved,
        seeds: seed_views,
        direction: request.direction,
        depth: request.depth,
        strength_filter: request.strength,
        totals,
        rows,
        gaps,
        freshness,
        terminal,
        next,
    })
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn untracked_sources_skip_build_output_hidden_directories_and_known_files() {
        let root = std::env::temp_dir().join(format!("nudox-impact-untracked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for path in [
            "src/a.css",
            "src/new.css",
            "src/App.tsx",
            "src/types.d.ts",
            "node_modules/x/y.css",
            ".hidden/z.css",
            "dist/q.css",
            "docs/readme.md",
        ] {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().expect("parent")).expect("directory");
            std::fs::write(full, b"x").expect("file");
        }
        let known: BTreeSet<String> = ["src/a.css".to_owned(), "src/App.tsx".to_owned()].into();
        assert_eq!(untracked_sources(&root, &known), vec!["src/new.css".to_owned()]);
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn node_keys_separate_scope_and_kind() {
        let a = node_key(ImpactDomain::Css, "class", "", "title");
        let module = node_key(ImpactDomain::Css, "class", "src/a.module.css", "title");
        let id = node_key(ImpactDomain::Css, "id", "", "title");
        let code = node_key(ImpactDomain::Code, "class", "", "title");
        assert_eq!(a, node_key(ImpactDomain::Css, "class", "", "title"));
        assert!(a != module && a != id && a != code);
    }

    #[test]
    fn freshness_reports_changed_and_missing_files_and_never_current_without_evidence() {
        let root = std::env::temp_dir().join(format!("nudox-impact-fresh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("directory");
        std::fs::write(root.join("src/a.css"), b".a{}").expect("file");
        let digest = *backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(b".a{}").as_ref();
        let witness = FileWitness { root: root.clone(), path: "src/a.css".into(), length: 4, source: digest };
        let files: BTreeMap<&str, &FileWitness> = [("src/a.css", &witness)].into();
        let wanted: BTreeSet<String> = ["src/a.css".to_owned()].into();
        assert!(matches!(freshness(&files, &wanted), ImpactFreshness::Current { checked: 1 }));
        std::fs::write(root.join("src/a.css"), b".b{}").expect("edit");
        assert!(matches!(freshness(&files, &wanted), ImpactFreshness::Stale { .. }));
        std::fs::remove_file(root.join("src/a.css")).expect("remove");
        assert!(matches!(freshness(&files, &wanted), ImpactFreshness::Stale { .. }));
        assert!(matches!(freshness(&BTreeMap::new(), &wanted), ImpactFreshness::Unknown { .. }));
        std::fs::remove_dir_all(&root).expect("cleanup");
    }
}
