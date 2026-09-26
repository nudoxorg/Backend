//! Indexes one admitted corpus for Trustfall neighbor and property resolution.

use super::*;

#[derive(Debug)]
struct SemanticGraph {
    corpus: SemanticQueryCorpus,
    all: Arc<[usize]>,
    projects: Arc<[usize]>,
    declarations: Arc<[usize]>,
    external_targets: Arc<[usize]>,
    by_id: BTreeMap<String, usize>,
    children: BTreeMap<String, Arc<[usize]>>,
    referenced_by: BTreeMap<String, Arc<[usize]>>,
    project_members: BTreeMap<PackageKey, Arc<[usize]>>,
}

impl SemanticGraph {
    fn new(corpus: SemanticQueryCorpus) -> Self {
        let mut projects = Vec::new();
        let mut declarations = Vec::new();
        let mut external_targets = Vec::new();
        let mut by_id = BTreeMap::new();
        let mut children = BTreeMap::<String, Vec<usize>>::new();
        let mut referenced_by = BTreeMap::<String, Vec<usize>>::new();
        let mut project_members = BTreeMap::<PackageKey, Vec<usize>>::new();
        for (index, fact) in corpus.facts().iter().enumerate() {
            let row = fact.presentation();
            by_id.insert(row.id.clone(), index);
            match fact.evidence() {
                SemanticQueryEvidence::Package(_) => projects.push(index),
                SemanticQueryEvidence::CompilerExternalTarget(_) => {
                    external_targets.push(index);
                }
                SemanticQueryEvidence::Compiler(_)
                | SemanticQueryEvidence::StructuralFallback(_) => declarations.push(index),
            }
            project_members
                .entry(fact.evidence().package())
                .or_default()
                .push(index);
            if let Some(parent) = &row.parent {
                children.entry(parent.clone()).or_default().push(index);
            } else if let Some(project) = &row.project {
                children.entry(project.clone()).or_default().push(index);
            }
            for target in &row.related {
                referenced_by.entry(target.clone()).or_default().push(index);
            }
        }
        Self {
            all: (0..corpus.facts().len()).collect::<Vec<_>>().into(),
            projects: projects.into(),
            declarations: declarations.into(),
            external_targets: external_targets.into(),
            by_id,
            children: children
                .into_iter()
                .map(|(key, values)| (key, values.into()))
                .collect(),
            referenced_by: referenced_by
                .into_iter()
                .map(|(key, values)| (key, values.into()))
                .collect(),
            project_members: project_members
                .into_iter()
                .map(|(key, values)| (key, values.into()))
                .collect(),
            corpus,
        }
    }
}

/// One corpus row addressed by the Trustfall adapter.
#[derive(Clone, Debug)]
pub(super) struct Vertex {
    graph: Arc<SemanticGraph>,
    index: usize,
}

impl Vertex {
    fn fact(&self) -> &SemanticQueryFact {
        &self.graph.corpus.facts()[self.index]
    }

    fn one(&self, id: Option<&str>) -> Arc<[usize]> {
        id.and_then(|id| self.graph.by_id.get(id).copied())
            .map_or_else(|| Arc::from([]), |index| Arc::from([index]))
    }

    fn neighbors(&self, edge: &str) -> Arc<[usize]> {
        let fact = self.fact();
        let row = fact.presentation();
        match edge {
            "project" => self.one(row.project.as_deref()),
            "parent" => self.one(row.parent.as_deref()),
            "children" => self
                .graph
                .children
                .get(&row.id)
                .cloned()
                .unwrap_or_else(|| Arc::from([])),
            "sameProject" => self
                .graph
                .project_members
                .get(&fact.evidence().package())
                .cloned()
                .unwrap_or_else(|| Arc::from([])),
            "related" => row
                .related
                .iter()
                .filter_map(|id| self.graph.by_id.get(id).copied())
                .collect::<Vec<_>>()
                .into(),
            "referencedBy" => self
                .graph
                .referenced_by
                .get(&row.id)
                .cloned()
                .unwrap_or_else(|| Arc::from([])),
            _ => Arc::from([]),
        }
    }

    fn stream(
        graph: Arc<SemanticGraph>,
        indices: Arc<[usize]>,
    ) -> AsyncNeighborStream<'static, Self> {
        let length = indices.len();
        Box::pin(stream::iter((0..length).map(move |at| Self {
            graph: Arc::clone(&graph),
            index: indices[at],
        })))
    }
}

impl Typename for Vertex {
    fn typename(&self) -> &'static str {
        "CodeItem"
    }
}

/// In-memory Trustfall adapter over one admitted semantic corpus.
#[derive(Debug)]
pub(super) struct SemanticAdapter {
    graph: Arc<SemanticGraph>,
}

impl SemanticAdapter {
    /// Builds the adapter index from one admitted corpus.
    pub(super) fn new(corpus: SemanticQueryCorpus) -> Self {
        Self {
            graph: Arc::new(SemanticGraph::new(corpus)),
        }
    }
}

impl AsyncBasicAdapter<'static> for SemanticAdapter {
    type Vertex = Vertex;

    fn resolve_starting_vertices(
        &self,
        edge_name: &str,
        _parameters: &EdgeParameters,
    ) -> AsyncNeighborStream<'static, Self::Vertex> {
        let indices = match edge_name {
            "Item" => Arc::clone(&self.graph.all),
            "Project" => Arc::clone(&self.graph.projects),
            "Declaration" => Arc::clone(&self.graph.declarations),
            "ExternalTarget" => Arc::clone(&self.graph.external_targets),
            _ => Arc::from([]),
        };
        Vertex::stream(Arc::clone(&self.graph), indices)
    }

    fn resolve_property<V: AsVertex<Self::Vertex> + 'static>(
        &self,
        contexts: AsyncContextStream<'static, V>,
        _type_name: &str,
        property_name: &str,
    ) -> AsyncContextOutcomeStream<'static, V, FieldValue> {
        match property_name {
            "id" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().presentation().id.as_str().into()
            }),
            "kind" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().presentation().kind.as_str().into()
            }),
            "coordinate" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().presentation().coordinate.as_str().into()
            }),
            "name" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().presentation().name.as_str().into()
            }),
            "signature" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex
                    .fact()
                    .presentation()
                    .signature
                    .as_deref()
                    .map_or(FieldValue::Null, Into::into)
            }),
            "documentation" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().presentation().documentation.as_str().into()
            }),
            "score" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex
                    .fact()
                    .presentation()
                    .score
                    .map_or(FieldValue::Null, |score| u64::from(score).into())
            }),
            "state" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex.fact().evidence().state().into()
            }),
            "revision" => resolve_property_with(contexts, |vertex: &Vertex| {
                encode_hex(vertex.graph.corpus.workspace().as_bytes()).into()
            }),
            "profile" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex
                    .fact()
                    .evidence()
                    .profile()
                    .map_or(FieldValue::Null, |profile| profile_name(profile).into())
            }),
            "packageUrl" => resolve_property_with(contexts, |vertex: &Vertex| {
                vertex
                    .fact()
                    .evidence()
                    .package_url()
                    .map_or(FieldValue::Null, Into::into)
            }),
            _ => resolve_property_with(contexts, |_vertex: &Vertex| FieldValue::Null),
        }
    }

    fn resolve_neighbors<V: AsVertex<Self::Vertex> + 'static>(
        &self,
        contexts: AsyncContextStream<'static, V>,
        _type_name: &str,
        edge_name: &str,
        _parameters: &EdgeParameters,
    ) -> AsyncContextOutcomeStream<'static, V, AsyncNeighborStream<'static, Self::Vertex>> {
        match edge_name {
            "project" | "parent" | "children" | "related" | "referencedBy" | "sameProject" => {
                let edge = edge_name.to_owned();
                resolve_neighbors_with(contexts, move |vertex: &Vertex| {
                    Vertex::stream(Arc::clone(&vertex.graph), vertex.neighbors(&edge))
                })
            }
            _ => resolve_neighbors_with(contexts, |_vertex: &Vertex| Box::pin(stream::empty())),
        }
    }

    fn resolve_coercion<V: AsVertex<Self::Vertex> + 'static>(
        &self,
        contexts: AsyncContextStream<'static, V>,
        _type_name: &str,
        coerce_to_type: &str,
    ) -> AsyncContextOutcomeStream<'static, V, bool> {
        let allowed = coerce_to_type == "CodeItem";
        resolve_coercion_with(contexts, move |_vertex: &Vertex| allowed)
    }
}

fn profile_name(profile: LanguageProfile) -> String {
    let [language, variant] = <[u8; 2]>::from(profile);
    format!("{language}:{variant}")
}
