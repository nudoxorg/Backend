//! A bounded projection of the selected local index into the graph model.
//!
//! The graph is a view of one exact owner root. Package dependencies come from
//! dependency facts and declaration relations come from exact outline and
//! related reads. Every read is off the UI thread, and truncation or missing
//! relation authority remains visible in `Coverage`.

use crate::core::{ProducerAuthority, VersionedRoot};
use crate::model::pages::{PackageRef, SymbolRef};
use crate::runtime::offload::{Answer, Cancellation, Memo};
use crate::shell::bodies::graph::identity::{IdentityAdapter, ResolvedSymbol};
use backend_client::{ClientError, Session};
use backend_library::{
    CommandReply, DeclarationKind, DependencyFacts, PageContinuation, PageTerminal, RowId,
    SemanticLinkKind, SurfaceCommand, SurfaceReply,
};
use facet::graph::{Edge, Kind, Module, Node, Package, Rel, World};
use gpui::{App, Context, Global};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
#[cfg(test)]
use std::sync::atomic::{AtomicU64, Ordering};

const WORLD_READS: usize = 4;
const MAX_PACKAGES: usize = 64;
const OUTLINE_PAGE: u16 = 128;
const OUTLINE_PAGES: usize = 8;
const MAX_DECLARATIONS: usize = 2_048;
const MAX_RELATION_ROOTS: usize = 128;
const MAX_RELATIONS: usize = 12_000;
// The local service closes a connection after 256 frames. A declaration read
// sends a revision and a query, so rotate well before that boundary. Each
// new connection must prove the same root before contributing any facts.
const MAX_READS_PER_GRAPH_SESSION: usize = 64;

#[derive(Default)]
struct GraphReadBudget { reads: usize }

impl GraphReadBudget {
    fn next(&mut self) -> bool {
        let rotate = self.reads == MAX_READS_PER_GRAPH_SESSION;
        if rotate { self.reads = 0; }
        self.reads += 1;
        rotate
    }
}

fn admit_graph_revision(
    expected: ProducerAuthority,
    observed_root: backend_library::ViewStateRoot,
    observed_cursor: backend_library::Cursor,
    when: &str,
) -> Result<(), Arc<str>> {
    if observed_root != expected.root() || observed_cursor != expected.cursor() {
        Err(Arc::from(format!("the local index changed {when}")))
    } else {
        Ok(())
    }
}

struct RootedGraphSession {
    session: Session,
    endpoint: PathBuf,
    generation: crate::host::registry::CompositionGeneration,
    authority: ProducerAuthority,
    budget: GraphReadBudget,
}

impl RootedGraphSession {
    fn connect(
        composition: &crate::host::registry::Composition,
        authority: ProducerAuthority,
    ) -> Result<Self, Arc<str>> {
        let session = Session::connect(&composition.endpoint).map_err(|error| {
            Arc::<str>::from(format!("could not connect to the local index: {error}"))
        })?;
        let mut rooted = Self {
            session,
            endpoint: composition.endpoint.clone(),
            generation: composition.generation,
            authority,
            budget: GraphReadBudget::default(),
        };
        rooted.confirm("before the graph read began")?;
        Ok(rooted)
    }

    fn confirm(&mut self, when: &str) -> Result<(), Arc<str>> {
        if !crate::host::registry::composed().is_some_and(|current| {
            current.endpoint == self.endpoint && current.generation == self.generation
        }) {
            return Err(Arc::from("the local service connection changed during the graph read"));
        }
        let revision = self.session.revision().map_err(|error| {
            Arc::<str>::from(format!("could not confirm the graph's index root {when}: {error}"))
        })?;
        admit_graph_revision(self.authority, revision.root, revision.cursor(), when)
    }

    fn read<T>(
        &mut self,
        cancellation: &Cancellation,
        request: impl FnOnce(&mut Session) -> Result<T, ClientError>,
    ) -> Result<Result<T, ClientError>, Arc<str>> {
        ensure_active(cancellation)?;
        if self.budget.next() {
            self.confirm("between graph batches")?;
            self.session = Session::connect(&self.endpoint).map_err(|error| {
                Arc::<str>::from(format!("could not reconnect to the local index: {error}"))
            })?;
            self.confirm("after reconnecting the graph read")?;
        }
        let result = request(&mut self.session);
        if result.is_err() {
            // A missing optional fact is a gap only while its owner session
            // still proves this exact root. A broken stream or renewed owner
            // must terminate the projection, never become a partial graph.
            self.confirm("after a failed graph request")?;
        }
        Ok(result)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum OwnerIdentity {
    Indexed {
        endpoint: PathBuf,
        generation: crate::host::registry::CompositionGeneration,
    },
    #[cfg(test)]
    Synthetic(u64),
}

#[derive(Clone)]
pub(crate) struct Key {
    /// Canonical scene identity; `root` below also retains diagnostic
    /// observation metadata from the most recent request.
    authority: ProducerAuthority,
    root: VersionedRoot,
    owner: OwnerIdentity,
    /// Keep the current project in the bounded projection when a workspace
    /// has more indexed packages than the graph can represent at once.
    preferred: Option<PackageRef>,
    #[cfg(test)]
    synthetic: Option<Arc<TestProjection>>,
}

impl Key {
    pub(crate) fn at_authority(&self, root: VersionedRoot) -> bool { self.authority == root.authority() }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Key")
            .field("root", &self.root)
            .field("owner", &self.owner)
            .field("preferred", &self.preferred)
            .finish()
    }
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        self.authority == other.authority
            && self.owner == other.owner
            && self.preferred == other.preferred
    }
}

impl Eq for Key {}

impl std::hash::Hash for Key {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.authority, state);
        std::hash::Hash::hash(&self.owner, state);
        std::hash::Hash::hash(&self.preferred, state);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Origin {
    IndexedOwner,
    #[cfg(test)]
    SyntheticFixture(Arc<str>),
}

impl Origin {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::IndexedOwner => "Indexed graph",
            #[cfg(test)]
            Self::SyntheticFixture(_) => "Graph fixture",
        }
    }

    pub(crate) fn lower_label(&self) -> &'static str {
        match self {
            Self::IndexedOwner => "indexed graph",
            #[cfg(test)]
            Self::SyntheticFixture(_) => "graph fixture",
        }
    }

    #[cfg(test)]
    pub(crate) fn fixture_identity(&self) -> Option<&str> {
        match self {
            Self::IndexedOwner => None,
            Self::SyntheticFixture(identity) => Some(identity),
        }
    }
}

pub(crate) struct Projection {
    pub(crate) world: Arc<World>,
    pub(crate) scene: Arc<facet::graph::scene::Scene>,
    pub(crate) identities: Arc<IdentityAdapter>,
    pub(crate) coverage: Coverage,
    pub(crate) origin: Origin,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Coverage {
    pub(crate) packages: usize,
    /// Exact total when the package page completed; `None` when the owner
    /// reported a continuation and the graph intentionally stopped early.
    pub(crate) packages_total: Option<usize>,
    pub(crate) package_rows_scanned: usize,
    pub(crate) declarations: usize,
    pub(crate) relations: usize,
    pub(crate) relation_roots: usize,
    pub(crate) relation_gaps: usize,
    /// Exact relation records observed per compiler edge kind. Every kind's
    /// completeness remains Unknown because the owner has no whole-index
    /// relation coverage certificate on this read surface.
    pub(crate) relations_by_kind: BTreeMap<SemanticLinkKind, RelationKindCoverage>,
    pub(crate) dependency_gaps: usize,
    pub(crate) declaration_gaps: usize,
    pub(crate) containment_gaps: usize,
    pub(crate) source_gaps: usize,
    pub(crate) bounded: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RelationCompleteness {
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RelationKindCoverage {
    pub(crate) observed: usize,
    pub(crate) unavailable: usize,
    pub(crate) completeness: RelationCompleteness,
}

impl Coverage {
    /// Bounded visible provenance. Detail is available in the coverage disclosure.
    pub(crate) fn summary(&self, origin: &Origin) -> String {
        format!("{} · {} packages · {} declarations · {} observed relations · coverage incomplete",
            origin.label(), self.packages, self.declarations, self.relations)
    }

    pub(crate) fn words(&self, origin: &Origin) -> String {
        let mut words = format!(
            "{} · {} packages · {} declarations · {} relations",
            origin.label(),
            self.packages, self.declarations, self.relations
        );
        let gaps = self
            .relation_gaps
            .saturating_add(self.dependency_gaps)
            .saturating_add(self.declaration_gaps)
            .saturating_add(self.containment_gaps)
            .saturating_add(self.source_gaps);
        if gaps > 0 {
            words.push_str(&format!(" · {gaps} fact groups unavailable"));
        }
        if !self.relations_by_kind.is_empty() {
            let kinds = self
                .relations_by_kind
                .iter()
                .map(|(kind, facts)| {
                    let coverage = match facts.completeness {
                        RelationCompleteness::Unknown => "unknown",
                    };
                    format!(
                        "{} {} observed, {} unavailable, completeness {coverage}",
                        relation_name(*kind),
                        facts.observed,
                        facts.unavailable
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            words.push_str(&format!(" · relation coverage unknown by kind: {kinds}"));
        }
        if self.bounded {
            if let Some(total) = self.packages_total {
                let noun = if matches!(origin, Origin::IndexedOwner) {
                    "indexed packages"
                } else {
                    "fixture packages"
                };
                words.push_str(&format!(" · bounded from {total} {noun}"));
            } else {
                let source = if matches!(origin, Origin::IndexedOwner) {
                    "the owner reports more"
                } else {
                    "the fixture provides more"
                };
                words.push_str(&format!(
                    " · bounded after {} package rows; {source}",
                    self.package_rows_scanned
                ));
            }
        }
        words
    }
}

fn relation_name(kind: SemanticLinkKind) -> &'static str {
    match kind {
        SemanticLinkKind::Calls => "calls",
        SemanticLinkKind::MethodCall => "method calls",
        SemanticLinkKind::TypeReference => "type references",
        SemanticLinkKind::Reads => "reads",
        SemanticLinkKind::Writes => "writes",
        SemanticLinkKind::Imports => "imports",
        SemanticLinkKind::Implements => "implements",
        SemanticLinkKind::Overrides => "overrides",
        SemanticLinkKind::Reexports => "reexports",
        SemanticLinkKind::Inherits => "inherits",
        SemanticLinkKind::Documents => "documentation links",
    }
}

const SEMANTIC_LINK_KINDS: [SemanticLinkKind; 11] = [
    SemanticLinkKind::Calls,
    SemanticLinkKind::MethodCall,
    SemanticLinkKind::TypeReference,
    SemanticLinkKind::Reads,
    SemanticLinkKind::Writes,
    SemanticLinkKind::Imports,
    SemanticLinkKind::Implements,
    SemanticLinkKind::Overrides,
    SemanticLinkKind::Reexports,
    SemanticLinkKind::Inherits,
    SemanticLinkKind::Documents,
];

struct Reads(Memo<Key, Result<Arc<Projection>, Arc<str>>>);
impl Global for Reads {}

#[cfg(test)]
/// Holds an already-started projection future while navigation supersedes it.
/// Releasing it still returns a successful value to test Memo's discard path.
pub(crate) struct TestProjectionGate {
    state: Mutex<(bool, bool)>, // entered, released
    release_sender: async_channel::Sender<()>,
    release_receiver: async_channel::Receiver<()>,
}

#[cfg(test)]
impl Default for TestProjectionGate {
    fn default() -> Self {
        let (release_sender, release_receiver) = async_channel::bounded(1);
        Self {
            state: Mutex::new((false, false)),
            release_sender,
            release_receiver,
        }
    }
}

#[cfg(test)]
impl TestProjectionGate {
    pub(crate) async fn wait_for_read(&self) -> Result<(), Arc<str>> {
        let already_released = {
            let mut state = self.state.lock().expect("projection gate");
            state.0 = true;
            state.1
        };
        if !already_released {
            self.release_receiver
                .recv()
                .await
                .map_err(|_| Arc::<str>::from("the synthetic graph read gate closed"))?;
        }
        Ok(())
    }

    pub(crate) fn entered(&self) -> bool {
        self.state.lock().expect("projection gate").0
    }

    pub(crate) fn release(&self) {
        self.state.lock().expect("projection gate").1 = true;
        let _ = self.release_sender.try_send(());
    }
}

#[cfg(test)]
#[derive(Clone)]
struct TestProjection {
    id: u64,
    root: VersionedRoot,
    label: Arc<str>,
    world: Arc<World>,
    identities: Arc<IdentityAdapter>,
    gate: Option<Arc<TestProjectionGate>>,
}

#[cfg(test)]
impl Global for TestProjection {}

#[cfg(test)]
static NEXT_TEST_PROJECTION: AtomicU64 = AtomicU64::new(1);

impl Reads {
    fn new() -> Self {
        Self(Memo::new_cancellable_async(
            NonZeroUsize::new(WORLD_READS).expect("positive capacity"),
            |key, cancellation| async move { read(&key, &cancellation).await },
        ))
    }
}

/// Whether the exact indexed-world key still has a Memo read in flight.
pub(crate) fn read_in_flight(key: &Key, cx: &App) -> bool {
    cx.try_global::<Reads>()
        .is_some_and(|reads| reads.0.is_reading(key))
}

pub(crate) enum State {
    Reading,
    Waiting,
    Ready(Arc<Projection>),
    Unavailable(Arc<str>),
}

pub(crate) fn key<T: 'static>(
    root: VersionedRoot,
    preferred: Option<PackageRef>,
    cx: &mut Context<T>,
) -> Option<Key> {
    #[cfg(test)]
    let synthetic = cx
        .try_global::<TestProjection>()
        .filter(|projection| projection.root.same_authority(root))
        .cloned()
        .map(Arc::new);
    #[cfg(test)]
    let owner = if let Some(synthetic) = synthetic.as_ref() {
        OwnerIdentity::Synthetic(synthetic.id)
    } else {
        let composition = crate::host::registry::composed()?;
        OwnerIdentity::Indexed { endpoint: composition.endpoint.clone(), generation: composition.generation }
    };
    #[cfg(not(test))]
    let owner = {
        let composition = crate::host::registry::composed()?;
        OwnerIdentity::Indexed { endpoint: composition.endpoint.clone(), generation: composition.generation }
    };
    if cx.try_global::<Reads>().is_none() {
        cx.set_global(Reads::new());
    }
    let memo = cx.global::<Reads>().0.clone();
    let authority = root.authority();
    memo.retain_keys(|previous| previous.owner == owner && previous.authority == authority);
    Some(Key {
        authority,
        root,
        owner,
        preferred,
        #[cfg(test)]
        synthetic,
    })
}

#[cfg(test)]
pub(crate) fn install_test_projection(
    root: VersionedRoot,
    label: impl Into<Arc<str>>,
    world: Arc<World>,
    identities: Arc<IdentityAdapter>,
    gate: Option<Arc<TestProjectionGate>>,
    cx: &mut gpui::App,
) {
    let id = NEXT_TEST_PROJECTION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| next.checked_add(1))
        .expect("synthetic owner identity space exhausted");
    cx.set_global(TestProjection {
        id,
        root,
        label: label.into(),
        world,
        identities,
        gate,
    });
}

pub(crate) fn get<T: 'static>(key: &Key, cx: &mut Context<T>) -> State {
    if cx.try_global::<Reads>().is_none() {
        cx.set_global(Reads::new());
    }
    let memo = cx.global::<Reads>().0.clone();
    match memo.get(key, cx) {
        Answer::Reading => State::Reading,
        Answer::Deferred => State::Waiting,
        Answer::Failed(fault) => State::Unavailable(Arc::from(fault.to_string())),
        Answer::Ready(value) => match value.as_ref() {
            Ok(world) => State::Ready(Arc::clone(world)),
            Err(reason) => State::Unavailable(Arc::clone(reason)),
        },
    }
}

async fn read(key: &Key, cancellation: &Cancellation) -> Result<Arc<Projection>, Arc<str>> {
    ensure_active(cancellation)?;
    #[cfg(test)]
    if let OwnerIdentity::Synthetic(id) = &key.owner {
        let id = *id;
        let root = key.root.clone();
        let synthetic = key.synthetic.clone();
        return read_synthetic(synthetic, id, root).await;
    }
    let (endpoint, generation) = match &key.owner {
        OwnerIdentity::Indexed { endpoint, generation } => (endpoint, generation),
        #[cfg(test)]
        OwnerIdentity::Synthetic(_) => {
            return Err(Arc::from("the synthetic graph owner is unsupported"));
        }
    };
    let composition = crate::host::registry::composed()
        .filter(|composition| composition.endpoint == *endpoint && composition.generation == *generation)
        .ok_or_else(|| {
            Arc::from("the local service connection changed before the graph was read")
        })?;
    let mut session = RootedGraphSession::connect(&composition, key.authority)?;
    let mut packages = Vec::with_capacity(MAX_PACKAGES);
    let mut package_rows_scanned = 0usize;
    if let Some(preferred) = key.preferred.as_ref() {
        // The selected project is admitted independently by exact package
        // coordinate, so a bounded first page cannot make a later local root
        // disappear from its own graph.
        ensure_active(cancellation)?;
        if matches!(session.read(cancellation, |session| session.outline_page(preferred.as_str(), 1, None))?, Ok(reply)
            if matches!(reply.reply, CommandReply::ProjectionPage(_)))
    {
            packages.push(preferred.clone());
        }
    }
    ensure_active(cancellation)?;
    let package_page_limit = MAX_PACKAGES.saturating_sub(usize::from(!packages.is_empty()));
    let package_page_limit = u16::try_from(package_page_limit)
        .map_err(|_| Arc::<str>::from("the package graph page limit is invalid"))?;
    let package_page = session
        .read(cancellation, |session| session.package_page(package_page_limit, None))?
        .map_err(|error| Arc::<str>::from(format!("could not read indexed packages: {error}")))?;
    ensure_active(cancellation)?;
    let package_page = match package_page.reply {
        CommandReply::ProjectionPage(page) => page,
        _ => {
            return Err(Arc::from(
                "the local service returned an unexpected package page",
            ));
        }
    };
    package_rows_scanned = package_page.snapshot.root.rows().len();
    let package_total = match package_page.terminal {
        PageTerminal::Complete => Some(package_rows_scanned),
        PageTerminal::More(_) => None,
        PageTerminal::Cancelled => {
            return Err(Arc::from("the owner cancelled its package graph page"));
        }
    };
    for row in package_page.snapshot.root.rows() {
        if !matches!(row.id, RowId::Package(_)) {
            continue;
        }
        let Ok(package) = PackageRef::parse(&row.label) else {
            continue;
        };
        retain_package(&mut packages, package, key.preferred.as_ref());
    }
    if packages.is_empty() {
        return Err(Arc::from(
            "the current index has no packages to show in the graph",
        ));
    }
    let bounded = package_total.is_none() || package_rows_scanned > packages.len();

    let mut world_packages = Vec::with_capacity(packages.len());
    let mut package_index = BTreeMap::<PackageRef, u32>::new();
    for package in &packages {
        let index = u32::try_from(world_packages.len())
            .map_err(|_| Arc::<str>::from("the package graph is too large"))?;
        let release = package.release();
        world_packages.push(Package {
            name: release
                .as_ref()
                .map_or_else(
                    || package.display_name().to_owned(),
                    |r| r.name.as_str().to_owned(),
                )
                .into(),
            version: package
                .release_version()
                .unwrap_or_default()
                .to_owned()
                .into(),
            yours: package.is_local(),
            external: !package.is_local(),
            deps: Vec::new(),
        });
        package_index.insert(package.clone(), index);
    }

    let mut coverage = Coverage {
        packages: packages.len(),
        packages_total: package_total,
        package_rows_scanned,
        bounded,
        ..Coverage::default()
    };
    for kind in SEMANTIC_LINK_KINDS {
        coverage
            .relations_by_kind
            .insert(kind, RelationKindCoverage::default());
    }
    for (index, package) in packages.iter().enumerate() {
        ensure_active(cancellation)?;
        let reference = package.reference().clone();
        match session.read(cancellation, |session| session.surface(SurfaceCommand::Dependencies { package: reference }))? {
            Ok(SurfaceReply::Dependencies(DependencyFacts::Known(edges))) => {
                for edge in edges.iter() {
                    let Some(target) = edge.target.resolved.as_ref() else {
                        coverage.dependency_gaps += 1;
                        continue;
                    };
                    let exact = PackageRef::from_reference(target.clone());
                    if let Some(to) = package_index.get(&exact)
                        && !world_packages[index].deps.contains(to)
                    {
                        world_packages[index].deps.push(*to);
                    } else if !package_index.contains_key(&exact) {
                        // The owner supplied an exact dependency target that
                        // this bounded graph did not represent.
                        coverage.dependency_gaps += 1;
                    }
                }
            }
            Ok(SurfaceReply::Dependencies(
                DependencyFacts::Unknown(_) | DependencyFacts::Unavailable(_),
            )) => coverage.dependency_gaps += 1,
            Ok(_) | Err(_) => coverage.dependency_gaps += 1,
        }
    }

    let mut modules = Vec::<Module>::new();
    let mut module_index = BTreeMap::<(u32, String), u32>::new();
    let mut nodes = Vec::<Node>::new();
    let mut node_for_symbol = BTreeMap::<SymbolRef, u32>::new();
    let mut symbol_keys = HashMap::<backend_library::SymbolKey, u32>::new();
    let mut symbol_parents =
        HashMap::<backend_library::SymbolKey, Option<backend_library::SymbolKey>>::new();
    let mut resolved = BTreeMap::<u32, ResolvedSymbol>::new();
    let mut parents = Vec::<(u32, Option<backend_library::SymbolKey>)>::new();
    let mut relation_candidates = Vec::<SymbolRef>::new();

    'packages: for (package_position, package) in packages.iter().enumerate() {
        let package_id = u32::try_from(package_position)
            .map_err(|_| Arc::<str>::from("the package graph is too large"))?;
        let mut continuation: Option<PageContinuation> = None;
        for page_index in 0..OUTLINE_PAGES {
            ensure_active(cancellation)?;
            if nodes.len() >= MAX_DECLARATIONS {
                coverage.bounded = true;
                break 'packages;
            }
            let reply = match session.read(cancellation, |session| session.outline_page(package.as_str(), OUTLINE_PAGE, continuation))? {
                Ok(reply) => reply,
                Err(_) => {
                    coverage.declaration_gaps += 1;
                    break;
                }
            };
            let CommandReply::ProjectionPage(page) = reply.reply else {
                coverage.declaration_gaps += 1;
                break;
            };
            for row in page.snapshot.root.rows() {
                if !matches!(row.id, RowId::Symbol(_)) || row.kind.is_none() {
                    continue;
                }
                let Ok(symbol) = SymbolRef::new(&row.label) else {
                    continue;
                };
                if node_for_symbol.contains_key(&symbol) || nodes.len() >= MAX_DECLARATIONS {
                    continue;
                }
                let (file, line) = match &row.source {
                    backend_library::SourceAvailability::Captured(location) => (
                        Some(location.path().to_owned()),
                        Some(location.start_line()),
                    ),
                    _ => (None, None),
                };
                if file.is_none() {
                    coverage.source_gaps += 1;
                }
                let file_key = file
                    .clone()
                    .unwrap_or_else(|| format!("<source unavailable:{}>", package.as_str()));
                let module = *module_index
                    .entry((package_id, file_key.clone()))
                    .or_insert_with(|| {
                        let next = u32::try_from(modules.len()).unwrap_or(u32::MAX);
                        modules.push(Module {
                            pkg: package_id,
                            path: module_path(file.as_deref()).into(),
                            file: file_key.into(),
                        });
                        next
                    });
                if module == u32::MAX {
                    coverage.bounded = true;
                    continue;
                }
                let id = u32::try_from(nodes.len())
                    .map_err(|_| Arc::<str>::from("the declaration graph is too large"))?;
                let node = project_declaration(row, &symbol, package_id, module);
                nodes.push(node);
                parents.push((id, row.parent));
                node_for_symbol.insert(symbol.clone(), id);
                if let RowId::Symbol(key) = row.id {
                    symbol_keys.insert(key, id);
                    symbol_parents.insert(key, row.parent);
                }
                resolved.insert(
                    id,
                    ResolvedSymbol {
                        symbol: symbol.clone(),
                        package: package.clone(),
                        line,
                    },
                );
                if relation_candidates.len() < MAX_RELATION_ROOTS {
                    relation_candidates.push(symbol);
                } else {
                    coverage.bounded = true;
                    coverage.relation_gaps += 1;
                }
            }
            match page.terminal {
                PageTerminal::Complete => {
                    break;
                }
                PageTerminal::More(next) => {
                    continuation = Some(next);
                    if page_index + 1 == OUTLINE_PAGES {
                        coverage.bounded = true;
                    }
                }
                PageTerminal::Cancelled => {
                    coverage.declaration_gaps += 1;
                    break;
                }
            }
        }
    }
    coverage.declarations = nodes.len();
    for (id, parent) in parents {
        nodes[id as usize].parent = parent.and_then(|key| {
            let Some(parent_id) = symbol_keys.get(&key).copied() else {
                coverage.containment_gaps += 1;
                return None;
            };
            if symbol_parents.get(&key).is_some_and(Option::is_none) {
                Some(parent_id)
            } else {
                // The graph model supports one containment level. Preserve
                // nested declarations as items until it can represent their
                // full parent chain instead of building an invalid world.
                coverage.containment_gaps += 1;
                None
            }
        });
    }

    // Read a bounded, exact-root neighborhood for each represented symbol.
    // Missing rows, failed reads, and symbols past the cap contribute no edge
    // and remain explicit in Coverage instead of being inferred.
    let mut edges = Vec::<Edge>::new();
    let mut edge_keys = HashSet::<(u32, u32, u16)>::new();
    let mut semantic_edge_keys = HashSet::<(u32, u32, SemanticLinkKind)>::new();
    'relation_roots: for symbol in relation_candidates {
        ensure_active(cancellation)?;
        match session.read(cancellation, |session| session.related(symbol.as_str()))? {
            Ok(reply) => {
                let CommandReply::Graph(snapshot) = reply.reply else {
                    coverage.relation_gaps += 1;
                    continue;
                };
                let Some(relations) = snapshot.graph_relations.as_ref() else {
                    coverage.relation_gaps += 1;
                    continue;
                };
                coverage.relation_roots += 1;
                for (relation_index, relation) in relations.iter().enumerate() {
                    if relation_index % 128 == 0 {
                        ensure_active(cancellation)?;
                    }
                    if edges.len() >= MAX_RELATIONS {
                        coverage.bounded = true;
                        for omitted in &relations[relation_index..] {
                            relation_gap(&mut coverage, omitted.relation);
                        }
                        break 'relation_roots;
                    }
                    let (RowId::Symbol(from), RowId::Symbol(to)) = (relation.from, relation.to)
                    else {
                        relation_gap(&mut coverage, relation.relation);
                        continue;
                    };
                    let (Some(&from), Some(&to)) = (symbol_keys.get(&from), symbol_keys.get(&to))
                    else {
                        relation_gap(&mut coverage, relation.relation);
                        continue;
                    };
                    let Some(rel) = graph_relation(relation.relation) else {
                        relation_gap(&mut coverage, relation.relation);
                        continue;
                    };
                    if semantic_edge_keys.insert((from, to, relation.relation))
                        && let Some(facts) = coverage.relations_by_kind.get_mut(&relation.relation)
                    {
                        facts.observed += 1;
                    }
                    if edge_keys.insert((from, to, rel.0)) {
                        edges.push(Edge { from, to, rel });
                    }
                }
            }
            Err(_) => coverage.relation_gaps += 1,
        }
    }
    coverage.relations = edges.len();

    ensure_active(cancellation)?;
    session.confirm("while its graph was being read")?;
    let world = Arc::new(
        World::new(world_packages, modules, nodes, edges).map_err(|error| {
            Arc::<str>::from(format!("could not assemble the indexed graph: {error}"))
        })?,
    );
    let identities = Arc::new(IdentityAdapter::indexed(&world, &package_index, resolved));
    let layout = facet::graph::layout::layout_of(&world);
    let scene = Arc::new(facet::graph::scene::Scene::new(Arc::clone(&world), layout));
    Ok(Arc::new(Projection {
        world,
        scene,
        identities,
        coverage,
        origin: Origin::IndexedOwner,
    }))
}

#[cfg(test)]
async fn read_synthetic(
    synthetic: Option<Arc<TestProjection>>,
    id: u64,
    root: VersionedRoot,
) -> Result<Arc<Projection>, Arc<str>> {
    let synthetic = synthetic
        .filter(|projection| projection.id == id && projection.root.same_authority(root))
        .ok_or_else(|| Arc::<str>::from("the synthetic graph owner changed before its read"))?;
    if let Some(gate) = &synthetic.gate {
        // Deliberately finish despite a forgotten memo flight so the anatomy
        // regression proves late successful values cannot resurrect stale state.
        gate.wait_for_read().await?;
    }
    let world = Arc::clone(&synthetic.world);
    let layout = facet::graph::layout::layout_of(&world);
    let scene = Arc::new(facet::graph::scene::Scene::new(Arc::clone(&world), layout));
    Ok(Arc::new(Projection {
        world,
        scene,
        identities: Arc::clone(&synthetic.identities),
        coverage: Coverage::default(),
        origin: Origin::SyntheticFixture(Arc::clone(&synthetic.label)),
    }))
}

fn ensure_active(cancellation: &Cancellation) -> Result<(), Arc<str>> {
    if cancellation.is_cancelled() {
        Err(Arc::from("the indexed graph read was superseded"))
    } else {
        Ok(())
    }
}

fn retain_package(
    packages: &mut Vec<PackageRef>,
    candidate: PackageRef,
    preferred: Option<&PackageRef>,
) {
    if packages.contains(&candidate) {
        return;
    }
    let at = packages
        .binary_search_by(|present| package_order(present, &candidate, preferred))
        .unwrap_or_else(|at| at);
    if at >= MAX_PACKAGES {
        return;
    }
    packages.insert(at, candidate);
    packages.truncate(MAX_PACKAGES);
}

fn package_order(
    left: &PackageRef,
    right: &PackageRef,
    preferred: Option<&PackageRef>,
) -> std::cmp::Ordering {
    (preferred != Some(left), !left.is_local(), left.as_str()).cmp(&(
        preferred != Some(right),
        !right.is_local(),
        right.as_str(),
    ))
}

fn relation_gap(coverage: &mut Coverage, kind: SemanticLinkKind) {
    coverage.relation_gaps += 1;
    if let Some(facts) = coverage.relations_by_kind.get_mut(&kind) {
        facts.unavailable += 1;
    }
}

/// One projection seam shared by owner reads and presentation regressions.
/// The semantic identity stays in the adapter; display facts come only from
/// this declaration row, never from a same-name definition or package file.
pub(crate) fn project_declaration(row: &backend_library::Row, symbol: &SymbolRef, package_id: u32, module: u32) -> Node {
    let kind = graph_kind(row.kind.unwrap_or(DeclarationKind::Unknown));
    let identity = symbol.identity();
    let mut node = Node::new(kind, identity.name(), package_id, module);
    node.declaration_role = match row.kind.unwrap_or(DeclarationKind::Unknown) {
        DeclarationKind::Module => facet::graph::model::DeclarationRole::Module,
        DeclarationKind::Import => facet::graph::model::DeclarationRole::Import,
        DeclarationKind::Unknown => facet::graph::model::DeclarationRole::Unknown,
        _ => facet::graph::model::DeclarationRole::Definition,
    };
    node.source_status = graph_source_status(&row.source);
    if let backend_library::SourceAvailability::Captured(location) = &row.source {
        node.line = location.start_line();
        node.file = Some(location.path().to_owned().into());
    }
    node.vis = None;
    node.sig = row.signature.as_deref().map(Into::into);
    node.doc = row
        .document
        .iter()
        .filter_map(|fragment| match fragment {
            backend_library::Fragment::Text(text) => Some(text.as_str()),
            _ => None,
        })
        .next()
        .map(Into::into);
    node.deprecated =
        matches!(row.facts.deprecation, backend_library::Fact::Present(_));
    node
}

fn graph_source_status(source: &backend_library::SourceAvailability) -> facet::graph::model::SourceStatus {
    use backend_library::SourceAvailability as S;
    use facet::graph::model::SourceStatus as G;
    match source {
        S::Captured(_) => G::Captured,
        S::NotCaptured => G::NotCaptured,
        S::NotHydrated => G::NotHydrated,
        S::Unconfigured => G::Unconfigured,
        S::StaleFile { .. } => G::Stale,
    }
}

fn graph_kind(kind: DeclarationKind) -> Kind {
    match kind {
        DeclarationKind::Class | DeclarationKind::Struct => Kind::Struct,
        DeclarationKind::Enum => Kind::Enum,
        DeclarationKind::Union => Kind::Union,
        DeclarationKind::Trait | DeclarationKind::Interface => Kind::Trait,
        DeclarationKind::Type => Kind::Type,
        DeclarationKind::Function | DeclarationKind::Constructor => Kind::Function,
        DeclarationKind::Method | DeclarationKind::Property => Kind::Method,
        DeclarationKind::Macro => Kind::Macro,
        DeclarationKind::Constant | DeclarationKind::Variable => Kind::Constant,
        DeclarationKind::Field => Kind::Field,
        DeclarationKind::Variant => Kind::Variant,
        DeclarationKind::Module | DeclarationKind::Import | DeclarationKind::Unknown => Kind::Other,
    }
}

fn graph_relation(relation: SemanticLinkKind) -> Option<Rel> {
    match relation {
        SemanticLinkKind::Calls | SemanticLinkKind::MethodCall => Some(Rel::CALLS),
        SemanticLinkKind::TypeReference | SemanticLinkKind::Reexports => Some(Rel::TYPE),
        SemanticLinkKind::Reads
        | SemanticLinkKind::Writes
        | SemanticLinkKind::Imports
        | SemanticLinkKind::Documents => Some(Rel::USES),
        SemanticLinkKind::Implements => Some(Rel::IMPL),
        SemanticLinkKind::Overrides | SemanticLinkKind::Inherits => Some(Rel::IS),
    }
}

fn module_path(file: Option<&str>) -> String {
    let Some(file) = file else {
        return "source location unavailable in this graph".to_owned();
    };
    let path = Path::new(file);
    let relative = path.strip_prefix("src").unwrap_or(path);
    let without_extension = relative.with_extension("");
    let mut components = without_extension
        .components()
        .filter_map(|part| part.as_os_str().to_str())
        .collect::<Vec<_>>();
    if components
        .last()
        .is_some_and(|name| matches!(*name, "lib" | "main" | "mod"))
    {
        components.pop();
    }
    components.join("::")
}

#[cfg(test)]
mod tests {
    use crate::core::VersionedRoot;
    use super::{
        Coverage, GraphReadBudget, Key, MAX_PACKAGES, Origin, OwnerIdentity, PackageRef,
        RelationCompleteness, RelationKindCoverage, admit_graph_revision, relation_gap, retain_package,
    };
    use backend_library::SemanticLinkKind;
    use std::collections::BTreeMap;

    #[test]
    fn graph_read_batches_survive_the_real_service_frame_ceiling_and_reject_a_renewed_root() {
        // A Session::related call sends Revision + Related. The old single
        // connection reaches frame 257 on its 128th relation request, even
        // before package, dependency and outline reads are counted.
        let limit = backend_local_service::FrameLimits::default().max_frames_per_connection;
        assert_eq!(limit, 256);
        assert!(1 + 128 * 2 > limit);

        struct LimitedConnection { frames: usize }
        impl LimitedConnection {
            fn request(&mut self, frames: usize, ceiling: usize) {
                self.frames += frames;
                assert!(self.frames <= ceiling, "the service would close this connection");
            }
        }

        let root = backend_library::view_state_root(&[("graph".into(), "first".into())]);
        let renewed = backend_library::view_state_root(&[("graph".into(), "second".into())]);
        let authority = VersionedRoot::synthetic(root, 1).authority();
        let mut budget = GraphReadBudget::default();
        let mut connection = LimitedConnection { frames: 1 }; // first root proof
        let mut rotations = 0;
        // Worst supported projection: a preferred outline, package page,
        // every package dependency, eight outline pages each, and 128
        // neighborhoods. Failed optional reads cost an extra root check.
        let costs = [2, 2].into_iter()
            .chain(std::iter::repeat_n(2, 64))
            .chain(std::iter::repeat_n(2, 64 * 8))
            .chain(std::iter::repeat_n(3, 128));
        for cost in costs {
            if budget.next() {
                connection.request(1, limit); // departing root proof
                admit_graph_revision(authority, root, authority.cursor(), "between graph batches").expect("same root");
                connection = LimitedConnection { frames: 1 }; // new root proof
                rotations += 1;
            }
            connection.request(cost, limit);
        }
        connection.request(1, limit); // final root proof
        assert!(rotations > 1, "the projection crosses several real service leases");
        assert!(admit_graph_revision(authority, renewed, authority.cursor(), "after reconnecting the graph read").is_err(),
            "a renewed producer cannot contribute facts to a previous projection");
        assert!(admit_graph_revision(authority, root,
            backend_library::Cursor::at(root, authority.cursor().sequence() + 1),
            "after reconnecting the graph read").is_err(),
            "a renewed cursor at the same root is also a different producer authority");
    }

    #[test]
    fn private_owner_closes_an_exhausted_session_but_rooted_graph_batches_continue() {
        use backend_client::Session;
        use backend_local_service::{EmbeddedLocalService, ProcessConfig};
        use backend_runtime::WorkspacePaths;
        use std::path::PathBuf;

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let scratch = PathBuf::from(format!("/tmp/nx-graph-{}-{nonce}", std::process::id()));
        let endpoint = PathBuf::from(format!("/tmp/nx-graph-{}-{nonce}.sock", std::process::id()));
        crate::host::private_dir(&scratch).expect("private fixture workspace");
        crate::host::private_dir(&scratch.join("data")).expect("private owner state");
        let project = scratch.join("source");
        std::fs::create_dir_all(project.join("src")).expect("source directory");
        std::fs::write(project.join("Cargo.toml"),
            b"[package]\nname = \"graph-lease-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
            .expect("manifest");
        std::fs::write(project.join("src/lib.rs"), b"pub fn one() {}\n").expect("source");
        let paths = WorkspacePaths::discover(
            Some(project.clone()), Some(scratch.join("data")), Some(endpoint.clone()),
        ).expect("owner paths");
        paths.initialize().expect("initialize owner paths");
        let arguments = [
            "--endpoint", paths.endpoint().to_str().expect("endpoint utf-8"),
            "--workspace", paths.data().to_str().expect("workspace utf-8"),
            "--authority-secret-file", paths.authority_secret().to_str().expect("authority utf-8"),
            "--profile", "builtin",
        ].map(ToOwned::to_owned);
        let mut config = ProcessConfig::parse(arguments).expect("owner arguments");
        config.compiler_environment = crate::host::toolchain::supplied_by_the_process();
        let owner = EmbeddedLocalService::start(config).expect("private owner");

        let mut exhausted = Session::connect(&endpoint).expect("first connection");
        let first = exhausted.revision().expect("first root");
        let root = first.root;
        let authority = VersionedRoot::from_revision(1, first.cursor(), 0).authority();
        let ceiling = backend_local_service::FrameLimits::default().max_frames_per_connection;
        for _ in 1..ceiling {
            assert_eq!(exhausted.revision().expect("within the service frame limit").root, root);
        }
        assert!(exhausted.revision().is_err(), "the actual listener closes after 256 requests");

        let mut budget = GraphReadBudget::default();
        let mut session = Session::connect(&endpoint).expect("rooted graph connection");
        assert_eq!(session.revision().expect("root proof").root, root);
        let mut rotations = 0;
        for _ in 0..300 {
            if budget.next() {
                let proof = session.revision().expect("departing proof");
                admit_graph_revision(authority, proof.root, proof.cursor(),
                    "between graph batches").expect("unchanged owner");
                session = Session::connect(&endpoint).expect("rotated connection");
                let proof = session.revision().expect("new session proof");
                admit_graph_revision(authority, proof.root, proof.cursor(),
                    "after reconnecting the graph read").expect("unchanged owner");
                rotations += 1;
            }
            assert_eq!(session.revision().expect("bounded graph read").root, root);
        }
        assert!(rotations >= 4, "several authenticated sessions served the projection");

        let mut mutation = Session::connect(&endpoint).expect("mutation session");
        mutation.index(project.to_str().expect("project utf-8")).expect("index fixture");
        let renewed = mutation.revision().expect("renewed root").root;
        assert_ne!(root, renewed, "the owner published a different index root");
        let proof = session.revision().expect("post-index proof");
        assert!(admit_graph_revision(authority, proof.root, proof.cursor(),
            "while its graph was being read").is_err(),
            "a batch cannot combine rows from the earlier and later owner roots");

        drop(session);
        drop(mutation);
        drop(exhausted);
        owner.close().expect("close owner");
        std::fs::remove_dir_all(scratch).expect("remove private fixture");
    }

    #[test]
    fn bounded_package_selection_keeps_the_current_project_even_when_it_is_last() {
        let mut packages = Vec::new();
        let preferred = PackageRef::parse("pkg:cargo/project@1.0.0").expect("project release");
        for number in 0..(MAX_PACKAGES + 20) {
            let coordinate = format!("pkg:cargo/crate-{number:03}@1.0.0");
            let candidate = PackageRef::parse(&coordinate).expect("registry package");
            retain_package(&mut packages, candidate, Some(&preferred));
        }
        retain_package(&mut packages, preferred.clone(), Some(&preferred));

        assert_eq!(packages.len(), MAX_PACKAGES);
        assert_eq!(packages.first(), Some(&preferred));
        assert!(
            packages
                .windows(2)
                .all(|pair| { super::package_order(&pair[0], &pair[1], Some(&preferred)).is_lt() })
        );
    }

    #[test]
    fn canary_definition_reexport_and_missing_source_keep_their_own_facts() {
        use backend_library::{Basis, DeclarationKind, Row, RowId, SourceAvailability, SourceLocation, object_version, symbol_key, view_state_root};
        use facet::graph::model::{DeclarationRole, SourceStatus};
        let basis = Basis::new(view_state_root(&[]), object_version(b"two-file canary"));
        let definition = "/fixture/real-rust-canary::semantic::cadence-definition::advance_signal";
        let import = "/fixture/real-rust-canary::semantic::root-reexport::advance_signal";
        let row = |label: &str, kind, source| {
            let mut row = Row::new(RowId::Symbol(symbol_key(label)), basis, label);
            row.kind = Some(kind);
            row.source = source;
            row
        };
        let definition = row(definition, DeclarationKind::Function,
            SourceAvailability::Captured(SourceLocation::new("src/cadence.rs", 8).expect("source")));
        let import = row(import, DeclarationKind::Import,
            SourceAvailability::Captured(SourceLocation::new("src/lib.rs", 17).expect("source")));
        let project = |row: &Row, module| super::project_declaration(row,
            &crate::model::pages::SymbolRef::new(&row.label).expect("exact coordinate"), 0, module);
        let def = project(&definition, 1);
        let imp = project(&import, 0);
        assert_eq!(def.name, imp.name, "same spelling does not merge declarations");
        assert_eq!(def.declaration_role, DeclarationRole::Definition);
        assert_eq!(imp.declaration_role, DeclarationRole::Import);
        assert_eq!(def.source_words(), "src/cadence.rs:8");
        assert_eq!(imp.source_words(), "src/lib.rs:17");
        for source in [SourceAvailability::NotCaptured, SourceAvailability::NotHydrated,
            SourceAvailability::Unconfigured, SourceAvailability::stale_file("src/cadence.rs").expect("stale")] {
            let missing = project(&row("/fixture/real-rust-canary::semantic::missing::cadence", DeclarationKind::Module, source), 2);
            assert_ne!(missing.source_status, SourceStatus::Captured);
            assert_eq!(missing.declaration_role, DeclarationRole::Module);
            assert!(missing.file.is_none(), "another row's captured file cannot grant source capability");
            assert_eq!(missing.line, 0);
        }
    }

    #[test]
    fn relation_coverage_keeps_gaps_typed_and_completeness_unknown() {
        let mut coverage = Coverage {
            relations_by_kind: BTreeMap::from([(
                SemanticLinkKind::Calls,
                RelationKindCoverage::default(),
            )]),
            ..Coverage::default()
        };
        relation_gap(&mut coverage, SemanticLinkKind::Calls);

        let calls = coverage
            .relations_by_kind
            .get(&SemanticLinkKind::Calls)
            .expect("calls coverage");
        assert_eq!(calls.unavailable, 1);
        assert_eq!(calls.completeness, RelationCompleteness::Unknown);
        assert!(
            coverage
                .words(&Origin::IndexedOwner)
                .contains("calls 0 observed, 1 unavailable, completeness unknown")
        );
    }

    #[cfg(test)]
    #[test]
    fn projection_cache_uses_producer_authority_not_observation() {
        fn root(tag: &str, epoch: u64, generation: u64, observation: u64) -> VersionedRoot {
            let digest = backend_library::view_state_root(&[("graph".into(), tag.into())]);
            VersionedRoot::from_revision(
                epoch,
                backend_library::Cursor::at(digest, generation),
                observation,
            )
        }
        let captured = root("fixture-a", 3, 7, 1);
        let observed_again = root("fixture-a", 3, 7, 99);
        let changed_root = root("fixture-b", 3, 7, 99);
        let changed_epoch = root("fixture-a", 4, 7, 99);
        let changed_cursor = root("fixture-a", 3, 8, 99);
        let key = |root: VersionedRoot, id| Key {
            authority: root.authority(),
            root,
            owner: OwnerIdentity::Synthetic(id),
            preferred: None,
            synthetic: None,
        };

        let captured_key = key(captured, 1);
        assert_eq!(
            captured_key,
            key(observed_again, 1),
            "an observation-only update keeps the scene memo key"
        );
        let memo = crate::runtime::offload::Memo::new(
            std::num::NonZeroUsize::new(2).expect("capacity"),
            |_| 0usize,
        );
        memo.seed(captured_key.clone(), 17);
        assert_eq!(
            memo.peek(&key(observed_again, 1)).as_deref(),
            Some(&17),
            "the captured projection remains available to a new observation"
        );
        let mut cached = std::collections::HashSet::new();
        cached.insert(captured_key.clone());
        assert!(cached.contains(&key(observed_again, 1)), "Eq and Hash share authority semantics");
        assert_ne!(captured_key, key(captured, 2), "distinct test owners cannot share a projection");
        assert_ne!(captured_key, key(changed_root, 1), "a changed root cannot reuse a projection");
        assert_ne!(captured_key, key(changed_epoch, 1), "a changed producer epoch cannot reuse a projection");
        assert_ne!(captured_key, key(changed_cursor, 1), "a changed cursor cannot reuse a projection");
    }
}
