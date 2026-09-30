//! A bounded projection of the selected local index into the graph model.
//!
//! The graph is a view of one exact owner root. Package dependencies come from
//! dependency facts and declaration relations come from exact outline and
//! related reads. Every read is off the UI thread, and truncation or missing
//! relation authority remains visible in `Coverage`.

use crate::core::VersionedRoot;
use crate::model::pages::{PackageRef, SymbolRef};
use crate::runtime::offload::{Answer, Memo};
use crate::shell::bodies::graph::identity::{IdentityAdapter, ResolvedSymbol};
use backend_client::Session;
use backend_library::{
    CommandReply, DeclarationKind, DependencyFacts, PageContinuation, PageTerminal, RowId,
    SemanticLinkKind, SurfaceCommand, SurfaceReply,
};
use facet::graph::{Edge, Kind, Module, Node, Package, Rel, World};
use gpui::{Context, Global};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const WORLD_READS: usize = 4;
const MAX_PACKAGES: usize = 64;
const OUTLINE_PAGE: u16 = 128;
const OUTLINE_PAGES: usize = 8;
const MAX_DECLARATIONS: usize = 2_048;
const MAX_RELATION_ROOTS: usize = 128;
const MAX_RELATIONS: usize = 12_000;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct Key {
    root: VersionedRoot,
    endpoint: PathBuf,
    /// Keep the current project in the bounded projection when a workspace
    /// has more indexed packages than the graph can represent at once.
    preferred: Option<PackageRef>,
}

pub(crate) struct Projection {
    pub(crate) world: Arc<World>,
    pub(crate) scene: Arc<facet::graph::scene::Scene>,
    pub(crate) identities: Arc<IdentityAdapter>,
    pub(crate) coverage: Coverage,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Coverage {
    pub(crate) packages: usize,
    pub(crate) packages_total: usize,
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
    pub(crate) fn words(&self) -> String {
        let mut words = format!(
            "Indexed graph · {} packages · {} declarations · {} relations",
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
        if self.bounded || self.packages < self.packages_total {
            words.push_str(&format!(
                " · bounded from {} indexed packages",
                self.packages_total
            ));
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

impl Reads {
    fn new() -> Self {
        Self(Memo::new(
            NonZeroUsize::new(WORLD_READS).expect("positive capacity"),
            read,
        ))
    }
}

pub(crate) enum State {
    Reading,
    Ready(Arc<Projection>),
    Unavailable(Arc<str>),
}

pub(crate) fn key<T: 'static>(
    root: VersionedRoot,
    preferred: Option<PackageRef>,
    cx: &mut Context<T>,
) -> Option<Key> {
    let composition = crate::host::registry::composed()?;
    if cx.try_global::<Reads>().is_none() {
        cx.set_global(Reads::new());
    }
    Some(Key {
        root,
        endpoint: composition.endpoint,
        preferred,
    })
}

pub(crate) fn get<T: 'static>(key: &Key, cx: &mut Context<T>) -> State {
    if cx.try_global::<Reads>().is_none() {
        cx.set_global(Reads::new());
    }
    let memo = cx.global::<Reads>().0.clone();
    match memo.get(key, cx) {
        Answer::Reading => State::Reading,
        Answer::Failed(fault) => State::Unavailable(Arc::from(fault.to_string())),
        Answer::Ready(value) => match value.as_ref() {
            Ok(world) => State::Ready(Arc::clone(world)),
            Err(reason) => State::Unavailable(Arc::clone(reason)),
        },
    }
}

fn read(key: &Key) -> Result<Arc<Projection>, Arc<str>> {
    let composition = crate::host::registry::composed()
        .filter(|composition| composition.endpoint == key.endpoint)
        .ok_or_else(|| {
            Arc::from("the local service connection changed before the graph was read")
        })?;
    let mut session = Session::connect(&composition.endpoint).map_err(|error| {
        Arc::<str>::from(format!("could not connect to the local index: {error}"))
    })?;
    let before = session.revision().map_err(|error| {
        Arc::<str>::from(format!("could not read the graph's index root: {error}"))
    })?;
    if before.root != key.root.root() {
        return Err(Arc::from(
            "the local index changed before the graph read began",
        ));
    }
    let package_snapshot = match session
        .packages()
        .map_err(|error| Arc::<str>::from(format!("could not read indexed packages: {error}")))?
        .reply
    {
        CommandReply::Packages(snapshot) => snapshot,
        _ => {
            return Err(Arc::from(
                "the local service returned an unexpected package listing",
            ));
        }
    };
    let mut packages = Vec::with_capacity(MAX_PACKAGES);
    let mut packages_total = 0usize;
    for row in package_snapshot.root.rows() {
        if !matches!(row.id, RowId::Package(_)) {
            continue;
        }
        let Ok(package) = PackageRef::parse(&row.label) else {
            continue;
        };
        packages_total = packages_total.saturating_add(1);
        retain_package(&mut packages, package, key.preferred.as_ref());
    }
    if packages.is_empty() {
        return Err(Arc::from(
            "the current index has no packages to show in the graph",
        ));
    }
    let bounded = packages_total > packages.len();

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
        packages_total,
        bounded,
        ..Coverage::default()
    };
    for kind in SEMANTIC_LINK_KINDS {
        coverage
            .relations_by_kind
            .insert(kind, RelationKindCoverage::default());
    }
    for (index, package) in packages.iter().enumerate() {
        let reference = package.reference().clone();
        match session.surface(SurfaceCommand::Dependencies { package: reference }) {
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
            if nodes.len() >= MAX_DECLARATIONS {
                coverage.bounded = true;
                break 'packages;
            }
            let reply = match session.outline_page(package.as_str(), OUTLINE_PAGE, continuation) {
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
                let kind = graph_kind(row.kind.expect("checked"));
                let identity = symbol.identity();
                let mut node = Node::new(kind, identity.name(), package_id, module);
                node.line = line.unwrap_or(0);
                node.file = file.map(Into::into);
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
        match session.related(symbol.as_str()) {
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

    let after = session.revision().map_err(|error| {
        Arc::<str>::from(format!("could not confirm the graph's index root: {error}"))
    })?;
    if after.root != key.root.root() {
        return Err(Arc::from(
            "the local index changed while its graph was being read",
        ));
    }
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
    }))
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
        return "source unavailable".to_owned();
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
    use super::{
        Coverage, MAX_PACKAGES, PackageRef, RelationCompleteness, RelationKindCoverage,
        relation_gap, retain_package,
    };
    use backend_library::SemanticLinkKind;
    use std::collections::BTreeMap;

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
                .words()
                .contains("calls 0 observed, 1 unavailable, completeness unknown")
        );
    }
}
