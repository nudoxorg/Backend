//! Transactional retained recipe dependency graph and cycle admission.

use super::checked_add;
use super::facts::{DependencyFact, DependencyManifest, DependencyManifestVersion};
use crate::{RecipeVersion, SemanticError};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn has_recipe_cycle(edges: &BTreeMap<RecipeVersion, BTreeSet<RecipeVersion>>) -> bool {
    fn visit(
        node: RecipeVersion,
        edges: &BTreeMap<RecipeVersion, BTreeSet<RecipeVersion>>,
        active: &mut BTreeSet<RecipeVersion>,
        done: &mut BTreeSet<RecipeVersion>,
    ) -> bool {
        if active.contains(&node) {
            return true;
        }
        if done.contains(&node) {
            return false;
        }
        active.insert(node);
        if edges.get(&node).is_some_and(|dependencies| {
            dependencies
                .iter()
                .any(|dependency| visit(*dependency, edges, active, done))
        }) {
            return true;
        }
        active.remove(&node);
        done.insert(node);
        false
    }

    let mut active = BTreeSet::new();
    let mut done = BTreeSet::new();
    edges
        .keys()
        .any(|node| visit(*node, edges, &mut active, &mut done))
}

type RecipeEdge = (RecipeVersion, RecipeVersion);

#[derive(Clone, Debug, Eq, PartialEq)]
struct RetainedManifest {
    edges: BTreeSet<RecipeEdge>,
    users: usize,
}

/// Retained recipe dependency graph shared by all admitted manifests.
///
/// Each manifest registration is checked against the complete graph before
/// any edge, support count, or manifest record is published.  A failed
/// cross-manifest cycle therefore leaves the prior graph byte-for-byte
/// unchanged.  Repeated [`Self::retain_manifest`] calls share one graph
/// record and are released one reference at a time.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RetainedDependencyGraph {
    edges: BTreeMap<RecipeVersion, BTreeSet<RecipeVersion>>,
    edge_users: BTreeMap<RecipeEdge, usize>,
    manifests: BTreeMap<DependencyManifestVersion, RetainedManifest>,
    admission_counters: GraphAdmissionCounters,
}

/// Bounded work envelope for cross-manifest recipe-cycle admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GraphAdmissionBudget {
    /// Maximum dependency edges from the manifest checked in one attempt.
    pub max_edges: u64,
    /// Maximum graph nodes visited by one manifest admission.
    pub max_nodes: u64,
    /// Maximum outgoing edge probes by one manifest admission.
    pub max_edge_probes: u64,
}

impl Default for GraphAdmissionBudget {
    fn default() -> Self {
        Self {
            max_edges: 1_000_000,
            max_nodes: 1_000_000,
            max_edge_probes: 1_000_000,
        }
    }
}

/// Measured work from the most recent graph admission attempt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphAdmissionCounters {
    /// Number of candidate recipe edges checked.
    pub edges_checked: u64,
    /// Number of graph nodes visited by reachability checks.
    pub nodes_visited: u64,
    /// Number of outgoing dependency edges inspected.
    pub edge_probes: u64,
}

impl RetainedDependencyGraph {
    /// Compares graph projections while ignoring diagnostic admission work
    /// counters, whose value depends on the order in which a snapshot was
    /// hydrated rather than on its logical state.
    pub(super) fn same_state(&self, other: &Self) -> bool {
        self.edges == other.edges
            && self.edge_users == other.edge_users
            && self.manifests == other.manifests
    }

    /// Registers one manifest transactionally.
    ///
    /// # Errors
    ///
    /// Returns [`SemanticError::DuplicateManifest`] when the exact manifest
    /// is already registered, [`SemanticError::DependencyCycle`] when the
    /// new edges close a cycle with retained manifests, or
    /// [`SemanticError::Overflow`] when an edge support count is exhausted.
    pub fn register_manifest(
        &mut self,
        manifest: &DependencyManifest,
    ) -> Result<DependencyManifestVersion, SemanticError> {
        self.register_manifest_budgeted(manifest, GraphAdmissionBudget::default())
    }

    /// Registers one manifest using an explicit sparse-graph work envelope.
    ///
    /// Reachability starts at each new dependency and walks only the old
    /// graph plus the staged edges from this manifest. The full retained edge
    /// map is never cloned merely to test for a cycle.
    ///
    /// # Errors
    ///
    /// Returns the same admission errors as [`Self::register_manifest`], or
    /// [`SemanticError::ReuseWorkLimit`] when reachability exceeds `budget`.
    pub fn register_manifest_budgeted(
        &mut self,
        manifest: &DependencyManifest,
        budget: GraphAdmissionBudget,
    ) -> Result<DependencyManifestVersion, SemanticError> {
        let (version, manifest_edges, counters) =
            self.prepare_manifest_registration(manifest, budget)?;

        for edge @ (recipe, dependency) in &manifest_edges {
            let users = self.edge_users.entry(*edge).or_insert(0);
            *users += 1;
            self.edges.entry(*recipe).or_default().insert(*dependency);
        }
        self.manifests.insert(
            version,
            RetainedManifest {
                edges: manifest_edges,
                users: 1,
            },
        );
        self.admission_counters = counters;
        Ok(version)
    }

    /// Checks whether a manifest can become the next graph projection row.
    ///
    /// This is deliberately separate from mutation. A canonical relation
    /// owner can run the graph admission before preparing its persistent
    /// relation delta, then apply the already admitted graph projection only
    /// after that delta commits.
    pub(super) fn validate_retain_manifest(
        &self,
        manifest: &DependencyManifest,
    ) -> Result<(), SemanticError> {
        let version = manifest.version();
        if let Some(retained) = self.manifests.get(&version) {
            retained
                .users
                .checked_add(1)
                .map(|_| ())
                .ok_or(SemanticError::Overflow)
        } else {
            self.prepare_manifest_registration(manifest, GraphAdmissionBudget::default())
                .map(|_| ())
        }
    }

    fn prepare_manifest_registration(
        &self,
        manifest: &DependencyManifest,
        budget: GraphAdmissionBudget,
    ) -> Result<
        (
            DependencyManifestVersion,
            BTreeSet<RecipeEdge>,
            GraphAdmissionCounters,
        ),
        SemanticError,
    > {
        let version = manifest.version();
        if self.manifests.contains_key(&version) {
            return Err(SemanticError::DuplicateManifest);
        }
        let manifest_edges = manifest_recipe_edges(manifest);
        let mut staged = BTreeMap::<RecipeVersion, BTreeSet<RecipeVersion>>::new();
        let mut counters = GraphAdmissionCounters::default();
        for (recipe, dependency) in &manifest_edges {
            counters.edges_checked = checked_add(counters.edges_checked, 1)?;
            if counters.edges_checked > budget.max_edges {
                return Err(SemanticError::ReuseWorkLimit);
            }
            if self.reaches_with_staged(*dependency, *recipe, &staged, &mut counters, budget)? {
                return Err(SemanticError::DependencyCycle);
            }
            staged.entry(*recipe).or_default().insert(*dependency);
        }
        if manifest_edges.iter().any(|edge| {
            self.edge_users
                .get(edge)
                .is_some_and(|users| *users == usize::MAX)
        }) {
            return Err(SemanticError::Overflow);
        }
        Ok((version, manifest_edges, counters))
    }

    fn reaches_with_staged(
        &self,
        start: RecipeVersion,
        target: RecipeVersion,
        staged: &BTreeMap<RecipeVersion, BTreeSet<RecipeVersion>>,
        counters: &mut GraphAdmissionCounters,
        budget: GraphAdmissionBudget,
    ) -> Result<bool, SemanticError> {
        let mut seen = BTreeSet::new();
        let mut frontier = vec![start];
        while let Some(node) = frontier.pop() {
            if !seen.insert(node) {
                continue;
            }
            counters.nodes_visited = checked_add(counters.nodes_visited, 1)?;
            if counters.nodes_visited > budget.max_nodes {
                return Err(SemanticError::ReuseWorkLimit);
            }
            if node == target {
                return Ok(true);
            }
            for dependency in self
                .edges
                .get(&node)
                .into_iter()
                .flat_map(BTreeSet::iter)
                .chain(staged.get(&node).into_iter().flat_map(BTreeSet::iter))
            {
                counters.edge_probes = checked_add(counters.edge_probes, 1)?;
                if counters.edge_probes > budget.max_edge_probes {
                    return Err(SemanticError::ReuseWorkLimit);
                }
                frontier.push(*dependency);
            }
        }
        Ok(false)
    }

    /// Retains a manifest reference, sharing an existing graph record.
    ///
    /// # Errors
    ///
    /// Returns the same admission errors as [`Self::register_manifest`], or
    /// [`SemanticError::Overflow`] when a manifest reference count is
    /// exhausted.
    pub fn retain_manifest(
        &mut self,
        manifest: &DependencyManifest,
    ) -> Result<DependencyManifestVersion, SemanticError> {
        self.retain_manifest_references(manifest, 1)
    }

    /// Retains a bounded batch of references while admitting the manifest
    /// graph only once. This is used by relation hydration/parity checks so a
    /// large reference count does not turn a differential scan into one
    /// mutation per reference.
    pub(super) fn retain_manifest_references(
        &mut self,
        manifest: &DependencyManifest,
        references: u64,
    ) -> Result<DependencyManifestVersion, SemanticError> {
        if references == 0 {
            return Ok(manifest.version());
        }
        let version = manifest.version();
        let references = usize::try_from(references).map_err(|_| SemanticError::Overflow)?;
        if let Some(retained) = self.manifests.get_mut(&version) {
            retained.users = retained
                .users
                .checked_add(references)
                .ok_or(SemanticError::Overflow)?;
            return Ok(version);
        }
        let _ = self.register_manifest(manifest)?;
        if references > 1 {
            let retained = self
                .manifests
                .get_mut(&version)
                .ok_or(SemanticError::Overflow)?;
            retained.users = retained
                .users
                .checked_add(references - 1)
                .ok_or(SemanticError::Overflow)?;
        }
        Ok(version)
    }

    /// Releases one retained manifest reference.
    #[must_use]
    pub fn release_manifest(&mut self, version: DependencyManifestVersion) -> bool {
        let Some(users) = self.manifests.get(&version).map(|retained| retained.users) else {
            return false;
        };
        if users > 1 {
            if let Some(retained) = self.manifests.get_mut(&version) {
                retained.users -= 1;
            }
            return true;
        }
        let Some(retained) = self.manifests.remove(&version) else {
            return false;
        };
        for edge @ (recipe, dependency) in retained.edges {
            let remove_edge = if let Some(users) = self.edge_users.get_mut(&edge) {
                if *users > 1 {
                    *users -= 1;
                    false
                } else {
                    true
                }
            } else {
                false
            };
            if remove_edge {
                self.edge_users.remove(&edge);
                if let Some(dependencies) = self.edges.get_mut(&recipe) {
                    dependencies.remove(&dependency);
                    if dependencies.is_empty() {
                        self.edges.remove(&recipe);
                    }
                }
            }
        }
        true
    }

    /// Returns whether a release can be applied without changing graph
    /// admission state. The relation owner uses this as a precondition before
    /// committing the corresponding manifest-row delta.
    pub(super) fn can_release_manifest(&self, version: DependencyManifestVersion) -> bool {
        self.manifests.contains_key(&version)
    }

    /// Returns whether one recipe dependency is retained.
    #[must_use]
    pub fn depends_on(&self, recipe: RecipeVersion, dependency: RecipeVersion) -> bool {
        self.edges
            .get(&recipe)
            .is_some_and(|dependencies| dependencies.contains(&dependency))
    }

    /// Returns the number of distinct retained manifests.
    #[must_use]
    pub fn manifest_count(&self) -> usize {
        self.manifests.len()
    }

    /// Returns the number of references to one retained manifest.
    #[must_use]
    pub fn manifest_reference_count(&self, version: DependencyManifestVersion) -> usize {
        self.manifests
            .get(&version)
            .map_or(0, |manifest| manifest.users)
    }

    /// Returns the number of distinct retained recipe edges.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edge_users.len()
    }

    /// Returns work measured by the most recent admission attempt.
    #[must_use]
    pub const fn admission_counters(&self) -> GraphAdmissionCounters {
        self.admission_counters
    }
}

fn manifest_recipe_edges(manifest: &DependencyManifest) -> BTreeSet<RecipeEdge> {
    manifest
        .facts()
        .iter()
        .filter_map(|fact| match fact {
            DependencyFact::Recipe(edge) => Some((edge.recipe(), edge.dependency())),
            DependencyFact::Read(_) | DependencyFact::Authority(_) => None,
        })
        .collect()
}
